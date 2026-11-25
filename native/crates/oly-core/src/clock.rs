//! Port of `src/shared/progress-clock.ts`.
//!
//! Both progress bars and the karaoke mask read this one clock, so the anchor
//! rules live here and nowhere else. Times are `f64` milliseconds because the
//! reference is JavaScript: fractional line starts produced by the optimiser
//! are compared against the clock directly, and rounding at this layer shows
//! up as a one-frame mask slip on a 240 Hz display.
//!
//! The behaviour that matters for fidelity, in the order the rules apply:
//!
//! * a repeated coarse sample never re-anchors — sources refresh their
//!   timestamp without advancing `positionMs`, and re-anchoring those made the
//!   lyrics pause for a frame every third packet;
//! * idle or unavailable clears the position instead of freezing it;
//! * the first Playing packet after a pause is anchored at "now", because it
//!   often still carries the pre-pause timestamp and the paused time is not
//!   playback time;
//! * a sample up to 250 ms behind the running clock is jitter and is
//!   suppressed, while anything larger is a real correction and is applied —
//!   otherwise one bad anchor stays ahead for the rest of the track.

use oly_contracts::ClockState;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgressClock {
    /// Position at [`Self::at`].
    pub pos: f64,
    /// Monotonic anchor time, milliseconds.
    pub at: f64,
    pub playing: bool,
    pub rate: f64,
    pub dur: f64,
    pub idle: bool,
    pub available: bool,
    pub track: Option<String>,
    pub source: Option<String>,
    /// Raw sample identity, used only to detect a repeated sample.
    pub sample_pos: Option<f64>,
}

/// What one anchoring pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnchorOutcome {
    /// Position the clock reports at the anchor instant.
    pub projected: f64,
    /// The sample repeated an already-anchored one, so the clock kept running.
    pub duplicate: bool,
    /// Idle or position-unavailable cleared the clock.
    pub cleared: bool,
}

impl ProgressClock {
    pub fn new() -> Self {
        Self {
            rate: 1.0,
            ..Default::default()
        }
    }

    /// Position at `now_ms`, clamped to `[0, dur]` when a duration is known.
    pub fn position(&self, now_ms: f64) -> f64 {
        let mut pos = self.pos;
        if self.playing {
            pos += (now_ms - self.at).max(0.0) * self.rate;
        }
        if self.dur > 0.0 {
            pos.clamp(0.0, self.dur)
        } else {
            pos.max(0.0)
        }
    }

    /// Apply one state sample. `now_ms` is the wall clock in Unix milliseconds.
    pub fn anchor(&mut self, state: &ClockState, now_ms: f64) -> AnchorOutcome {
        let at = now_ms;
        let anchored = self.at > 0.0;
        let was_playing = self.playing;
        let previous_rate = self.rate;
        // Deliberately evaluated against the *previous* duration: the reference
        // computes `current` before it stores the new state, so a packet that
        // both changes the duration and re-anchors still projects with the old
        // clamp for this pass.
        let current = if anchored { self.position(at) } else { 0.0 };

        let key = state.track_key.clone().unwrap_or_else(|| {
            format!(
                "{}\u{1}{}\u{1}{}",
                state.app.as_deref().unwrap_or(""),
                state.title.as_deref().unwrap_or(""),
                state.artist.as_deref().unwrap_or("")
            )
        });
        let changed = state.track_changed
            || state.position_discontinuity
            || (self.track.is_some() && self.track.as_deref() != Some(key.as_str()));

        let playing = !state.idle
            && state.position_available != Some(false)
            && state.status.as_ref().is_some_and(|s| s.is_playing());
        let rate = match state.rate {
            Some(rate) if rate > 0.0 => rate,
            _ => 1.0,
        };
        let p = state.position_ms.filter(|v| v.is_finite());
        let sample = match state.sample_position_ms {
            Some(explicit) => explicit,
            None => p,
        };
        let same_sample = self.sample_pos == sample;
        let same_source = self.source.as_deref() == state.timeline_source.as_deref();
        let duplicate = anchored
            && !changed
            && same_source
            && same_sample
            && was_playing == playing
            && previous_rate == rate;

        self.idle = state.idle;
        self.available = state.position_available != Some(false);
        self.dur = state.duration_ms.unwrap_or(0.0).max(0.0);
        self.rate = rate;
        self.playing = playing;
        self.track = Some(key);
        self.source = state.timeline_source.clone();

        if state.idle || !self.available {
            self.pos = 0.0;
            self.at = at;
            self.sample_pos = None;
            return AnchorOutcome {
                projected: 0.0,
                duplicate: false,
                cleared: true,
            };
        }
        if duplicate {
            return AnchorOutcome {
                projected: current,
                duplicate: true,
                cleared: false,
            };
        }

        let mut sampled_at = match state.last_updated_unix_ms {
            Some(stamp) if stamp.is_finite() && stamp > 0.0 => at.min(stamp),
            _ => at,
        };
        // The first Playing packet can retain the pre-pause timestamp. Time
        // spent paused is not playback time.
        if anchored && !changed && !was_playing && playing {
            sampled_at = at;
        }
        let mut incoming = match p {
            None => {
                if changed {
                    0.0
                } else {
                    current
                }
            }
            Some(position) => position + if playing { (at - sampled_at) * rate } else { 0.0 },
        };
        // Status and timeline notifications arrive independently. Freeze or
        // resume the displayed clock while the source still reports its
        // previous sample.
        if anchored
            && !changed
            && same_source
            && (!was_playing || !playing)
            && (same_sample || (incoming - current).abs() <= 1000.0)
        {
            incoming = current;
        }
        // Suppress only small jitter; larger errors are corrected even when the
        // sample moves forward.
        if anchored
            && !changed
            && was_playing
            && playing
            && current > incoming
            && current - incoming <= 250.0
        {
            incoming = current;
        }

        self.pos = if self.dur > 0.0 {
            incoming.clamp(0.0, self.dur)
        } else {
            incoming.max(0.0)
        };
        self.at = at;
        self.sample_pos = sample;

        AnchorOutcome {
            projected: self.position(at),
            duplicate: false,
            cleared: false,
        }
    }

    /// Reset to the state of a clock that has never been anchored.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}
