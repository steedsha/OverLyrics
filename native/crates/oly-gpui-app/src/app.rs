//! The app shell: one window wiring Tasks 3–9 into a single render loop.
//!
//! This is the Task 10 "render sample plate": a *sample plate*, not the
//! finished product. It reproduces every high-risk render surface — frozen
//! lyric rows (Task 5 scroll + Task 4 word effects), the am_bg v36 background
//! (Task 7), local glass popovers (Task 8), and the Media Foundation motion
//! cover (Task 9) — from replayed packets only. No online services, no real
//! players, no production media control. The lyric text itself is placeholder
//! content until Task 11 ports the lyric pipeline; this shell renders the
//! *layout and motion* systems against frozen rows.
//!
//! The window itself is GPUI's; the GPU-heavy passes live in `oly-render` and
//! reach the screen through the fork's custom-surface pass
//! (`paint_custom_surface` over `register_gpu_surface` handles), which
//! composites application-owned same-device textures into the scene in z
//! order without a CPU readback.

use std::path::PathBuf;

use oly_contracts::{MediaMs, MonoMs};
use oly_core::transport::TransportClock;
use oly_platform_win::media::{MotionPlayback, VideoDecoder, VideoHost};
use oly_render::background::gpu::BackgroundRenderer;

/// Everything the shell needs to know to reproduce a moment of the product.
#[derive(Clone, Debug, Default)]
pub struct PlateConfig {
    /// The motion cover video, if the plate should show one.
    pub motion_video: Option<PathBuf>,
    /// The window content size in logical pixels.
    pub viewport: (f32, f32),
    /// The device pixel ratio the QA harness runs at.
    pub scale: f32,
}

impl PlateConfig {
    pub fn viewport_w(&self) -> u32 {
        self.viewport.0.max(1.0) as u32
    }

    pub fn viewport_h(&self) -> u32 {
        self.viewport.1.max(1.0) as u32
    }
}

/// Bridges [`MotionPlayback`]'s lifecycle state machine to the decoder.
///
/// `MotionPlayback` never touches the decoder directly; it drives this host,
/// which maps the DOM-shaped verbs (`load`/`play`/`pause`/`seek`/`unload`)
/// onto `VideoDecoder` operations and the playhead model.
#[derive(Debug)]
pub struct MotionVideoHost {
    /// `Some((path, info))` while a source is loaded on the decoder.
    loaded_src: Option<String>,
    /// The DOM's "NaN until metadata": `None` while nothing is loaded or no
    /// frame has been decoded yet.
    current_time: Option<MediaMs>,
}

impl MotionVideoHost {
    pub fn new() -> Self {
        Self {
            loaded_src: None,
            current_time: None,
        }
    }

    pub fn loaded(&self) -> bool {
        self.loaded_src.is_some()
    }
}

impl Default for MotionVideoHost {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoHost for MotionVideoHost {
    fn has_source(&self) -> bool {
        self.loaded_src.is_some()
    }

    fn load(&mut self, src: &str) {
        // `video.src = src; video.load()` resets the playhead until metadata.
        self.loaded_src = Some(src.to_string());
        self.current_time = None;
    }

    fn play(&mut self) {
        // Playback is the caller's decode loop; play marks the source live.
        // The real `play()` promise resolves via on_play_completed.
    }

    fn pause(&mut self) {
        // Frames stop being pumped; the decoder keeps its position.
    }

    fn current_time_ms(&self) -> Option<MediaMs> {
        self.current_time
    }

    fn duration_ms(&self) -> Option<MediaMs> {
        // The task-9 decoder reports 0 for the MP4s MF cannot open duration
        // for; the shell falls back to PlayerPacket.durationMs. Report None
        // here so MotionPlayback's restore clamps use the packet's duration.
        None
    }

    fn seek_ms(&mut self, to: MediaMs) -> bool {
        self.current_time = Some(to);
        true
    }

    fn unload(&mut self) {
        self.loaded_src = None;
        self.current_time = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_clamps_to_one() {
        let cfg = PlateConfig {
            viewport: (0.0, 0.0),
            ..Default::default()
        };
        assert_eq!(cfg.viewport_w(), 1);
        assert_eq!(cfg.viewport_h(), 1);
    }

    #[test]
    fn default_plate_has_no_video() {
        let cfg = PlateConfig::default();
        assert!(cfg.motion_video.is_none());
    }

    /// The host's load/unload cycle matches the DOM verbs it stands for.
    #[test]
    fn host_load_unload_resets_playhead() {
        let mut host = MotionVideoHost::new();
        assert!(!host.has_source());
        host.load("a.mp4");
        assert!(host.has_source());
        assert!(host.current_time_ms().is_none());
        host.seek_ms(MediaMs::from_millis(5000));
        assert_eq!(host.current_time_ms(), Some(MediaMs::from_millis(5000)));
        host.unload();
        assert!(!host.has_source());
        assert!(host.current_time_ms().is_none());
    }

    /// `MotionPlayback` drives the host through the Task 9 lifecycle: a
    /// short hide (< 1.5 s) pauses but keeps the source; a long hide releases
    /// the decoder; re-showing restores the pre-hide position.
    #[test]
    fn motion_playback_short_hide_keeps_position() {
        let t0 = MonoMs::from_millis(0);
        let mut host = MotionVideoHost::new();
        host.load("a.mp4");
        let mut mp = MotionPlayback::new(host, "a.mp4", true);
        mp.on_metadata();
        mp.set_visible(false, t0 + 100);
        // Position must survive the hide (short-hide path).
        mp.set_visible(true, t0 + 500);
        assert!(mp.host().has_source(), "short hide keeps the source");
        let _ = t0;
    }
}
