//! The lyric timeline: which rows are playing, which are still lit, and where
//! the scroll target sits.
//!
//! This is a port of AMLL's `TimelineController`, not a simplification of it.
//! Four of its rules look like accidents and are not, so each one is spelled
//! out where it lives:
//!
//!   * a row is *playing* only inside the half-open window `[start, end)`, so a
//!     row that ends exactly as the next begins never double-counts a frame;
//!   * a row that stops playing keeps its *highlight* until some other row
//!     starts, which is what stops the current line blinking off in the gap
//!     before the next one and what the "已唱" styling reads;
//!   * a gap of at least [`MIN_INTERLUDE_GAP_MS`] between the furthest end time
//!     seen so far and the next start is an *interlude*, and an interlude with
//!     nothing playing hands focus away from the lyrics and clears the
//!     highlight -- otherwise a held syllable would stay lit through a pause;
//!   * a seek (explicit, or time going backwards) rebuilds both sets from the
//!     target time instead of stepping, because stepping cannot un-light rows
//!     that the jump skipped over.
//!
//! `playing` and `highlighted` are kept apart on purpose. The long-note glow
//! animator must only animate on a row that is playing *now*; the lingering
//! highlight must not be able to start one. Collapsing the two would light a
//! held syllable in the silence after its line finished.

use std::collections::BTreeSet;

use oly_contracts::AmllLine;

/// A gap shorter than this stays a plain gap: the reference treats shorter
/// silences as part of the phrasing, and only a longer one earns the interlude
/// treatment.
pub const MIN_INTERLUDE_GAP_MS: f64 = 7_000.0;

/// One row's time window, in milliseconds on the media timeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineWindow {
    pub start_ms: f64,
    pub end_ms: f64,
}

impl LineWindow {
    pub fn new(start_ms: f64, end_ms: f64) -> Self {
        Self { start_ms, end_ms }
    }

    /// Project optimised AMLL lines, which are already sorted by start time.
    pub fn from_lines(lines: &[AmllLine]) -> Vec<Self> {
        lines
            .iter()
            .map(|line| Self::new(line.start_time, line.end_time))
            .collect()
    }
}

/// A silence long enough to be worth showing interlude dots in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interlude {
    pub start_ms: f64,
    pub end_ms: f64,
    /// The row the interlude follows; `-1` for silence before the first row.
    pub anchor_line_index: isize,
}

/// The read-only state the renderer consumes once per frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineSnapshot {
    pub current_ms: f64,
    pub playing: Vec<usize>,
    pub highlighted: Vec<usize>,
    pub scroll_to_index: usize,
    pub latest_highlighted_index: Option<usize>,
    pub is_end_of_song: bool,
    pub active_interlude: Option<Interlude>,
    pub focus_on_interlude: bool,
}

/// What changed relative to the previous `sync`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineDiff {
    pub has_changed: bool,
    pub is_time_jumped: bool,
    pub added_playing: Vec<usize>,
    pub removed_playing: Vec<usize>,
    pub added_highlighted: Vec<usize>,
    pub removed_highlighted: Vec<usize>,
    pub is_interlude_changed: bool,
    pub is_scroll_to_changed: bool,
    pub is_end_of_song_changed: bool,
}

/// The timeline itself. Rows must be sorted by `start_ms`.
#[derive(Debug)]
pub struct Timeline {
    bounds: Vec<LineWindow>,
    interludes: Vec<Interlude>,
    max_end_ms: f64,
    playback_cursor: usize,
    interlude_cursor: usize,
    playing: BTreeSet<usize>,
    highlighted: BTreeSet<usize>,
    next_playing: BTreeSet<usize>,
    next_highlighted: BTreeSet<usize>,
    snapshot: TimelineSnapshot,
    diff: TimelineDiff,
}

impl Timeline {
    pub fn new(bounds: Vec<LineWindow>) -> Self {
        let interludes = Self::calculate_interludes(&bounds);
        let max_end_ms = bounds.iter().fold(0.0_f64, |acc, b| acc.max(b.end_ms));
        Self {
            bounds,
            interludes,
            max_end_ms,
            playback_cursor: 0,
            interlude_cursor: 0,
            playing: BTreeSet::new(),
            highlighted: BTreeSet::new(),
            next_playing: BTreeSet::new(),
            next_highlighted: BTreeSet::new(),
            snapshot: TimelineSnapshot::default(),
            diff: TimelineDiff::default(),
        }
    }

    pub fn snapshot(&self) -> &TimelineSnapshot {
        &self.snapshot
    }

    pub fn bounds(&self) -> &[LineWindow] {
        &self.bounds
    }

    pub fn interludes(&self) -> &[Interlude] {
        &self.interludes
    }

    /// Whether a row is playing at the current playhead. This is the gate the
    /// long-note animator asks about; a future row must answer `false`.
    pub fn is_playing(&self, index: usize) -> bool {
        self.playing.contains(&index)
    }

    /// Advance (or rebuild) the timeline for a playhead.
    ///
    /// `force_seek` marks a jump the caller already knows about; time going
    /// backwards is treated as a jump even without the flag, because a clock
    /// that rewound cannot have done so by playing.
    pub fn sync(&mut self, time_ms: f64, force_seek: bool) -> TimelineDiff {
        self.diff = TimelineDiff::default();

        let prev_interlude = self.snapshot.active_interlude;
        let prev_focus = self.snapshot.focus_on_interlude;
        let prev_scroll = self.snapshot.scroll_to_index;
        let prev_end = self.snapshot.is_end_of_song;

        let is_retreating = time_ms < self.snapshot.current_ms;
        let is_jump = force_seek || is_retreating;

        let active = self.resolve_active_interlude(time_ms, is_jump);
        self.snapshot.active_interlude = active;

        let past_last = !self.bounds.is_empty() && time_ms >= self.max_end_ms;
        if is_jump {
            self.perform_seek(time_ms, active.is_some() || past_last);
        } else {
            self.perform_playback(time_ms);
        }

        // An interlude with nothing singing, and the end of the song, are the
        // two moments with no following row to take the highlight over. Both
        // must clear it explicitly or the last line stays lit forever.
        if active.is_some() && self.playing.is_empty() {
            self.flush_all_highlighted();
        }
        if past_last {
            self.flush_all_highlighted();
        }
        self.snapshot.focus_on_interlude = active.is_some() && self.highlighted.is_empty();

        let interlude_changed = prev_interlude != active;
        let focus_changed = prev_focus != self.snapshot.focus_on_interlude;
        let scroll_changed = prev_scroll != self.snapshot.scroll_to_index;
        let end_changed = past_last != prev_end;

        self.diff.has_changed = is_jump
            || !self.diff.added_playing.is_empty()
            || !self.diff.removed_playing.is_empty()
            || !self.diff.added_highlighted.is_empty()
            || !self.diff.removed_highlighted.is_empty()
            || interlude_changed
            || focus_changed
            || scroll_changed
            || end_changed;
        self.diff.is_time_jumped = is_jump;
        self.diff.is_interlude_changed = interlude_changed;
        self.diff.is_scroll_to_changed = scroll_changed;
        self.diff.is_end_of_song_changed = end_changed;

        self.snapshot.current_ms = time_ms;
        self.snapshot.latest_highlighted_index = self.highlighted.iter().next_back().copied();
        self.snapshot.is_end_of_song = past_last;
        self.snapshot.playing = self.playing.iter().copied().collect();
        self.snapshot.highlighted = self.highlighted.iter().copied().collect();

        self.diff.clone()
    }

    /// Step the sets forward from the playhead. Only rows the cursor has not
    /// passed yet can start, which is what makes this O(new rows) per frame.
    fn perform_playback(&mut self, time_ms: f64) {
        let expired: Vec<usize> = self
            .playing
            .iter()
            .copied()
            .filter(|&id| match self.bounds.get(id) {
                Some(bound) => time_ms < bound.start_ms || bound.end_ms <= time_ms,
                None => true,
            })
            .collect();
        for id in expired {
            self.playing.remove(&id);
            self.diff.removed_playing.push(id);
        }

        let len = self.bounds.len();
        let mut cursor = self.playback_cursor.min(len);
        while cursor < len {
            let bound = self.bounds[cursor];
            if bound.start_ms > time_ms {
                break;
            }
            if bound.end_ms > time_ms && !self.playing.contains(&cursor) {
                self.playing.insert(cursor);
                self.diff.added_playing.push(cursor);
            }
            cursor += 1;
        }
        self.playback_cursor = cursor;

        // A row that stopped playing is only *expired*; it keeps its highlight
        // until a new row starts and displaces it.
        let expired_highlighted: Vec<usize> = self
            .highlighted
            .iter()
            .copied()
            .filter(|id| !self.playing.contains(id))
            .collect();

        let started = self.diff.added_playing.clone();
        for id in &started {
            self.highlighted.insert(*id);
            self.diff.added_highlighted.push(*id);
        }
        if !started.is_empty() {
            for id in expired_highlighted {
                self.highlighted.remove(&id);
                self.diff.removed_highlighted.push(id);
            }
            if let Some(min) = self.highlighted.iter().next().copied() {
                self.snapshot.scroll_to_index = min;
            }
        }
    }

    /// Rebuild both sets from the target time. A seek cannot be stepped: rows
    /// the jump flew over were never "playing", so nothing would remove them.
    fn perform_seek(&mut self, time_ms: f64, drop_lingering_when_idle: bool) {
        self.next_playing.clear();
        self.next_highlighted.clear();

        let len = self.bounds.len();
        let mut left = 0isize;
        let mut right = len as isize - 1;
        let mut first_greater = len;
        while left <= right {
            let mid = ((left + right) >> 1) as usize;
            if self.bounds[mid].start_ms > time_ms {
                first_greater = mid;
                right = mid as isize - 1;
            } else {
                left = mid as isize + 1;
            }
        }

        // Walking back from the first row that starts after the target gives
        // the row the playhead is "inside", even when that row began before an
        // earlier seek target.
        let mut anchor_index: Option<usize> = None;
        if first_greater > 0 {
            for i in (0..first_greater).rev() {
                let bound = self.bounds[i];
                if bound.end_ms > bound.start_ms {
                    anchor_index = Some(i);
                    break;
                }
            }
        }

        let Some(anchor_index) = anchor_index else {
            self.playback_cursor = first_greater;
            self.snapshot.scroll_to_index = 0;
            self.commit_seek_diff();
            return;
        };

        let anchor_start = self.bounds[anchor_index].start_ms;
        let mut min_playing: Option<usize> = None;
        let mut min_highlighted: Option<usize> = None;
        for i in (0..=anchor_index).rev() {
            let bound = self.bounds[i];
            if bound.end_ms <= anchor_start {
                continue;
            }
            if bound.start_ms <= time_ms && bound.end_ms > time_ms {
                self.next_playing.insert(i);
                min_playing = Some(i);
            }
            self.next_highlighted.insert(i);
            min_highlighted = Some(i);
        }

        if drop_lingering_when_idle && self.next_playing.is_empty() {
            self.next_highlighted.clear();
        }

        self.playback_cursor = min_playing.unwrap_or(first_greater);
        self.snapshot.scroll_to_index = min_highlighted.unwrap_or(0);
        self.commit_seek_diff();
    }

    fn commit_seek_diff(&mut self) {
        let stale_playing: Vec<usize> = self
            .playing
            .iter()
            .copied()
            .filter(|id| !self.next_playing.contains(id))
            .collect();
        for id in stale_playing {
            self.playing.remove(&id);
            self.diff.removed_playing.push(id);
        }
        let fresh_playing: Vec<usize> = self
            .next_playing
            .iter()
            .copied()
            .filter(|id| !self.playing.contains(id))
            .collect();
        for id in fresh_playing {
            self.playing.insert(id);
            self.diff.added_playing.push(id);
        }

        let stale_highlighted: Vec<usize> = self
            .highlighted
            .iter()
            .copied()
            .filter(|id| !self.next_highlighted.contains(id))
            .collect();
        for id in stale_highlighted {
            self.highlighted.remove(&id);
            self.diff.removed_highlighted.push(id);
        }
        let fresh_highlighted: Vec<usize> = self
            .next_highlighted
            .iter()
            .copied()
            .filter(|id| !self.highlighted.contains(id))
            .collect();
        for id in fresh_highlighted {
            self.highlighted.insert(id);
            self.diff.added_highlighted.push(id);
        }
    }

    fn flush_all_highlighted(&mut self) {
        if self.highlighted.is_empty() {
            return;
        }
        for id in std::mem::take(&mut self.highlighted) {
            self.diff.removed_highlighted.push(id);
        }
    }

    /// Gaps are measured from the furthest end time seen so far, so a long
    /// syllable that hangs over the next row's start cannot manufacture a gap
    /// between the two.
    fn calculate_interludes(bounds: &[LineWindow]) -> Vec<Interlude> {
        let mut interludes = Vec::new();
        let mut max_end = 0.0_f64;
        if bounds.is_empty() {
            return interludes;
        }
        for i in -1..(bounds.len() as isize - 1) {
            if i >= 0 {
                max_end = max_end.max(bounds[i as usize].end_ms);
            }
            let next_start = bounds[(i + 1) as usize].start_ms;
            let gap_end = max_end.max(next_start);
            if gap_end - max_end >= MIN_INTERLUDE_GAP_MS {
                interludes.push(Interlude {
                    start_ms: max_end,
                    end_ms: gap_end,
                    anchor_line_index: i,
                });
            }
        }
        interludes
    }

    fn resolve_active_interlude(&mut self, time_ms: f64, is_seek: bool) -> Option<Interlude> {
        if self.interludes.is_empty() {
            return None;
        }
        if is_seek {
            let mut cursor = self.interludes.len();
            let mut left = 0isize;
            let mut right = self.interludes.len() as isize - 1;
            while left <= right {
                let mid = ((left + right) >> 1) as usize;
                if self.interludes[mid].end_ms > time_ms {
                    cursor = mid;
                    right = mid as isize - 1;
                } else {
                    left = mid as isize + 1;
                }
            }
            self.interlude_cursor = cursor;
            if let Some(inter) = self.interludes.get(cursor) {
                if time_ms >= inter.start_ms && time_ms < inter.end_ms {
                    return Some(*inter);
                }
            }
            return None;
        }

        while let Some(inter) = self.interludes.get(self.interlude_cursor).copied() {
            if time_ms >= inter.start_ms && time_ms < inter.end_ms {
                return Some(inter);
            }
            if time_ms >= inter.end_ms {
                self.interlude_cursor += 1;
            } else {
                break;
            }
        }
        None
    }

    /// Back to the origin, for a new document.
    pub fn reset(&mut self) {
        self.playback_cursor = 0;
        self.interlude_cursor = 0;
        self.playing.clear();
        self.highlighted.clear();
        self.next_playing.clear();
        self.next_highlighted.clear();
        self.snapshot = TimelineSnapshot::default();
        self.diff = TimelineDiff::default();
    }
}

const SEEK_JITTER_TOLERANCE_MS: f64 = 150.0;
const SEEK_DRIFT_SLACK: f64 = 0.5;
const SEEK_MAX_TRUSTED_GAP_MS: f64 = 800.0;

/// Automatic seek detector, matching AMLL's `SeekDetector`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SeekDetector {
    last_media_ms: f64,
    last_wall_ms: f64,
    has_baseline: bool,
}

impl SeekDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Infer whether a progress push represents a jump relative to elapsed wall time.
    pub fn detect(&mut self, media_ms: f64, is_playing: bool, wall_ms: f64) -> bool {
        if !self.has_baseline {
            self.rebase(media_ms, wall_ms);
            self.has_baseline = true;
            return false;
        }
        if media_ms < self.last_media_ms {
            self.rebase(media_ms, wall_ms);
            return true;
        }
        let media_delta = media_ms - self.last_media_ms;
        let wall_delta = (wall_ms - self.last_wall_ms)
            .max(0.0)
            .min(SEEK_MAX_TRUSTED_GAP_MS);
        self.rebase(media_ms, wall_ms);
        let expected = if is_playing { wall_delta } else { 0.0 };
        let tolerance = if is_playing {
            SEEK_JITTER_TOLERANCE_MS.max(wall_delta * SEEK_DRIFT_SLACK)
        } else {
            SEEK_JITTER_TOLERANCE_MS
        };
        (media_delta - expected) > tolerance
    }

    pub fn reset(&mut self) {
        self.has_baseline = false;
        self.last_media_ms = 0.0;
        self.last_wall_ms = 0.0;
    }

    fn rebase(&mut self, media_ms: f64, wall_ms: f64) {
        self.last_media_ms = media_ms;
        self.last_wall_ms = wall_ms;
    }
}

/// Alignment target resolved by [`FocusController`], matching AMLL's `FocusController`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocalTarget {
    Line(usize),
    Interlude(isize),
    Bottom,
}

impl Default for FocalTarget {
    fn default() -> Self {
        Self::Line(0)
    }
}

/// Cross-frame focal target state machine (freezes during manual browsing,
/// hands off from an expired interlude, and aligns bottom credits at end-of-song).
#[derive(Clone, Copy, Debug, Default)]
pub struct FocusController {
    target: FocalTarget,
}

impl FocusController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn target(&self) -> FocalTarget {
        self.target
    }

    pub fn resolve(
        &mut self,
        snapshot: &TimelineSnapshot,
        line_count: usize,
        is_auto_align_suspended: bool,
        has_bottom_content: bool,
        can_display_interlude: bool,
    ) -> FocalTarget {
        let next = if is_auto_align_suspended {
            self.resolve_suspended(snapshot, line_count, can_display_interlude)
        } else {
            self.resolve_active(snapshot, line_count, has_bottom_content, can_display_interlude)
        };
        self.target = next;
        next
    }

    fn resolve_suspended(
        &self,
        snapshot: &TimelineSnapshot,
        line_count: usize,
        can_display_interlude: bool,
    ) -> FocalTarget {
        match self.target {
            FocalTarget::Line(index) => FocalTarget::Line(Self::clamp_index(index, line_count)),
            FocalTarget::Interlude(anchor) => {
                if !(snapshot.focus_on_interlude
                    && snapshot.active_interlude.is_some()
                    && can_display_interlude)
                {
                    let next = (anchor + 1).max(0) as usize;
                    FocalTarget::Line(Self::clamp_index(next, line_count))
                } else {
                    FocalTarget::Interlude(anchor)
                }
            }
            FocalTarget::Bottom => FocalTarget::Bottom,
        }
    }

    fn resolve_active(
        &self,
        snapshot: &TimelineSnapshot,
        line_count: usize,
        has_bottom_content: bool,
        can_display_interlude: bool,
    ) -> FocalTarget {
        if snapshot.focus_on_interlude
            && let Some(interlude) = snapshot.active_interlude
        {
            if !can_display_interlude {
                let next = (interlude.anchor_line_index + 1).max(0) as usize;
                return FocalTarget::Line(Self::clamp_index(next, line_count));
            }
            return FocalTarget::Interlude(interlude.anchor_line_index);
        }
        if snapshot.is_end_of_song {
            if has_bottom_content {
                return FocalTarget::Bottom;
            }
            return FocalTarget::Line(Self::clamp_index(
                line_count.saturating_sub(1),
                line_count,
            ));
        }
        FocalTarget::Line(Self::clamp_index(snapshot.scroll_to_index, line_count))
    }

    fn clamp_index(index: usize, line_count: usize) -> usize {
        if line_count == 0 {
            0
        } else {
            index.min(line_count - 1)
        }
    }

    pub fn reset(&mut self) {
        self.target = FocalTarget::Line(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(start: f64, end: f64) -> LineWindow {
        LineWindow::new(start, end)
    }

    #[test]
    fn a_line_is_playing_only_inside_its_half_open_window() {
        let mut tl = Timeline::new(vec![row(1000.0, 2000.0)]);
        tl.sync(999.0, false);
        assert!(tl.snapshot().playing.is_empty(), "not yet started");

        tl.sync(1000.0, false);
        assert_eq!(tl.snapshot().playing, vec![0]);

        tl.sync(1999.0, false);
        assert_eq!(tl.snapshot().playing, vec![0]);

        tl.sync(2000.0, false);
        assert!(
            tl.snapshot().playing.is_empty(),
            "half-open window closes at endTime"
        );
    }

    #[test]
    fn a_finished_line_stays_highlighted_until_the_next_one_starts() {
        let mut tl = Timeline::new(vec![row(0.0, 1000.0), row(2000.0, 3000.0)]);
        tl.sync(500.0, false);
        assert_eq!(tl.snapshot().playing, vec![0]);
        assert_eq!(tl.snapshot().highlighted, vec![0]);

        // The window closed but nothing new started: the highlight lingers.
        tl.sync(1500.0, false);
        assert!(tl.snapshot().playing.is_empty());
        assert_eq!(
            tl.snapshot().highlighted,
            vec![0],
            "a sung line keeps its highlight"
        );

        // A new line starts, and the expired one goes out with it.
        tl.sync(2500.0, false);
        assert_eq!(tl.snapshot().playing, vec![1]);
        assert_eq!(tl.snapshot().highlighted, vec![1]);
        assert_eq!(tl.snapshot().scroll_to_index, 1);
    }

    #[test]
    fn overlapping_lines_both_play_at_once() {
        // Duet rows and harmony rows overlap by design; a single "current line"
        // index would drop one of them and make a held syllable go dark.
        let mut tl = Timeline::new(vec![row(0.0, 3000.0), row(1000.0, 4000.0)]);
        tl.sync(2000.0, false);
        assert_eq!(tl.snapshot().playing, vec![0, 1]);
        assert_eq!(tl.snapshot().highlighted, vec![0, 1]);
    }

    #[test]
    fn a_short_gap_is_not_an_interlude() {
        let mut tl = Timeline::new(vec![row(0.0, 1000.0), row(4000.0, 5000.0)]);
        tl.sync(2000.0, false);
        assert!(
            tl.snapshot().active_interlude.is_none(),
            "a 3 s gap stays a gap"
        );
    }

    #[test]
    fn a_long_gap_becomes_an_interlude_and_clears_the_highlight() {
        let mut tl = Timeline::new(vec![row(0.0, 1000.0), row(9000.0, 12000.0)]);
        tl.sync(500.0, false);
        assert_eq!(tl.snapshot().highlighted, vec![0]);

        tl.sync(2000.0, false);
        let interlude = tl
            .snapshot()
            .active_interlude
            .expect("an 8 s gap is an interlude");
        assert_eq!(interlude.start_ms, 1000.0);
        assert_eq!(interlude.end_ms, 9000.0);
        assert_eq!(interlude.anchor_line_index, 0);
        assert!(
            tl.snapshot().highlighted.is_empty(),
            "the interlude hands focus away from the lyrics"
        );
        assert!(tl.snapshot().focus_on_interlude);
    }

    #[test]
    fn a_seek_rebuilds_the_sets_from_the_target_time() {
        let mut tl = Timeline::new(vec![
            row(0.0, 1000.0),
            row(1000.0, 2000.0),
            row(2000.0, 3000.0),
        ]);
        tl.sync(500.0, false);

        // Seeking forward must not leave the old line lit: a seek is a fresh
        // derivation from the target time, not an incremental step.
        let diff = tl.sync(2500.0, true);
        assert!(diff.is_time_jumped);
        assert_eq!(tl.snapshot().playing, vec![2]);
        assert_eq!(tl.snapshot().highlighted, vec![2]);

        // Seeking back onto a row that started earlier re-derives it too.
        tl.sync(0.0, true);
        assert_eq!(tl.snapshot().playing, vec![0]);
    }

    #[test]
    fn a_backwards_jump_is_treated_as_a_seek() {
        let mut tl = Timeline::new(vec![row(0.0, 1000.0), row(1000.0, 2000.0)]);
        tl.sync(1500.0, false);
        assert_eq!(tl.snapshot().playing, vec![1]);
        let diff = tl.sync(200.0, false);
        assert!(
            diff.is_time_jumped,
            "time going backwards is a seek even without the flag"
        );
        assert_eq!(tl.snapshot().playing, vec![0]);
    }

    #[test]
    fn the_last_line_leaves_no_highlight_behind() {
        let mut tl = Timeline::new(vec![row(0.0, 1000.0)]);
        tl.sync(500.0, false);
        assert_eq!(tl.snapshot().highlighted, vec![0]);
        tl.sync(1000.0, false);
        assert!(tl.snapshot().is_end_of_song);
        assert!(
            tl.snapshot().highlighted.is_empty(),
            "song end clears the highlight"
        );
    }

    #[test]
    fn the_scroll_target_tracks_the_earliest_highlighted_line() {
        let mut tl = Timeline::new(vec![row(0.0, 1000.0), row(1000.0, 2000.0)]);
        tl.sync(500.0, false);
        assert_eq!(tl.snapshot().scroll_to_index, 0);
        tl.sync(1200.0, false);
        assert_eq!(tl.snapshot().scroll_to_index, 1);
    }

    #[test]
    fn a_leading_silence_is_an_interlude_too() {
        // Ten seconds of nothing before the first row is exactly the case the
        // reference shows dots for; measuring gaps only *between* rows would
        // leave the intro blank.
        let tl = Timeline::new(vec![row(10_000.0, 12_000.0)]);
        assert_eq!(tl.interludes().len(), 1);
        let interlude = tl.interludes()[0];
        assert_eq!(interlude.start_ms, 0.0);
        assert_eq!(interlude.end_ms, 10_000.0);
        assert_eq!(interlude.anchor_line_index, -1);
    }

    #[test]
    fn a_held_syllable_does_not_manufacture_an_interlude() {
        // Row 0 runs to 9 s and row 1 starts at 8 s: the overlap means there is
        // no silence, even though the second start is 8 s after the first.
        let tl = Timeline::new(vec![row(0.0, 9000.0), row(8000.0, 10_000.0)]);
        assert!(tl.interludes().is_empty());
    }

    #[test]
    fn the_active_line_gate_matches_the_long_note_animator() {
        // The long-note animator only animates a word whose line is playing.
        // Asserting it here keeps the gate in one place: if the timeline ever
        // reported a future line as playing, a held syllable would light early.
        let mut tl = Timeline::new(vec![row(0.0, 1000.0), row(5000.0, 6000.0)]);
        tl.sync(500.0, false);
        assert!(tl.is_playing(0));
        assert!(!tl.is_playing(1), "a future line is never playing");
        tl.sync(5500.0, false);
        assert!(!tl.is_playing(0));
        assert!(tl.is_playing(1));
    }

    #[test]
    fn seek_detector_distinguishes_drift_from_jumps() {
        let mut detector = SeekDetector::new();
        assert!(!detector.detect(1000.0, true, 0.0));
        assert!(!detector.detect(1050.0, true, 50.0));
        assert!(!detector.detect(1190.0, true, 100.0), "within 150ms tolerance");
        assert!(detector.detect(2000.0, true, 150.0), "forward jump detected");
        assert!(detector.detect(1500.0, true, 200.0), "backward jump detected");
        assert!(detector.detect(1700.0, false, 250.0), "paused jump > 150ms detected");
    }

    #[test]
    fn focus_controller_handles_interlude_bottom_and_suspended_browsing() {
        let mut fc = FocusController::new();
        let mut snap = TimelineSnapshot {
            active_interlude: Some(Interlude {
                start_ms: 1000.0,
                end_ms: 9000.0,
                anchor_line_index: 2,
            }),
            focus_on_interlude: true,
            ..TimelineSnapshot::default()
        };
        assert_eq!(
            fc.resolve(&snap, 5, false, true, true),
            FocalTarget::Interlude(2)
        );
        // While suspended during interlude, if interlude ends, focus hands off to anchor + 1.
        snap.focus_on_interlude = false;
        snap.active_interlude = None;
        assert_eq!(
            fc.resolve(&snap, 5, true, true, false),
            FocalTarget::Line(3)
        );
        // End of song focuses Bottom only when bottom content exists.
        snap.is_end_of_song = true;
        assert_eq!(fc.resolve(&snap, 5, false, true, false), FocalTarget::Bottom);
        assert_eq!(
            fc.resolve(&snap, 5, false, false, false),
            FocalTarget::Line(4)
        );
    }
}
