//! The volume popover: three availability states, a debounced slider and the
//! flush-on-close rule.
//!
//! Legacy `VolumeControl.tsx` semantics, kept exactly:
//!
//! * Three states. `internal` — the player reports its own volume; the label
//!   is `播放器内部音量`. `app mixer` — the player only exposes the Windows
//!   app mixer; the label is `应用音量 · 音量合成器`. `unavailable` — the
//!   4-second wait timed out or the subscription errored; the label is
//!   `当前播放器音量不可用` and the slider is disabled.
//! * The slider is 0..100 step 1. Every change schedules a `setVolume` after
//!   80 ms (the legacy debounce); a new change inside the window replaces the
//!   pending value rather than queueing a second write.
//! * Closing the panel flushes any pending value immediately, using the
//!   `app`/`mode` captured *before* the panel closed (the legacy closes over
//!   `request.current`'s target), and bumps the request generation so a late
//!   in-flight result cannot overwrite it.
//!
//! The widget is a pure state machine; GPUI event wiring lives with the app
//! shell (Task 10).

/// Where a player's volume can be reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeSource {
    /// The player itself reports a volume: `getVolume(app, 'internal')`.
    Internal,
    /// Only the Windows app mixer is reachable.
    AppMixer,
}

impl VolumeSource {
    pub fn label(self) -> &'static str {
        match self {
            VolumeSource::Internal => "播放器内部音量",
            VolumeSource::AppMixer => "系统音量合成器",
        }
    }
}

/// The panel's data state, exactly the three the legacy renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeStatus {
    /// Reading succeeded; the value is authoritative.
    Ready(VolumeSource),
    /// The initial read is still within its 4 s budget.
    Reading,
    /// The read timed out or the subscription failed.
    Unavailable,
}

impl VolumeStatus {
    pub fn label(&self) -> &'static str {
        match self {
            VolumeStatus::Ready(source) => source.label(),
            VolumeStatus::Reading => "读取音量…",
            VolumeStatus::Unavailable => "当前播放器音量不可用",
        }
    }

    pub fn is_editable(&self) -> bool {
        matches!(self, VolumeStatus::Ready(_))
    }
}

/// The initial-read budget, ms (`VOLUME_TIMEOUT_MS` in the legacy component).
pub const READ_TIMEOUT_MS: f64 = 4000.0;
/// The slider write debounce, ms.
pub const DEBOUNCE_MS: f64 = 80.0;
/// Slider bounds and step, from the legacy `GlassSlider 0..100 step 1`.
pub const SLIDER_MIN: f64 = 0.0;
pub const SLIDER_MAX: f64 = 100.0;
pub const SLIDER_STEP: f64 = 1.0;

/// One volume panel.
#[derive(Debug)]
pub struct VolumePanel {
    status: VolumeStatus,
    value: f64,
    /// The app the panel is bound to; captured on open.
    app: String,
    /// The mixer mode captured on open (the legacy `mode` in the closure).
    mode: VolumeSource,
    /// The value scheduled for a debounced write, if any.
    pending: Option<f64>,
    /// When the pending write fires (open time + debounce).
    pending_at_ms: Option<f64>,
    /// Monotonic request generation; a flush or rebind invalidates writes.
    generation: u64,
}

impl VolumePanel {
    /// Opens the panel for one app, optimistically showing the last known
    /// volume. The legacy seeds the slider from the subscription's cached
    /// value; the app shell passes whatever it has.
    pub fn open(app: &str, cached: Option<f64>, mode: VolumeSource) -> Self {
        Self {
            status: VolumeStatus::Reading,
            value: cached.unwrap_or(50.0),
            app: app.to_string(),
            mode,
            pending: None,
            pending_at_ms: None,
            generation: 0,
        }
    }

    pub fn status(&self) -> &VolumeStatus {
        &self.status
    }

    pub fn value(&self) -> f64 {
        self.value
    }

    pub fn has_pending_write(&self) -> bool {
        self.pending.is_some()
    }

    pub fn app(&self) -> &str {
        &self.app
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The first read resolved. `None` means "the player does not expose an
    /// internal volume" — the panel falls back to the app mixer, exactly the
    /// legacy `getVolume(app, 'auto')` ladder.
    pub fn on_read(&mut self, value: Option<f64>, source: VolumeSource) {
        self.mode = source;
        self.status = VolumeStatus::Ready(source);
        if let Some(value) = value {
            self.value = value.clamp(SLIDER_MIN, SLIDER_MAX);
        }
    }

    /// The initial read exceeded its budget.
    pub fn on_timeout(&mut self) {
        self.status = VolumeStatus::Unavailable;
        self.pending = None;
        self.pending_at_ms = None;
    }

    /// The subscription errored after a successful read; the panel degrades.
    pub fn on_error(&mut self) {
        self.status = VolumeStatus::Unavailable;
        self.pending = None;
        self.pending_at_ms = None;
    }

    /// A slider move. Returns a write request when one should now be in
    /// flight; otherwise the move is only debounced. `now_ms` is the event
    /// time; the write fires at `now_ms + DEBOUNCE_MS` unless a newer move
    /// replaces it.
    pub fn on_slide(&mut self, value: f64, now_ms: f64) -> Option<VolumeWrite> {
        if !self.status.is_editable() {
            return None;
        }
        // Snap to the slider step, like the DOM range input does.
        let value = ((value - SLIDER_MIN) / SLIDER_STEP).round() * SLIDER_STEP + SLIDER_MIN;
        let value = value.clamp(SLIDER_MIN, SLIDER_MAX);
        self.value = value;
        self.pending = Some(value);
        self.pending_at_ms = Some(now_ms + DEBOUNCE_MS);
        None
    }

    /// Fires the debounced write when its time has come. Returns the request
    /// to send, tagged with the generation.
    pub fn tick(&mut self, now_ms: f64) -> Option<VolumeWrite> {
        let (value, at) = (self.pending?, self.pending_at_ms?);
        if now_ms + 1e-9 < at {
            return None;
        }
        self.pending = None;
        self.pending_at_ms = None;
        Some(VolumeWrite {
            app: self.app.clone(),
            source: self.mode,
            value,
            generation: self.generation,
        })
    }

    /// The panel is closing. Any pending value is written immediately with the
    /// target captured at open time, and the generation bumps so the write's
    /// eventual result cannot be double-applied by a later tick.
    pub fn flush_on_close(&mut self) -> Option<VolumeWrite> {
        self.generation += 1;
        self.pending.take().map(|value| VolumeWrite {
            app: self.app.clone(),
            source: self.mode,
            value,
            generation: self.generation,
        })
    }
}

/// One volume write the caller must deliver.
#[derive(Debug, Clone, PartialEq)]
pub struct VolumeWrite {
    pub app: String,
    pub source: VolumeSource,
    pub value: f64,
    pub generation: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "spotify.exe";

    #[test]
    fn the_labels_match_the_legacy_strings() {
        assert_eq!(
            VolumeStatus::Ready(VolumeSource::Internal).label(),
            "播放器内部音量"
        );
        assert_eq!(
            VolumeStatus::Ready(VolumeSource::AppMixer).label(),
            "系统音量合成器"
        );
        assert_eq!(
            VolumeStatus::Unavailable.label(),
            "当前播放器音量不可用"
        );
    }

    #[test]
    fn the_read_falls_back_to_the_app_mixer() {
        let mut panel = VolumePanel::open(APP, None, VolumeSource::Internal);
        assert_eq!(*panel.status(), VolumeStatus::Reading);
        panel.on_read(None, VolumeSource::AppMixer);
        assert_eq!(
            *panel.status(),
            VolumeStatus::Ready(VolumeSource::AppMixer),
            "no internal volume falls back to the mixer"
        );
        assert!(panel.status().is_editable());
    }

    #[test]
    fn a_timeout_disables_the_slider() {
        let mut panel = VolumePanel::open(APP, None, VolumeSource::Internal);
        panel.on_timeout();
        assert_eq!(*panel.status(), VolumeStatus::Unavailable);
        assert!(!panel.status().is_editable());
        assert!(
            panel.on_slide(30.0, 0.0).is_none() && panel.tick(10_000.0).is_none(),
            "an unavailable panel writes nothing"
        );
    }

    #[test]
    fn slides_debounce_and_coalesce() {
        let mut panel = VolumePanel::open(APP, Some(40.0), VolumeSource::Internal);
        panel.on_read(Some(40.0), VolumeSource::Internal);
        assert!(panel.on_slide(55.0, 0.0).is_none(), "a slide never writes directly");
        assert_eq!(panel.value(), 55.0);
        assert!(panel.tick(50.0).is_none(), "inside the debounce window");
        assert!(panel.on_slide(60.0, 60.0).is_none(), "the move replaces the pending value");
        let write = panel.tick(140.0).expect("past the debounce of the last move");
        assert_eq!(write.value, 60.0, "only the newest value is written");
        assert_eq!(write.app, APP);
        // After the write fires, nothing else is pending.
        assert!(panel.tick(1000.0).is_none());
    }

    #[test]
    fn closing_flushes_the_pending_value_with_the_captured_target() {
        let mut panel = VolumePanel::open(APP, Some(40.0), VolumeSource::Internal);
        panel.on_read(Some(40.0), VolumeSource::Internal);
        assert!(panel.on_slide(72.0, 0.0).is_none());
        let flush = panel.flush_on_close().expect("a pending value is flushed");
        assert_eq!(flush.value, 72.0);
        assert_eq!(flush.app, APP, "the app captured at open time");
        // The pending queue is drained; a later tick writes nothing.
        assert!(panel.tick(10_000.0).is_none());
        // And the bumped generation invalidates stale results.
        assert_eq!(panel.generation(), 1);
    }

    #[test]
    fn slider_values_snap_to_the_step_and_clamp() {
        let mut panel = VolumePanel::open(APP, None, VolumeSource::Internal);
        panel.on_read(Some(0.0), VolumeSource::Internal);
        panel.on_slide(33.4, 0.0);
        assert_eq!(panel.value(), 33.0, "snaps to the integer step");
        panel.on_slide(150.0, 0.0);
        assert_eq!(panel.value(), 100.0);
        panel.on_slide(-5.0, 0.0);
        assert_eq!(panel.value(), 0.0);
    }
}
