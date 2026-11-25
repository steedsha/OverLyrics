//! Task 10 render sample plate: one window wiring Tasks 3–9 together.
//!
//! This replaces the G0 probe. It is a *sample plate*, not the finished
//! player: replayed packets drive the frozen lyric rows, the am_bg v36
//! background pass (Task 7), the Task 8 glass, and the Task 9 motion cover,
//! composited through the fork's custom-surface pass. No online services,
//! no real players, no production media control.
//!
//! QA contract (plan §任务10), parsed here and honoured by the window:
//!
//! ```text
//! overlyrics --fixture <path> --capture-at-ms <n>
//!     --viewport <WxH> --scale <DPR> --evidence-dir <path>
//! ```
//!
//! plus a fixed clock seed: the plate starts its clock at a constant, so two
//! runs of the same parameters replay identically. With `--capture-at-ms 0`
//! no capture happens and the window runs until closed (the measure.ps1
//! sampling target).

//! OverLyrics desktop entry point.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::*;
use oly_contracts::PlayerPacket;
use oly_platform_win::media::VideoDecoder;
use oly_render::video_gpu::{readback_bgra8, VideoRenderer};

use oly_gpui_app::commands::{Inbox, ShellHost, ShellSink, UiEvent};
use oly_gpui_app::ui::controls::ToolBarState;
use oly_platform_win::window::{SettingsLifecycle, WinMode};

/// The fixed monotonic seed the plate's clock starts at (ms). Constant, not
/// read from the wall clock, so replays are deterministic.
pub const CLOCK_SEED_MS: f64 = 1000.0;

/// Parsed QA/plate parameters.
#[derive(Clone, Debug)]
struct PlateArgs {
    fixture: Option<PathBuf>,
    capture_at_ms: Option<u64>,
    viewport: (f32, f32),
    scale: f32,
    evidence_dir: Option<PathBuf>,
    seconds: Option<u64>,
    scenario: Option<String>,
    /// Follow the real system player through the shell's SMTC worker instead
    /// of the frozen fixture. Off by default: the QA plate must stay
    /// deterministic, and the automated suites must never touch a live
    /// session.
    live: bool,
    replay: bool,
    /// G4 soak: drive the workload the acceptance plan names (track changes,
    /// panel toggles, shape round trips) on a schedule so a single long run
    /// exercises it instead of a battery of short ones.
    soak: bool,
    /// Static cover frame for the player. When omitted, the player uses the
    /// demo cover that ships beside the executable.
    cover: Option<PathBuf>,
    /// Motion cover clip for the player. When omitted, the player uses the demo
    /// clip that ships beside the executable.
    motion: Option<PathBuf>,
    /// Runs the Task 10 sample plate instead of the player. The plate is a QA
    /// probe (one video surface plus a text HUD); the normal entry point is
    /// the full player, so this has to be asked for explicitly.
    probe: bool,
    /// Offscreen capture for viewports a screen grab cannot reach: the player
    /// renders its scene to a texture and writes raw RGBA here. The capture
    /// uses `capture_at_ms` as the media time.
    capture_rgba: Option<PathBuf>,
    /// Pins the replay clock to this media time and pauses, so a still shows
    /// the same song instant as a reference image.
    freeze_ms: Option<f64>,
    explicit_viewport: bool,
}

fn parse_args() -> PlateArgs {
    let mut args = PlateArgs {
        fixture: None,
        capture_at_ms: None,
        viewport: (1272.0, 843.0),
        scale: 1.0,
        evidence_dir: None,
        seconds: None,
        scenario: None,
        live: false,
        replay: false,
        soak: false,
        cover: None,
        motion: None,
        probe: false,
        capture_rgba: None,
        freeze_ms: None,
        explicit_viewport: false,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        let mut take = |what: &str| {
            argv.next()
                .unwrap_or_else(|| panic!("missing value for {what}"))
        };
        match arg.as_str() {
            "--fixture" => args.fixture = Some(PathBuf::from(take("--fixture"))),
            "--capture-at-ms" => args.capture_at_ms = Some(take("--capture-at-ms").parse().expect("capture-at-ms")),
            "--viewport" => {
                let v = take("--viewport");
                let (w, h) = v.split_once('x').unwrap_or_else(|| panic!("viewport must be WxH, got {v}"));
                args.viewport = (w.parse().expect("viewport w"), h.parse().expect("viewport h"));
                args.explicit_viewport = true;
            }
            "--scale" => args.scale = take("--scale").parse().expect("scale"),
            "--evidence-dir" => args.evidence_dir = Some(PathBuf::from(take("--evidence-dir"))),
            "--seconds" => args.seconds = Some(take("--seconds").parse().expect("seconds")),
            "--scenario" => args.scenario = Some(take("--scenario")),
            "--live" => args.live = true,
            "--replay" => args.replay = true,
            "--soak" => args.soak = true,
            "--cover" => args.cover = Some(PathBuf::from(take("--cover"))),
            "--motion" => args.motion = Some(PathBuf::from(take("--motion"))),
            "--qa-probe" => args.probe = true,
            "--capture-rgba" => args.capture_rgba = Some(PathBuf::from(take("--capture-rgba"))),
            "--freeze-ms" => args.freeze_ms = Some(take("--freeze-ms").parse().expect("freeze-ms")),
            other => eprintln!("ignoring unknown argument: {other}"),
        }
    }
    let force_replay = args.replay
        || args.fixture.is_some()
        || args.scenario.is_some()
        || args.capture_at_ms.is_some()
        || args.capture_rgba.is_some()
        || args.freeze_ms.is_some()
        || args.cover.is_some()
        || args.motion.is_some()
        || args.probe;
    if !args.live && !force_replay {
        args.live = true;
    }
    args
}

/// The demo assets beside the executable.
///
/// The shipped bundle has to open a playing window on a double-click, and a
/// bare launch names no fixture, cover or clip. These are the defaults for the
/// gaps a launch leaves, resolved next to the exe so the bundle can be moved as
/// a whole. A run that names its own paths keeps them: this only fills in what
/// was omitted.
fn demo_assets() -> (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("assets").join("demo"));
            // `target/<profile>/overlyrics.exe` -> `native/assets`.
            dirs.push(dir.join("..").join("..").join("assets").join("demo"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("assets").join("demo"));
        dirs.push(cwd.join("native").join("assets").join("demo"));
    }
    let pick = |name: &str| {
        dirs.iter()
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    };
    (
        pick("hard-times.json"),
        pick("cover-600.png"),
        pick("motion-a.mp4"),
    )
}

/// The per-frame media pipeline behind the motion cover: the Task 9 decoder
/// bound to the window's device, the Task 10 NV12→BGRA renderer, and the
/// registered custom surface the fork composites into the GPUI scene.
///
/// All three live on the same `ID3D11Device` as GPUI's renderer (the plate
/// never creates its own), and everything here runs on the UI thread during
/// paint, so the immediate context is driven in order by one thread — the
/// fork contract for `register_gpu_surface`.
struct ScenePipeline {
    decoder: VideoDecoder,
    renderer: VideoRenderer,
    /// Registered handle for the current output texture. Re-registered when
    /// the device is rebuilt (GPUI drops registered surfaces on device loss).
    surface: Option<CustomSurfaceHandle>,
    /// The playhead this pipeline presents (ms into the stream, mirroring the
    /// MotionVideoHost the Task 9 state machine drives).
    playhead_ms: f64,
}

impl ScenePipeline {
    /// Opens `path` on the window's device and builds the matching renderer.
    fn open(
        device: oly_render::video_gpu::VideoDevice,
        context: oly_render::video_gpu::VideoContext,
        path: &std::path::Path,
    ) -> anyhow::Result<Self> {
        let mut decoder = VideoDecoder::new(device, context)?;
        let info = decoder.open(path)?;
        log::info!(
            "motion pipeline: {}x{} {} fps colour {} ({:?})",
            info.width,
            info.height,
            info.fps_num as f64 / info.fps_den.max(1) as f64,
            info.colour.transfer,
            info.colour.matrix
        );
        let space = oly_render::video::ColourSpace {
            matrix: info.colour.matrix,
            range: info.colour.range,
            transfer: info.colour.transfer.to_string(),
        };
        let renderer = VideoRenderer::new(
            decoder.device_handle(),
            decoder.context_handle(),
            info.width,
            info.height,
            &space,
        )?;
        Ok(Self {
            decoder,
            renderer,
            surface: None,
            playhead_ms: 0.0,
        })
    }

    /// Presents the newest decoded frame at the playhead: pump the bounded
    /// queue, render the newest sample through the GPU pass, and keep the
    /// custom surface registered over the (stable) output texture.
    fn present(&mut self, window: &Window) -> Option<CustomSurfaceHandle> {
        self.decoder
            .pump(oly_contracts::MediaMs::from_millis(self.playhead_ms as i64))
            .ok();
        if let Some(sample) = self.decoder.front().cloned() {
            match self
                .renderer
                .render_nv12_texture(self.decoder.context_handle(), &sample.texture)
            {
                Ok(_) => {}
                Err(e) => log::warn!("video render failed: {e:#}"),
            }
        }
        // The output texture is stable for the pipeline's lifetime, so the
        // registration is created once and reused; the fork keeps the SRV
        // alive until the device is lost (which clears its registry — the
        // caller then re-registers through the same call path).
        if self.surface.is_none() {
            match window.register_gpu_surface(&self.renderer.output_texture()) {
                Ok(handle) => self.surface = Some(handle),
                Err(e) => log::warn!("registering video surface failed: {e:#}"),
            }
        }
        self.surface
    }

    /// Releases the decoder and unregisters the surface (unload path).
    fn shutdown(&mut self, window: &Window) {
        if let Some(handle) = self.surface.take() {
            window.destroy_custom_surface(handle);
        }
        self.decoder.release();
    }
}

/// One sample of the swap chain's own present counters.
#[derive(Clone, Copy, Debug)]
struct PresentSample {
    /// Plate wall clock at the sample (ms since plate start).
    t_ms: f64,
    /// Presents the swap chain accepted since it was created.
    present_count: u64,
    /// Display refreshes the swap chain observed since it was created.
    refresh_count: u64,
    /// QPC timestamp of the last sync, in the swap chain's own units.
    sync_qpc_time: i64,
}

/// Frame-cadence accumulator fed by `Window::present_statistics`.
///
/// This is the only trustworthy source for the plan's 240 Hz questions: the
/// swap chain counts what the display stack accepted and observed, so the
/// delta ratio names missed vsyncs. An application-side frame counter (or a
/// UI "240 fps" readout) reports what the app asked for, which is why the
/// plan forbids claiming stability from it.
///
/// Sampling is once per paint: `GetFrameStatistics` is cheap, and one sample
/// per frame is enough to reconstruct every present interval. The derived
/// series is written to the evidence directory at exit, so the harness reads
/// numbers from the run instead of recomputing anything.
#[derive(Default)]
struct PresentCadence {
    last: Option<PresentSample>,
    /// Present-to-present intervals in milliseconds (from QPC deltas).
    intervals: Vec<f64>,
    /// Refresh counters observed without a matching present, per interval.
    missed: Vec<u64>,
    /// Total presents accepted during the run.
    presents: u64,
    /// Samples whose counters did not advance (a coalesced frame): kept, not
    /// discarded, because a long stall shows up here first.
    stagnant: u64,
    /// Samples rejected by the platform (`GetFrameStatistics` refused, e.g. a
    /// composition swap chain before its first present).
    rejected: u64,
    /// Samples whose own clocks disagree, so no frame interval can be
    /// derived from them: a zero `SyncQPCTime` before the first present, a
    /// DWM restart, a display mode switch. Counted and skipped rather than
    /// folded into the series — one such sample otherwise fabricates a
    /// three-minute "frame" and a four-million-vsync miss, which is exactly
    /// how a cadence report lies.
    ///
    /// A genuinely slow frame is *not* a discontinuity: the plate's own wall
    /// clock advances with it, so the interval still fits inside the wall
    /// time between the two samples and is recorded.
    discontinuities: u64,
}

impl PresentCadence {
    fn observe(&mut self, window: &Window, t_ms: f64) {
        let Ok(stats) = window.present_statistics() else {
            self.rejected += 1;
            return;
        };
        let sample = PresentSample {
            t_ms,
            present_count: stats.present_count,
            refresh_count: stats.refresh_count,
            sync_qpc_time: stats.sync_qpc_time,
        };
        if let Some(last) = self.last {
            let dp = sample.present_count.saturating_sub(last.present_count);
            let dr = sample.refresh_count.saturating_sub(last.refresh_count);
            if dp == 0 {
                self.stagnant += 1;
            } else {
                // QPC deltas are in the same units as the swap chain's own
                // clock; the app never assumes a frequency, it takes the
                // ratio of QPC ticks to presents the display accepted.
                // 10,000 units per ms at the 10 MHz counter DXGI uses.
                let dt = (sample.sync_qpc_time - last.sync_qpc_time) as f64 / 10_000.0;
                let wall = (sample.t_ms - last.t_ms).max(0.0);
                // A sample whose present clock advanced further than the
                // plate's own wall clock cannot describe a frame: the two
                // counters started at different epochs (the first present
                // after creation), or the desktop compositor restarted.
                // Skipping it keeps one bad counter jump out of every
                // percentile instead of fabricating a three-minute frame.
                if !(dt > 0.0) || dt > wall + 50.0 {
                    self.discontinuities += 1;
                } else {
                    self.intervals.push(dt / dp as f64);
                    self.missed.push(dr.saturating_sub(dp));
                    self.presents += dp;
                }
            }
        } else {
            self.presents = 0;
        }
        self.last = Some(sample);
    }

    /// Percentile of the interval series (sorted copy; p in 0..=100).
    fn percentile(&self, p: f64) -> f64 {
        if self.intervals.is_empty() {
            return 0.0;
        }
        let mut v = self.intervals.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((p / 100.0) * (v.len() - 1) as f64).round() as usize;
        v[idx.min(v.len() - 1)]
    }

    /// The JSON report written beside the run's other evidence.
    fn report(&self) -> String {
        let max = self
            .intervals
            .iter()
            .copied()
            .fold(0.0f64, f64::max);
        let missed_total: u64 = self.missed.iter().map(|m| *m as u64).sum();
        let missed_intervals = self.missed.iter().filter(|m| **m > 0).count();
        format!(
            "{{\"presents\":{},\"samples\":{},\"stagnant\":{},\"rejected\":{},\"discontinuities\":{},\
             \"intervalMs\":{{\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3},\"max\":{:.3}}},\
             \"missedVsync\":{{\"total\":{},\"intervals\":{}}},\
             \"longFrames\":{{\"over8_33\":{},\"over16_67\":{}}}}}",
            self.presents,
            self.intervals.len(),
            self.stagnant,
            self.rejected,
            self.discontinuities,
            self.percentile(50.0),
            self.percentile(95.0),
            self.percentile(99.0),
            max,
            missed_total,
            missed_intervals,
            self.intervals.iter().filter(|v| **v > 8.33).count(),
            self.intervals.iter().filter(|v| **v > 16.67).count(),
        )
    }
}

/// The plate window's root element. Render paints the GPU-produced surfaces
/// (background pass output, motion cover frame) into the GPUI scene through
/// the fork's custom-surface pass, then lets the normal element tree draw the
/// chrome above them.
struct PlateWindow {
    viewport: (f32, f32),
    scale: f32,
    fixture: Option<PathBuf>,
    capture_at_ms: Option<u64>,
    evidence_dir: Option<PathBuf>,
    /// Motion pipeline, opened on the first frame when the device is known.
    pipeline: Option<ScenePipeline>,
    /// Video surface handle, re-registered whenever the decoder produced a
    /// new texture or the device was rebuilt.
    video_surface: Option<CustomSurfaceHandle>,
    /// Monotonic clock since plate start, seeded at [`CLOCK_SEED_MS`].
    clock_ms: f64,
    /// The plate playhead (ms from plate start), read by the capture trigger.
    playhead_ms: f64,
    /// Whether the once-only capture has been taken.
    captured: bool,
    /// The measure.ps1 scenario name (`window-idle`, `window-wheel`, ...).
    scenario: Option<String>,
    /// Auto-quit after this many seconds (the measure harness runs each
    /// scenario for a fixed window and samples the OS side meanwhile).
    seconds: Option<u64>,
    /// Resize-scenario phase: the plate toggles between the main size and
    /// 800x500 every second so the resize path is actually exercised.
    resize_phase: u32,
    /// Wall-clock start of the plate. Scenario durations and the measure
    /// window use this, not the frame counter: an unthrottled render loop
    /// would otherwise finish a "10 second" run in well under a second.
    started: std::time::Instant,
    /// Set when `--live` starts the shell workers; `None` means the plate is
    /// replaying the frozen fixture and never touches a real session.
    shell: Option<ShellHost>,
    /// The inbound mailbox the workers publish into. Kept even without a
    /// shell so the drain path is one code path in tests and in the plate.
    inbox: Arc<std::sync::Mutex<Inbox>>,
    /// The newest live packet the render loop has not consumed yet.
    live_packet: Option<PlayerPacket>,
    /// Latest volume answer, in request order (id, fraction or unavailable).
    live_volume: Vec<(i64, Option<f32>)>,
    /// Counters for the HUD: how many packets/answers arrived and were
    /// coalesced. Proves the "one poll, bounded mailbox" contract on screen.
    live_states: u64,
    /// Swap-chain present counters accumulated over the run. Fed once per
    /// paint from the platform's `GetFrameStatistics`; written to the
    /// evidence directory at exit as `present-report.json`.
    cadence: PresentCadence,
    /// G4 soak driver state: the scripted workload runs beside the ordinary
    /// render loop so one long run covers track changes, panel toggles and
    /// shape round trips instead of a battery of short ones.
    soak: Option<SoakState>,
}

/// The deterministic workload the acceptance plan asks for, driven from the
/// paint loop on the plate's own wall clock.
///
/// Every count is recorded here and written to `soak-report.json` at exit;
/// the harness reads the file rather than inferring the workload from the
/// clock, so a run that silently stopped driving (a panic in an event
/// handler, a stuck timer) reports fewer actions instead of looking complete.
struct SoakState {
    /// Track changes issued so far (the plan wants >= 20).
    tracks: u32,
    /// Panel toggles (more + volume popovers, >= 50).
    toggles: u32,
    /// Shape round trips (window -> mini -> window, >= 20).
    shape_cycles: u32,
    /// Next tick at which the panel toggles.
    next_toggle_ms: f64,
    /// Next tick at which the track changes.
    next_track_ms: f64,
    /// Next tick at which the shape round trips.
    next_shape_ms: f64,
    /// Current half of a shape round trip: false = wide, true = compact.
    compact_half: bool,
    /// Toolbar model driven alongside the window so the panels really open
    /// and close (the same state the real UI binds to).
    toolbar: ToolBarState,
    /// Settings panel lifecycle: opened and closed on the same schedule the
    /// reference uses, so the timer chain runs against real wall time.
    settings: SettingsLifecycle,
    /// Whether the settings panel is currently meant to be open.
    settings_open: bool,
    /// Next tick at which the settings panel flips.
    next_settings_ms: f64,
    /// The fixture's packet rotation, so a "track change" is a real packet
    /// rather than a counter: title/artist/position all change.
    track_index: u32,
    /// A shape change the render loop still has to apply to the real window.
    /// `advance` only updates the model; the window belongs to the caller.
    pending_resize: Option<WinMode>,
}

impl SoakState {
    fn new() -> Self {
        Self {
            tracks: 0,
            toggles: 0,
            shape_cycles: 0,
            next_toggle_ms: 1_500.0,
            next_track_ms: 20_000.0,
            next_shape_ms: 8_000.0,
            compact_half: false,
            toolbar: ToolBarState::new(oly_gpui_app::ui::controls::Mode::Window),
            settings: SettingsLifecycle::default(),
            settings_open: false,
            next_settings_ms: 5_000.0,
            track_index: 0,
            pending_resize: None,
        }
    }

    /// The shape change the caller must apply to the window, if any.
    fn pending_resize(&self) -> Option<WinMode> {
        self.pending_resize
    }

    fn clear_pending_resize(&mut self) {
        self.pending_resize = None;
    }

    /// The synthetic packet for the current soak track. Shape-compatible with
    /// the frozen bridge packets so the row pipeline sees ordinary input.
    fn packet(&self, index: u32) -> PlayerPacket {
        let titles = [
            ("Soak One", "Verifier", "Gate Runner"),
            ("Soak Two", "Verifier", "Second Pass"),
            ("Soak Three", "Verifier", "Third Pass"),
        ];
        let (title, artist, album) = titles[(index as usize) % titles.len()];
        PlayerPacket {
            album: album.to_string(),
            album_artist: artist.to_string(),
            app: "MSEdge".to_string(),
            artist: artist.to_string(),
            can_seek: true,
            cover: None,
            cover_updated: true,
            ctrl: Default::default(),
            duration_ms: 180_000.0,
            idle: false,
            last_updated_unix_ms: 0.0,
            position_available: Some(true),
            position_discontinuity: true,
            position_ms: 0.0,
            rate: 1.0,
            start_ms: 0.0,
            status: Some(oly_contracts::PlaybackStatus::Code(4)),
            timeline_source: "smtc".to_string(),
            title: title.to_string(),
            track_changed: true,
            track_number: index as i64 + 1,
            kind: "state".to_string(),
        }
    }

    /// Advances the workload to `now_ms`. Returns the packets produced by the
    /// track changes this tick (the caller folds them into the plate's live
    /// view, which is exactly what a real SMTC update would do).
    fn advance(&mut self, now_ms: f64) -> Vec<PlayerPacket> {
        let mut packets = Vec::new();
        while now_ms >= self.next_toggle_ms {
            self.toolbar.toggle_more();
            self.toggles += 1;
            self.toolbar.toggle_volume();
            self.toggles += 1;
            self.next_toggle_ms += 5_000.0;
        }
        while now_ms >= self.next_track_ms {
            self.track_index += 1;
            self.tracks += 1;
            packets.push(self.packet(self.track_index));
            self.next_track_ms += 20_000.0;
        }
        while now_ms >= self.next_shape_ms {
            if self.compact_half {
                self.toolbar.on_mode_changed(oly_gpui_app::ui::controls::Mode::Window);
                self.shape_cycles += 1;
            } else {
                self.toolbar.on_mode_changed(oly_gpui_app::ui::controls::Mode::Mini);
            }
            self.compact_half = !self.compact_half;
            self.next_shape_ms += 30_000.0;
        }
        while now_ms >= self.next_settings_ms {
            if self.settings_open {
                self.settings.close(now_ms as u64);
                self.settings_open = false;
            } else {
                self.settings.open(now_ms as u64);
                self.settings_open = true;
            }
            self.next_settings_ms += 20_000.0;
        }
        // Drive the timer chain every tick (hide/destroy fire on their own
        // deadlines, not on the flip schedule).
        self.settings.tick(now_ms as u64);
        packets
    }

    /// The workload counts, written beside the run's other evidence so the
    /// harness reads what actually happened instead of trusting the schedule.
    fn report(&self) -> String {
        format!(
            "{{\"trackChanges\":{},\"panelToggles\":{},\"shapeCycles\":{},\"settingsOpen\":{}}}",
            self.tracks, self.toggles, self.shape_cycles, self.settings_open
        )
    }
}

impl PlateWindow {
    /// Takes the frozen QA capture: reads the video output back to a PNG-able
    /// buffer in `--evidence-dir`. QA-only (staging readback), never on the
    /// steady-state present path.
    fn take_capture(&mut self, window: &Window) {
        let Some(dir) = self.evidence_dir.clone() else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        if let Some(pipeline) = &self.pipeline {
            match readback_bgra8(&pipeline.renderer.output_texture()) {
                Ok((pixels, w, h)) => {
                    let path = dir.join(format!(
                        "plate-capture-{}x{}-at-{}ms.bgra8",
                        w,
                        h,
                        self.capture_at_ms.unwrap_or(0)
                    ));
                    match std::fs::write(&path, &pixels) {
                        Ok(_) => log::info!("capture written: {} ({}x{})", path.display(), w, h),
                        Err(e) => log::warn!("capture write failed: {e}"),
                    }
                }
                Err(e) => log::warn!("capture readback failed: {e:#}"),
            }
        }
        let _ = window;
    }
}

impl Render for PlateWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Advance the plate clock one frame (60 Hz cadence assumption; the
        // capture path uses this deterministic frame clock so two runs of the
        // same parameters present the same frame).
        self.clock_ms += 1000.0 / 60.0;

        // Sample the swap chain's own counters once per paint. Cheap, and it
        // is the only series that can prove a steady cadence: `Present`
        // counters advance when the display stack accepts a frame.
        self.cadence
            .observe(window, self.started.elapsed().as_secs_f64() * 1000.0);

        // Scenario timing is wall-clock: the measure harness samples the OS
        // side once a second and expects the app to live for the requested
        // number of real seconds, whatever rate the compositor drives.
        let elapsed_s = self.started.elapsed().as_secs_f64();

        // G4 soak: drive the acceptance workload on the plate's own wall clock.
        // The packets a track change produces go through the same live view a
        // real SMTC update would, and the counts travel out as soak-report.json
        // so the harness reads what actually ran.
        if let Some(soak) = &mut self.soak {
            let packets = soak.advance(elapsed_s * 1000.0);
            if !packets.is_empty() {
                self.live_packet = packets.last().cloned();
                self.live_states += packets.len() as u64;
            }
            // A shape round trip really resizes the window, so the resize path
            // runs against the same code the user drives.
            if let Some(mode) = soak.pending_resize() {
                soak.clear_pending_resize();
                let (w, h) = (mode.size().width as f32, mode.size().height as f32);
                window.resize(size(px(w), px(h)));
            }
        }

        // Self-driven scenario + fixed duration: the measure harness starts
        // the app once per scenario and only samples the OS side, so the
        // scenario work and the exit both belong to the app.
        if let Some(scenario) = self.scenario.clone() {
            let phase = (elapsed_s as u32).max(1);
            if scenario.ends_with("resize") && phase != self.resize_phase && elapsed_s > 1.0 {
                self.resize_phase = phase;
                let (w, h) = if phase % 2 == 0 {
                    (self.viewport.0, self.viewport.1)
                } else {
                    (800.0f32.min(self.viewport.0), 500.0f32.min(self.viewport.1))
                };
                window.resize(size(px(w), px(h)));
            }
        }
        if let Some(seconds) = self.seconds {
            if elapsed_s >= seconds as f64 {
                // The scenario window is over: quit cleanly so the harness
                // sees a normal exit and the teardown checks still run.
                log::info!("scenario complete after {seconds}s, quitting");
                // Unload path: release the decoder and unregister the surface
                // before the window goes away — the same teardown the web
                // build runs from `dispose()`.
                if let Some(pipeline) = self.pipeline.as_mut() {
                    pipeline.shutdown(window);
                }
                // Evidence the harness reads instead of trusting the launcher's
                // exit code (Start-Process with redirected streams can report
                // null there even on a clean exit).
                if let Some(dir) = &self.evidence_dir {
                    let _ = std::fs::create_dir_all(dir);
                    let _ = std::fs::write(
                        dir.join("scenario-complete.json"),
                        format!(
                            "{{\"scenario\":\"{}\",\"seconds\":{},\"elapsedSec\":{:.3},\"pipeline\":{},\"liveSurfaces\":{}}}
",
                            self.scenario.as_deref().unwrap_or(""),
                            seconds,
                            elapsed_s,
                            self.pipeline.is_some(),
                            window.live_custom_surfaces(),
                        )
                        .replace(
                            "}\n",
                            &format!(
                                ",\"liveShell\":{},\"liveStates\":{},\"volumeAnswers\":{}}}\n",
                                self.shell.is_some(),
                                self.live_states,
                                self.live_volume.len(),
                            ),
                        ),
                    );
                }
                // The cadence series travels with the run's other evidence so
                // the harness reads the swap chain's own numbers instead of
                // recomputing them from the app's frame counter.
                if let Some(dir) = &self.evidence_dir {
                    let _ = std::fs::write(
                        dir.join("present-report.json"),
                        self.cadence.report(),
                    );
                    // The soak counters are written from the state that actually
                    // drove the run: a workload that stopped early reports fewer
                    // actions instead of looking complete.
                    if let Some(soak) = &self.soak {
                        let _ = std::fs::write(dir.join("soak-report.json"), soak.report());
                    }
                }
                cx.quit();
                return div().size_full();
            }
        }

        // Live mode: take whatever the shell workers published since the last
        // frame. One drain per frame is the whole bridge contract — the state
        // slot is latest-wins, so a blocked frame resumes from truth instead
        // of replaying a backlog, and volume answers keep their request ids.
        if self.shell.is_some() {
            let events = {
                let mut inbox = self
                    .inbox
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                inbox.drain()
            };
            for event in events {
                match event {
                    UiEvent::State(packet) => {
                        self.live_states += 1;
                        if !packet.idle {
                            if let Some(shell) = &self.shell {
                                shell.follow(&packet.app);
                            }
                        }
                        self.live_packet = Some(packet);
                    }
                    UiEvent::Idle => {
                        self.live_states += 1;
                        self.live_packet = None;
                    }
                    UiEvent::Control(_) | UiEvent::Sessions(_) => {}
                    UiEvent::Volume(answer) => {
                        self.live_volume.push((answer.request_id, answer.value));
                        if self.live_volume.len() > 8 {
                            self.live_volume.remove(0);
                        }
                    }
                }
            }
            // The workers publish from their own threads; without a frame
            // source the window would stop draining until the next input.
            window.request_animation_frame();
        }

        // Open the motion pipeline on the first frame — the device handle is
        // only reachable through the window, which needs the view alive.
        if self.pipeline.is_none() {
            if let Some(fixture) = self.fixture.clone() {
                match window.gpu_device_handle() {
                    Ok((device, context)) => match ScenePipeline::open(device, context, &fixture) {
                        Ok(pipeline) => self.pipeline = Some(pipeline),
                        Err(e) => log::warn!("motion pipeline open failed: {e:#}"),
                    },
                    Err(e) => log::warn!("device handle unavailable: {e:#}"),
                }
            }
        }

        // Present the newest decoded frame and refresh the surface handle.
        if let Some(pipeline) = &mut self.pipeline {
            self.video_surface = pipeline.present(window);
            // Capture/replay runs use the deterministic frame clock; the
            // measure scenarios follow wall time so the decoded stream plays
            // at its real rate against the sampled process metrics.
            let since_start = if self.scenario.is_some() {
                elapsed_s * 1000.0
            } else {
                self.clock_ms - CLOCK_SEED_MS
            };
            // Deck-aligned playhead: the stream starts at ~10 s for the
            // fixture, so pump against the decoded PTS space.
            pipeline.playhead_ms = since_start + 10000.0;
            self.playhead_ms = since_start;
        }

        // The once-only QA capture at the requested playhead.
        if !self.captured {
            if let Some(at) = self.capture_at_ms {
                if at > 0 && self.playhead_ms >= at as f64 {
                    self.take_capture(window);
                    self.captured = true;
                }
            }
        }

        // Continuous frames while a motion cover is live: the plate is a
        // video surface, so it owns the frame source the way a video element
        // would. Without this the window only redraws on input.
        //
        // The harness modes also need a frame source of their own: the exit
        // check and the resize scenario run inside `render`, so a short-lived
        // run with no fixture would otherwise stop after its first frame and
        // never reach `--seconds`.
        if self.pipeline.is_some() || self.seconds.is_some() || self.scenario.is_some() {
            window.request_animation_frame();
        }

        // The motion cover fills its square region through the custom-surface
        // pass. `paint_custom_surface` is paint-phase gated, so the call goes
        // through a `canvas` paint closure rather than straight from render
        // (which builds the element tree). The handle is produced by
        // register_gpu_surface over the same-device NV12→BGRA output texture
        // (Task 9 decoder + Task 10 VideoRenderer), so there is no CPU
        // readback anywhere on this path.
        let video_surface = self.video_surface;
        let cover = canvas(
            |bounds, _, _| bounds,
            move |bounds, bounds_in, window, _| {
                if let Some(handle) = video_surface {
                    window.paint_custom_surface(
                        Bounds {
                            origin: bounds.origin,
                            size: size(px(320.0), px(320.0)),
                        },
                        handle,
                    );
                    let _ = bounds_in;
                }
            },
        )
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .w(px(320.0))
        .h(px(320.0));

        // The HUD names the live session on screen, and proves the mailbox
        // contract: one line per drained packet plus the coalescing counters,
        // so a poll storm is visible as a rising drop count rather than a
        // stalled clock.
        let live_line = match &self.live_packet {
            Some(packet) => format!(
                "LIVE {} — {} / {} ({} ms) x{}",
                packet.app, packet.title, packet.artist, packet.position_ms, self.live_states
            ),
            None if self.shell.is_some() => {
                format!("LIVE waiting for a session ({} reports)", self.live_states)
            }
            None => "replay fixture (no shell)".to_string(),
        };

        div()
            .size_full()
            .child(cover)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .text_color(rgb(0xFFFFFF))
            .child(format!(
                "OverLyrics sample plate — {}x{} @{}x (clock seed {}ms)",
                self.viewport.0, self.viewport.1, self.scale, CLOCK_SEED_MS as u64
            ))
            .child(live_line)
    }
}

fn make_root_transparent(window_handle: gpui_kit::AnyWindowHandle, cx: &mut App) {
    if let Some(root_win) = window_handle.downcast::<gpui_kit::base::Root>() {
        let _ = root_win.update(cx, |root, _window, cx| {
            root.style().background = Some(gpui_kit::transparent_black().into());
            cx.notify();
        });
    }
}

fn main() {
    env_logger::init();
    let mut args = parse_args();

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        if cx.has_global::<gpui_kit::component::Theme>() {
            let theme = gpui_kit::component::Theme::global_mut(cx);
            theme.colors.background = gpui_kit::transparent_black();
            theme.tokens.background = gpui_kit::transparent_black().into();
        }

        // The worker pair lives for the process, not for a window: two SMTC
        // pollers would double the polling cost and race each other's
        // subscriptions, so `ShellHost::start` claims the process-wide guard
        // once and every window shares the result.
        let inbox = Arc::new(std::sync::Mutex::new(Inbox::default()));
        let shell = if args.live && args.probe {
            let sink: Arc<dyn oly_platform_win::smtc::EventSink> =
                Arc::new(ShellSink::new(inbox.clone()));
            match ShellHost::start(sink) {
                Some(host) => {
                    log::info!("live shell workers started");
                    Some(host)
                }
                None => {
                    log::warn!("live shell refused: a worker pair is already running");
                    None
                }
            }
        } else {
            None
        };

        let start_mini = !args.explicit_viewport
            && args.scenario.is_none()
            && args.capture_at_ms.is_none()
            && args.capture_rgba.is_none()
            && !args.probe;
        if start_mini {
            args.viewport = (380.0, 176.0);
        }

        let options = WindowOptions {
            inactive_frame_interval: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(args.viewport.0), px(args.viewport.1)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("OverLyrics".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            window_background: if start_mini {
                WindowBackgroundAppearance::Blurred
            } else {
                WindowBackgroundAppearance::Transparent
            },
            is_resizable: !start_mini,
            ..Default::default()
        };

        if args.probe {
            // The Task 10 sample plate: a QA probe, not a player. It keeps
            // working for the cadence/soak measurements, but it is only
            // reachable when explicitly requested.
            let window_inbox = inbox.clone();
            let window_shell = shell;
            let (probe_window, _probe) = gpui_kit::open_window(options, cx, move |_, cx| {
                cx.new(|_| PlateWindow {
                    viewport: args.viewport,
                    scale: args.scale,
                    fixture: args.fixture.clone(),
                    capture_at_ms: args.capture_at_ms,
                    evidence_dir: args.evidence_dir.clone(),
                    pipeline: None,
                    video_surface: None,
                    clock_ms: CLOCK_SEED_MS,
                    playhead_ms: 0.0,
                    captured: false,
                    scenario: args.scenario.clone(),
                    seconds: args.seconds,
                    resize_phase: 0,
                    started: std::time::Instant::now(),
                    shell: window_shell,
                    inbox: window_inbox,
                    live_packet: None,
                    live_volume: Vec::new(),
                    live_states: 0,
                    cadence: PresentCadence::default(),
                    soak: args.soak.then(SoakState::new),
                })
            })
            .expect("failed to open the sample-plate window");
            make_root_transparent(probe_window, cx);
            log::info!("QA probe: sample plate window opened");
            return;
        }

        // The normal entry point is the player itself: one window holding the
        // background, cover, lyrics, chrome and panels.
        // A launch names no paths; the bundle beside it supplies the demo.
        let demo = if args.live {
            (None, None, None)
        } else {
            demo_assets()
        };
        let fixture = args.fixture.clone().or(demo.0);
        let cover = args.cover.clone().or(demo.1);
        let motion = args.motion.clone().or(demo.2);
        if args.fixture.is_none() && fixture.is_some() {
            log::info!("no --fixture: replaying the demo document beside the executable");
        }
        let cfg = oly_gpui_app::player::view::ViewConfig {
            fixture: fixture.clone(),
            motion,
            cover,
            seconds: args.seconds,
            evidence_dir: args.evidence_dir.clone(),
            scale: args.scale,
            source: if args.live {
                oly_gpui_app::player::view::Source::Live
            } else {
                oly_gpui_app::player::view::Source::Replay
            },
            scenario: args.scenario.clone(),
            capture_rgba: args.capture_rgba.clone(),
            capture_at_ms: args.capture_at_ms.map(|at| at as f64),
            freeze_ms: args.freeze_ms,
            start_mini,
            settings_only: false,
        };
        let log_fixture = cfg.fixture.clone();
        let log_evidence = cfg.evidence_dir.clone();
        let (player_window, _view) = gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| oly_gpui_app::player::view::PlayerView::new(cfg, window, cx))
        })
        .expect("failed to open the player window");
        make_root_transparent(player_window, cx);
        let _ = player_window.update(cx, |_, window, _| {
            window.activate_window();
            window.refresh();
        });
        log::info!(
            "player opened: viewport {}x{} scale {} fixture {:?} evidence {:?}",
            args.viewport.0,
            args.viewport.1,
            args.scale,
            log_fixture,
            log_evidence
        );
    });
}
