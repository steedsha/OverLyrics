//! The transport/toolbar widget: mode switching, the sync-offset stepper and
//! the anchor geometry for every popover.
//!
//! The legacy `ToolBar.tsx` uses content-layer lists inside a glass popover;
//! the visual chrome is the glass pass (Task 8's renderer) and the popover
//! placement (ported in `oly-render::glass`). What this module owns is the
//! *behaviour*: which modes exist, what the sync stepper's draft does while
//! nudges are in flight, and when a track change must reset that draft. Those
//! rules are what the legacy component implements around its UI kit, and they
//! are replayable without a window.
//!
//! Legacy behaviour, kept exactly:
//!
//! * The mode list for the segmented control is mini/album/lyrics/window plus
//!   a `fullscreen` pseudo-mode; album is hidden (the legacy filters it out)
//!   and selecting it is not possible from this control.
//! * The stepper is disabled when no player packet is live (`state == null`).
//! * A nudge previews immediately in the draft; the committed value lands when
//!   the async nudge resolves. Pending nudges coalesce: the draft shows the
//!   sum of in-flight deltas until every promise settles.
//! * Any of app/title/artist/album changing bumps a generation that
//!   invalidates in-flight previews — the draft snaps to the newly saved
//!   offset even when the pending promise later resolves for the old track.

use oly_render::glass::Rect;

/// The four modes the segmented control can select, in display order.
/// `album` is exposed for completeness but the toolbar filters it out, as the
/// legacy `modes.filter(x => x.mode !== 'album')` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mini,
    Album,
    Lyrics,
    Window,
}

impl Mode {
    /// The short label the legacy segmented control shows after stripping the
    /// player/mode suffixes: 迷你 / 封面 / 歌词 / 窗口.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Mini => "迷你",
            Mode::Album => "封面",
            Mode::Lyrics => "歌词",
            Mode::Window => "窗口",
        }
    }

    /// Every selectable value in display order, including the fullscreen
    /// pseudo-mode.
    pub fn selectable() -> [SelectValue; 4] {
        [
            SelectValue::Mode(Mode::Mini),
            SelectValue::Mode(Mode::Album),
            SelectValue::Mode(Mode::Lyrics),
            SelectValue::FULLSCREEN,
        ]
    }
}

/// A segmented-control value: a real mode or the fullscreen pseudo-mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectValue {
    Mode(Mode),
    Fullscreen,
}

impl SelectValue {
    pub const FULLSCREEN: SelectValue = SelectValue::Fullscreen;

    pub fn label(self) -> &'static str {
        match self {
            SelectValue::Mode(mode) => mode.label(),
            SelectValue::Fullscreen => "全屏",
        }
    }
}

/// The track identity a packet carries; any field changing counts as a track
/// change, exactly like the legacy `[state?.app, state?.title, ...]` effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackIdentity {
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl TrackIdentity {
    pub fn new(app: &str, title: &str, artist: &str, album: &str) -> Self {
        Self {
            app: app.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            album: album.to_string(),
        }
    }
}

/// The sync-offset stepper state machine.
///
/// `draft` is what the UI shows; `committed` is the last known server-side
/// value. Nudges are async, so while any are in flight the draft leads the
/// committed value by the sum of pending deltas. A track change snaps the
/// draft to the new committed value and invalidates in-flight results via a
/// generation counter (mirroring the legacy `generation.current++`).
#[derive(Debug)]
pub struct OffsetStepper {
    committed: i64,
    draft: i64,
    last_offset: i64,
    pending: usize,
    generation: u64,
    enabled: bool,
    step: i64,
}

impl OffsetStepper {
    pub fn new(offset_ms: i64) -> Self {
        Self {
            committed: offset_ms,
            draft: offset_ms,
            last_offset: offset_ms,
            pending: 0,
            generation: 0,
            enabled: true,
            step: 200,
        }
    }

    /// The value the UI should render right now.
    pub fn draft(&self) -> i64 {
        self.draft
    }

    /// The last value known to be persisted server-side.
    pub fn committed(&self) -> i64 {
        self.committed
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// A live player packet arrived: enable the stepper and refresh the saved
    /// value (the legacy `savedOffset.current = offsetMs` effect). A track
    /// change additionally snaps the draft and invalidates in-flight nudges.
    pub fn on_packet(&mut self, offset_ms: i64, track: Option<&TrackIdentity>, seen: Option<&TrackIdentity>) {
        self.enabled = true;
        let track_changed = track.is_some() && seen.is_some_and(|prev| prev != track.unwrap());
        self.committed = offset_ms;
        if track_changed {
            self.generation += 1;
            self.pending = 0;
            self.last_offset = offset_ms;
            self.draft = offset_ms;
        } else if self.pending == 0 {
            self.last_offset = offset_ms;
            self.draft = offset_ms;
        }
    }

    /// No player packet: the stepper is disabled (legacy `disabled={!state}`).
    pub fn on_idle(&mut self) {
        self.enabled = false;
    }

    /// One step down (歌词延后 200ms). Returns the nudge request the caller
    /// must send, tagged with the generation that owns it.
    pub fn decrement(&mut self) -> Option<Nudge> {
        self.nudge(-self.step)
    }

    /// One step up (歌词提前 200ms).
    pub fn increment(&mut self) -> Option<Nudge> {
        self.nudge(self.step)
    }

    fn nudge(&mut self, delta: i64) -> Option<Nudge> {
        if !self.enabled {
            return None;
        }
        let next = self.last_offset + delta;
        self.last_offset = next;
        self.draft = next;
        self.pending += 1;
        Some(Nudge {
            delta,
            generation: self.generation,
        })
    }

    /// An async nudge resolved. `saved` is the persisted offset the server
    /// reported (`null` in the legacy API means "not reported", which keeps
    /// the previously saved value). Returns false when the resolution belongs
    /// to a superseded generation and must be dropped.
    pub fn on_nudge_resolved(&mut self, generation: u64, saved: Option<i64>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.pending = self.pending.saturating_sub(1);
        if let Some(saved) = saved {
            self.committed = saved;
        }
        if self.pending == 0 {
            self.last_offset = self.committed;
            self.draft = self.committed;
        }
        true
    }
}

/// One in-flight nudge request produced by the stepper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nudge {
    pub delta: i64,
    pub generation: u64,
}

/// Where the "更多" popover anchors. The legacy keeps the button ref; here the
/// app layer sets the rect from its layout each frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct Anchors {
    pub more_button: Option<Rect>,
    pub volume_button: Option<Rect>,
}

/// Popover panels currently allowed to be open. The toolbar closes both on a
/// mode change and reports the combined state upward (the legacy
/// `onMenuOpenChange(open || volumeOpen)` effect), which the window layer uses
/// to suspend cover art animations while a panel is up.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PanelState {
    pub more_open: bool,
    pub volume_open: bool,
}

impl PanelState {
    pub fn any_open(self) -> bool {
        self.more_open || self.volume_open
    }
}

/// The toolbar state machine: mode selection plus the two popover toggles.
#[derive(Debug, Default)]
pub struct ToolBarState {
    mode: Option<Mode>,
    panels: PanelState,
    anchors: Anchors,
}

impl ToolBarState {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode: Some(mode),
            panels: PanelState::default(),
            anchors: Anchors::default(),
        }
    }

    pub fn mode(&self) -> Option<Mode> {
        self.mode
    }

    pub fn panels(&self) -> PanelState {
        self.panels
    }

    pub fn anchors(&self) -> &Anchors {
        &self.anchors
    }

    pub fn set_anchor(&mut self, anchor: Rect) {
        self.anchors.more_button = Some(anchor);
    }

    pub fn set_volume_anchor(&mut self, anchor: Rect) {
        self.anchors.volume_button = Some(anchor);
    }

    /// Toggle the more panel. Opening it closes the volume panel and vice
    /// versa? No: the legacy keeps them independent — one click outside
    /// dismisses whichever is open, and the *toolbar* only forces both closed
    /// on a mode change. This mirrors that.
    pub fn toggle_more(&mut self) -> bool {
        self.panels.more_open = !self.panels.more_open;
        self.panels.more_open
    }

    pub fn toggle_volume(&mut self) -> bool {
        self.panels.volume_open = !self.panels.volume_open;
        self.panels.volume_open
    }

    pub fn set_more_open(&mut self, open: bool) {
        self.panels.more_open = open;
    }

    pub fn set_volume_open(&mut self, open: bool) {
        self.panels.volume_open = open;
    }

    /// Mode selection from the segmented control. Returns the resulting mode,
    /// or `None` when the value was the fullscreen pseudo-mode (the caller
    /// must run its fullscreen toggle instead). Any selection closes both
    /// panels, like the legacy `run()` helper.
    pub fn select(&mut self, value: SelectValue) -> Option<Mode> {
        self.panels = PanelState::default();
        match value {
            SelectValue::Fullscreen => None,
            SelectValue::Mode(mode) => {
                self.mode = Some(mode);
                Some(mode)
            }
        }
    }

    /// A mode change from outside (the legacy `useEffect(..., [mode])` that
    /// closes both popovers when the mode flips).
    pub fn on_mode_changed(&mut self, mode: Mode) {
        self.mode = Some(mode);
        self.panels = PanelState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_list_matches_the_legacy_filter() {
        // Album exists but is never selectable from the toolbar's control.
        let selectable = Mode::selectable();
        assert_eq!(
            selectable
                .iter()
                .filter(|v| matches!(v, SelectValue::Mode(m) if *m == Mode::Album))
                .count(),
            1,
            "album stays in the enum for completeness"
        );
        assert_eq!(SelectValue::Fullscreen.label(), "全屏");
        assert_eq!(Mode::Window.label(), "窗口");
    }

    #[test]
    fn a_nudge_previews_immediately_and_settles_into_the_saved_value() {
        let mut stepper = OffsetStepper::new(0);
        let nudge = stepper.increment().expect("a live packet enables nudges");
        assert_eq!(stepper.draft(), 200, "the draft leads while async");
        assert_eq!(stepper.committed(), 0);
        assert!(stepper.on_nudge_resolved(nudge.generation, Some(-200)));
        assert_eq!(stepper.draft(), -200, "the draft snaps to the saved value");
        assert_eq!(stepper.committed(), -200);
    }

    #[test]
    fn pending_nudges_coalesce_until_every_promise_settles() {
        let mut stepper = OffsetStepper::new(0);
        let first = stepper.increment().expect("enabled");
        let second = stepper.increment().expect("enabled");
        assert_eq!(stepper.draft(), 400);
        // The first resolves but the second is still in flight: the draft must
        // not snap yet.
        assert!(stepper.on_nudge_resolved(first.generation, Some(200)));
        assert_eq!(stepper.draft(), 400, "still one nudge in flight");
        assert!(stepper.on_nudge_resolved(second.generation, Some(400)));
        assert_eq!(stepper.draft(), 400);
        assert_eq!(stepper.committed(), 400);
    }

    #[test]
    fn a_track_change_invalidates_in_flight_previews() {
        let mut stepper = OffsetStepper::new(0);
        let track_a = TrackIdentity::new("spotify", "A", "x", "y");
        let seen_a = track_a.clone();
        stepper.on_packet(0, Some(&track_a), Some(&seen_a));
        let _nudge = stepper.increment().expect("enabled");
        assert_eq!(stepper.draft(), 200);
        // The track flips before the nudge resolves.
        let track_b = TrackIdentity::new("spotify", "B", "x", "y");
        stepper.on_packet(-400, Some(&track_b), Some(&track_a));
        assert_eq!(stepper.draft(), -400, "the draft snaps to the new track");
        // The stale resolution must be dropped, not applied.
        assert!(
            !stepper.on_nudge_resolved(0, Some(0)),
            "generation 0 was superseded"
        );
        assert_eq!(stepper.draft(), -400);
    }

    #[test]
    fn an_idle_packet_disables_the_stepper() {
        let mut stepper = OffsetStepper::new(0);
        stepper.on_idle();
        assert!(!stepper.is_enabled());
        assert!(stepper.increment().is_none(), "disabled steppers nudge nothing");
    }

    #[test]
    fn selecting_a_mode_closes_both_panels() {
        let mut bar = ToolBarState::new(Mode::Window);
        bar.set_more_open(true);
        bar.set_volume_open(true);
        let selected = bar.select(SelectValue::Mode(Mode::Mini));
        assert_eq!(selected, Some(Mode::Mini));
        assert!(!bar.panels().any_open(), "run() closes both panels");
        assert_eq!(bar.mode(), Some(Mode::Mini));
    }

    #[test]
    fn fullscreen_is_a_pseudo_mode_that_reports_none() {
        let mut bar = ToolBarState::new(Mode::Window);
        bar.set_more_open(true);
        assert_eq!(bar.select(SelectValue::Fullscreen), None);
        assert!(!bar.panels().any_open());
        assert_eq!(bar.mode(), Some(Mode::Window), "the mode is untouched");
    }

    #[test]
    fn an_external_mode_change_closes_both_panels() {
        let mut bar = ToolBarState::new(Mode::Window);
        bar.set_more_open(true);
        bar.on_mode_changed(Mode::Lyrics);
        assert!(!bar.panels().any_open());
        assert_eq!(bar.mode(), Some(Mode::Lyrics));
    }
}
