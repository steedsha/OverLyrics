//! The GPU passes the player needs, on the window's own device.
//!
//! Three producers run here and all of them hand the renderer a texture rather
//! than pixels: the am_bg v36 immersion background, the motion cover's NV12
//! decode, and (later) the local glass panels. They share one `ID3D11Device`
//! with GPUI, so `register_gpu_surface` can composite them in scene order and
//! nothing is read back per frame.
//!
//! Decoding the static cover is the one CPU step: a PNG has to become an
//! `RgbaImage` before `prepare_artwork` can derive the field, and it happens once
//! per track rather than once per frame.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use oly_contracts::MediaMs;
use oly_platform_win::audio::{
    AudioMonitor, DEFAULT_FLASH_STRENGTH, DEFAULT_FLASH_THRESHOLD, DEFAULT_WOBBLE_STRENGTH,
    DEFAULT_WOBBLE_THRESHOLD,
};
use oly_platform_win::media::VideoDecoder;
use oly_render::background::artwork::{RgbaImage, prepare_artwork};
use oly_render::background::gpu::{BackgroundRenderer, Sampling};
use oly_render::background::lifecycle::BackgroundClock;
use oly_render::background::presentation::{WindowMode, immersion_scrim};
use oly_render::video_gpu::{VideoContext, VideoDevice, VideoRenderer, VideoTexture};

/// Where the motion cover's playhead sits relative to the song clock.
///
/// The reference ties the motion video to the track's own position; the frozen
/// fixture starts its clip 10 s in so the sample plate had visible movement on
/// its first frame. Keeping the same offset here means a fixture replay shows
/// the same frames as the plate did.
pub const MOTION_LEAD_MS: f64 = 10_000.0;

/// One decoded still cover, already prepared for the background pass.
pub struct Cover {
    pub width: u32,
    pub height: u32,
    pub rgba: RgbaImage,
}

/// Decode a PNG/JPEG cover from memory.
///
/// `image` is only pulled in for this; the frame path never touches it. A
/// failure is not fatal: the reference draws the generated placeholder and
/// keeps playing, so the caller falls back to the field colour.
const MAX_COVER_EDGE: u32 = 1024;

pub fn decode_cover(bytes: &[u8]) -> Result<Cover> {
    let decoded = image::load_from_memory(bytes).context("decoding the cover image")?;
    let (raw_w, raw_h) = (decoded.width(), decoded.height());
    let decoded = if raw_w > MAX_COVER_EDGE || raw_h > MAX_COVER_EDGE {
        decoded.resize(
            MAX_COVER_EDGE,
            MAX_COVER_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Cover {
        width,
        height,
        rgba: RgbaImage::new(width, height, rgba.into_raw()),
    })
}

pub fn decode_cover_file(path: &Path) -> Result<Cover> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading the cover at {}", path.display()))?;
    decode_cover(&bytes)
}

/// Runtime audio-beat reactivity controls for the immersion background.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatConfig {
    /// True only when an immersion surface is active in the main window.
    pub active: bool,
    /// User toggle: whether the immersion background follows audio drum beats.
    pub enabled: bool,
    pub flash_threshold: f32,
    pub flash_strength: f32,
    pub wobble_threshold: f32,
    pub wobble_strength: f32,
}

impl Default for BeatConfig {
    fn default() -> Self {
        Self {
            active: false,
            enabled: true,
            flash_threshold: DEFAULT_FLASH_THRESHOLD,
            flash_strength: DEFAULT_FLASH_STRENGTH,
            wobble_threshold: DEFAULT_WOBBLE_THRESHOLD,
            wobble_strength: DEFAULT_WOBBLE_STRENGTH,
        }
    }
}

/// Everything that draws into the window through a registered surface.
pub struct PlayerScene {
    device: VideoDevice,
    context: VideoContext,
    background: Option<BackgroundRenderer>,
    background_clock: BackgroundClock,
    audio: Option<AudioMonitor>,
    beat: BeatConfig,
    sim_speed: f64,
    breath_smooth: f32,
    last_frame_ms: Option<f64>,
    sim_clock_ms: Option<f64>,
    video: Option<MotionCover>,
    /// Sampling policy, fixed to the frozen profile's default.
    sampling: Sampling,
    /// Cover size, for the caller's layout.
    pub cover_size: (u32, u32),
}

struct MotionCover {
    decoder: VideoDecoder,
    renderer: VideoRenderer,
    path: PathBuf,
    playhead_ms: f64,
    looped: bool,
    last_now_ms: Option<f64>,
    frame_rendered: bool,
}

impl PlayerScene {
    /// Build the passes on the window's device. Returns `None` only when the
    /// window cannot hand out a device at all, which is a platform failure the
    /// caller reports rather than a panic.
    pub fn new(device: VideoDevice, context: VideoContext) -> Self {
        let background = BackgroundRenderer::from_device(device.clone(), context.clone()).ok();
        Self {
            device,
            context,
            background,
            background_clock: BackgroundClock::new(),
            audio: None,
            beat: BeatConfig::default(),
            sim_speed: 1.0,
            breath_smooth: 1.0,
            last_frame_ms: None,
            sim_clock_ms: None,
            video: None,
            // The window shape's own scrim: the reference samples the middle
            // band of the artwork and multiplies it by `immersionScrim('window')`
            // = 0.19 at every stop. Without it the field reads far brighter
            // than the reference frame.
            sampling: Sampling {
                sample_y: 0.21875,
                sample_height: 0.5625,
                scrim: immersion_scrim(WindowMode::Window),
                ..Sampling::default()
            },
            cover_size: (0, 0),
        }
    }

    pub fn set_beat(&mut self, beat: BeatConfig) {
        self.beat = beat;
        if beat.active && beat.enabled {
            let monitor = self.audio.get_or_insert_with(AudioMonitor::spawn);
            monitor.set_flash_threshold(beat.flash_threshold);
            monitor.set_wobble_threshold(beat.wobble_threshold);
        } else if self.audio.is_some() {
            self.audio = None;
        }
    }

    pub fn clear_cover(&mut self) {
        if let Some(background) = self.background.as_mut() { background.clear(); }
        self.cover_size = (0, 0);
    }

    pub fn has_background(&self) -> bool {
        self.background.as_ref().is_some_and(BackgroundRenderer::is_ready)
    }

    pub fn has_background_work(&self) -> bool {
        self.background.as_ref().is_some_and(BackgroundRenderer::has_work)
    }

    /// Install a decoded cover as the immersion background's artwork.
    pub fn set_cover(&mut self, cover: &Cover) -> Result<()> {
        self.cover_size = (cover.width, cover.height);
        if let Some(background) = self.background.as_mut() {
            let prepared = prepare_artwork(&cover.rgba);
            background.set_artwork(&prepared)?;
        }
        Ok(())
    }

    /// Resize the background's present target. The window's own size is the
    /// reference: the CSS backdrop is painted at the window's device pixels.
    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(background) = self.background.as_mut() {
            let _ = background.resize(width.max(1), height.max(1));
        }
    }

    /// Open a motion cover synced to `lyric_ms + MOTION_LEAD_MS`. Idempotent for the same path.
    pub fn open_motion(&mut self, path: &Path) -> Result<()> {
        self.open_motion_mode(path, false)
    }

    /// Open a motion cover that loops continuously from EOF back to `0 ms`.
    pub fn open_motion_looped(&mut self, path: &Path) -> Result<()> {
        self.open_motion_mode(path, true)
    }

    fn open_motion_mode(&mut self, path: &Path, looped: bool) -> Result<()> {
        if self
            .video
            .as_ref()
            .is_some_and(|v| v.path == path && v.looped == looped)
        {
            return Ok(());
        }
        self.video = None;
        let mut decoder = VideoDecoder::new(self.device.clone(), self.context.clone())?;
        let info = decoder.open(path)?;
        let space = oly_render::video::ColourSpace {
            matrix: info.colour.matrix,
            range: info.colour.range,
            transfer: info.colour.transfer.to_string(),
        };
        let renderer = VideoRenderer::new(
            &self.device,
            &self.context,
            info.width,
            info.height,
            &space,
        )?;
        self.video = Some(MotionCover {
            decoder,
            renderer,
            path: path.to_path_buf(),
            playhead_ms: if looped { 0.0 } else { -1.0 },
            looped,
            last_now_ms: None,
            frame_rendered: false,
        });
        Ok(())
    }

    pub fn has_motion(&self) -> bool {
        self.video.is_some()
    }

    /// True once at least one video frame has been decoded into the output texture,
    /// keeping the still cover visible until frame 0 is ready.
    pub fn has_motion_frame(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.frame_rendered)
    }

    /// The motion cover's output texture, for `register_gpu_surface`.
    pub fn motion_texture(&self) -> Option<VideoTexture> {
        self.video.as_ref().map(|v| v.renderer.output_texture())
    }

    /// The background's present texture, for `register_gpu_surface`.
    pub fn background_texture(&self) -> Option<&VideoTexture> {
        self.background.as_ref().map(|b| b.present_texture_handle())
    }

    /// The background's present-target size, so the view knows when to
    /// re-register the surface after a resize.
    pub fn background_size(&self) -> (u32, u32) {
        self.background
            .as_ref()
            .map(BackgroundRenderer::present_size)
            .unwrap_or((0, 0))
    }

    /// The motion cover's output size.
    pub fn motion_size(&self) -> (u32, u32) {
        self.video
            .as_ref()
            .map(|v| v.renderer.size())
            .unwrap_or((0, 0))
    }

    /// Advance every pass to `now_ms`.
    ///
    /// The video only re-decodes when the playhead actually moved: on a paused
    /// frame the texture from the previous frame is still correct, and pumping
    /// the decoder again would burn a decode per presented frame.
    pub fn tick(
        &mut self,
        now_ms: f64,
        lyric_ms: f64,
        playing: bool,
        render_background: bool,
    ) -> Result<()> {
        if render_background {
            if let Some(background) = self.background.as_mut() {
                let dt_ms = self
                    .last_frame_ms
                    .map_or(16.667, |prev| (now_ms - prev).clamp(0.0, 100.0));
                self.last_frame_ms = Some(now_ms);
                let sim_clock = self.sim_clock_ms.get_or_insert(now_ms);

                let is_paused = !playing;
                let (bass, beat_flash, beat_wobble) =
                    match (&self.audio, !is_paused && self.beat.enabled) {
                        (Some(monitor), true) => (
                            monitor.bass_level(),
                            monitor.beat_pulse().powf(1.10) * self.beat.flash_strength,
                            monitor.beat_wobble() * self.beat.wobble_strength,
                        ),
                        _ => (0.0, 0.0, 0.0),
                    };
                let target_speed = if is_paused {
                    0.18
                } else {
                    1.0 + (bass as f64) * 0.45
                };
                let target_breath = if is_paused {
                    0.92
                } else {
                    1.0 + bass * 0.06
                };
                let ease = (dt_ms / 240.0).clamp(0.02, 0.25);
                self.sim_speed += (target_speed - self.sim_speed) * ease;
                self.breath_smooth += (target_breath - self.breath_smooth) * (ease as f32);
                *sim_clock += dt_ms * self.sim_speed;

                let sampling = Sampling {
                    breath: self.breath_smooth,
                    beat_flash,
                    beat_wobble,
                    ..self.sampling
                };
                let times = self.background_clock.advance(*sim_clock);
                background.tick(&times, now_ms, sampling)?;
                // Flush initial warm-up bursts on the first cover so cover and
                // immersion background appear on the same frame.
                while !background.is_ready() && background.has_work() {
                    background.tick(&[], now_ms, sampling)?;
                }
            }
        } else {
            self.last_frame_ms = Some(now_ms);
        }
        if let Some(video) = self.video.as_mut() {
            let dt = video
                .last_now_ms
                .map(|prev| (now_ms - prev).clamp(0.0, 200.0))
                .unwrap_or(0.0);
            video.last_now_ms = Some(now_ms);
            let target = if video.looped {
                if playing {
                    video.playhead_ms = (video.playhead_ms.max(0.0) + dt).max(0.0);
                } else if video.playhead_ms < 0.0 {
                    video.playhead_ms = 0.0;
                }
                video.playhead_ms
            } else {
                lyric_ms + MOTION_LEAD_MS
            };
            let need_pump = if video.looped {
                playing || !video.frame_rendered
            } else {
                (video.playhead_ms - target).abs() >= 0.5 || playing
            };
            if need_pump {
                video
                    .decoder
                    .pump(MediaMs::from_millis(target.max(0.0) as i64))?;
                if video.looped && video.decoder.at_eof() && video.decoder.pending() == 0 {
                    video.playhead_ms = 0.0;
                    video.decoder.seek(MediaMs::from_millis(0))?;
                    video.decoder.pump(MediaMs::from_millis(0))?;
                }
                if let Some(sample) = video.decoder.front() {
                    video
                        .renderer
                        .render_nv12_texture(&self.context, &sample.texture)?;
                    video.frame_rendered = true;
                }
                if !video.looped {
                    video.playhead_ms = target;
                }
            }
        }
        Ok(())
    }

    /// Drop the motion cover's decoder when the track changes.
    pub fn clear_motion(&mut self) {
        if let Some(video) = self.video.as_mut() {
            video.decoder.release();
        }
        self.video = None;
    }
}

impl Drop for PlayerScene {
    fn drop(&mut self) {
        self.audio = None;
        self.clear_motion();
    }
}

#[cfg(test)]
mod background_animation_tests {
    use oly_render::background::{BackgroundClock, BackgroundScene};

    #[test]
    fn paused_song_background_advances_shader_time_at_the_same_rate_across_displays() {
        for hz in [30, 60, 144, 240] {
            let mut clock = BackgroundClock::new();
            let mut background = BackgroundScene::new();
            background.set_artwork(0.0);
            // Warm the artwork without advancing the song or animation clock.
            while !background.is_ready() {
                background.tick(&[], 0.0);
            }
            clock.advance(0.0);
            let mut rendered_times = Vec::new();
            for frame in 1..=hz * 2 {
                let now_ms = f64::from(frame) * 1000.0 / f64::from(hz);
                let steps = clock.advance(now_ms);
                rendered_times.extend(background.tick(&steps, now_ms).steps);
            }
            assert_eq!(rendered_times.len(), 120, "{hz} Hz display");
            assert!(rendered_times.windows(2).all(|pair| pair[1] > pair[0]));
            assert!((background.time() - 2.0).abs() < 1e-10);
        }
    }
}
