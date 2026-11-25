//! The one clock the whole window reads, ported from `adapter.ts`.
//!
//! The reference keeps two representations of the same playhead, and the
//! duplication is load-bearing rather than accidental:
//!
//!   * `mediaClock` (a [`ProgressClock`]) is the *anchor*: it absorbs every
//!     source sample and decides what the incoming sample means.
//!   * `anchor_pos` / `anchor_at` is the *displayed* position: it is what the
//!     lyric renderer seeks to, and it is re-anchored only when a sample passes
//!     the tolerance gate.
//!
//! A sample that lands inside the tolerance updates the anchor but deliberately
//! leaves the displayed position alone. Collapsing the two would make one bad
//! Apple sample rewind the word mask, which is the failure this file exists to
//! prevent.
//!
//! Three properties are asserted by `transport_scenarios` and by the browser
//! replay in `native/crates/oly-core/tests/transport_parity.rs`:
//!
//!   * paused time never plays back as a fast-forward, so resuming with a stale
//!     sample leaves the playhead where the pause froze it;
//!   * a seek still moves the playhead, even though it arrives as a position
//!     that is "behind" the running clock;
//!   * a silent playing source stalls after three seconds instead of drifting
//!     past the audio.
//!
//! All times are one monotonic base, in milliseconds. The reference reads
//! `performance.now()` for the anchor and `Date.now()` for packet arrival, but
//! it never subtracts one base from the other — so a caller that supplies a
//! single monotonic base gets identical behaviour, and can replay a recorded
//! trace at any speed.

use oly_contracts::{ClockState, PlayerPacket, PlaybackStatus};

use crate::clock::{AnchorOutcome, ProgressClock};
use crate::mapping::{
    DEFAULT_PROFILE, FRAME_DT_CLAMP_MS, STATUS_OVERRIDE_MS, STALL_NOPKT_MS, SyncProfile,
    is_playing, sync_profile_for,
};

/// What one [`TransportClock::feed`] pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FeedOutcome {
    /// The playhead the renderer should seek to, in lyric time.
    pub projected: f64,
    /// The sample was inside the tolerance and the displayed position did not
    /// move. This is the state Apple reports most of the time.
    pub held: bool,
    /// The renderer should perform a real seek: the playhead moved far enough,
    /// or the sample was discontinuous.
    pub seek_pending: bool,
    /// Consecutive packets carrying the same coarse position.
    pub same_pos_packets: u32,
    /// The packet was suppressed by a local preview toggle.
    pub overridden: bool,
}

/// One frame's worth of bookkeeping.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameTick {
    /// Milliseconds since the previous frame, clamped to `FRAME_DT_CLAMP_MS`.
    /// Zero on the first frame, and after the window was hidden.
    pub dt_ms: f64,
    /// Nominally playing, but no packet for `STALL_NOPKT_MS`.
    pub stalled: bool,
    /// True on the frame the stall began.
    pub became_stalled: bool,
    /// The renderer should seek this frame rather than scrub.
    ///
    /// The reference hands `seekPending` to `setCurrentTime` and then clears it
    /// on every frame, so the flag lives for exactly one frame. Clearing it here
    /// rather than in the caller keeps the two implementations on the same
    /// timeline: a packet that arrived between frames still forces one seek, and
    /// no later frame can act on a seek that has already been served.
    pub seek: bool,
}

/// The last coarse sample, used only to decide whether a packet is new.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LastPacket {
    pub pos: Option<f64>,
    pub status: Option<PlaybackStatus>,
}

/// A telemetry snapshot in the same shape as the reference's `debug()`.
///
/// The field names are the reference's because the Task 5 golden records them
/// verbatim; renaming them here would make the diff unreadable.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransportSnapshot {
    pub pos_ms: f64,
    pub have_pos: bool,
    pub sync_offset_ms: f64,
    pub song_offset_ms: f64,
    pub stalled: bool,
    pub seek_pending: bool,
    pub same_pos_pkts: u32,
    pub pos_since_move_ms: f64,
    pub status_override: Option<PlaybackStatus>,
    pub smtc_status: Option<PlaybackStatus>,
}

/// Playhead, offsets, and the sample gate — the whole of `adapter.ts`'s clock.
#[derive(Clone, Debug)]
pub struct TransportClock {
    clock: ProgressClock,
    /// Displayed playhead at `anchor_at`.
    anchor_pos: f64,
    anchor_at: f64,
    have_pos: bool,
    /// Position the last accepted sample projected, in lyric time.
    song_pos: f64,
    last_pkt: LastPacket,
    same_pos_pkts: u32,
    /// `None` until the first packet arrives. The reference uses `0` as a
    /// sentinel and then treats `lastPacketAt > 0` as "has arrived", which
    /// silently disables stall detection on a monotonic clock that starts at
    /// zero.
    last_packet_at: Option<f64>,
    stalled: bool,
    status_override: Option<PlaybackStatus>,
    override_at: f64,
    seek_pending: bool,
    profile: SyncProfile,
    sync_offset_ms: f64,
    song_offset_ms: f64,
    /// Mirrors `state` in the reference so `local_pos` can read the rate and
    /// duration without the caller re-supplying them on every frame.
    state: Option<ClockState>,
    playing_status: Option<PlaybackStatus>,
    pending_forward_spike: Option<f64>,
}

impl Default for TransportClock {
    fn default() -> Self {
        Self {
            clock: ProgressClock::new(),
            anchor_pos: 0.0,
            anchor_at: 0.0,
            have_pos: false,
            song_pos: 0.0,
            last_pkt: LastPacket::default(),
            same_pos_pkts: 0,
            last_packet_at: None,
            stalled: false,
            status_override: None,
            override_at: 0.0,
            seek_pending: false,
            profile: DEFAULT_PROFILE,
            sync_offset_ms: 0.0,
            song_offset_ms: 0.0,
            state: None,
            playing_status: None,
            pending_forward_spike: None,
        }
    }
}

impl TransportClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clock(&self) -> &ProgressClock {
        &self.clock
    }

    pub fn profile(&self) -> SyncProfile {
        self.profile
    }

    pub fn sync_offset_ms(&self) -> f64 {
        self.sync_offset_ms
    }

    pub fn song_offset_ms(&self) -> f64 {
        self.song_offset_ms
    }

    /// The user's manual lyric delay. It shifts the mapping, not the clock.
    pub fn set_song_offset(&mut self, ms: f64) {
        self.song_offset_ms = if ms.is_finite() { ms } else { 0.0 };
        // `setSongOffset` in the reference only stores the value; the next
        // `tick` maps it. No re-anchor, so the mask does not jump.
    }

    /// SMTC position → lyric position.
    pub fn to_lyric_time(&self, smtc_pos: f64) -> f64 {
        smtc_pos + self.sync_offset_ms + self.song_offset_ms
    }

    /// Lyric position → SMTC position, for seeking the player back.
    pub fn from_lyric_time(&self, lyric_ms: f64) -> f64 {
        lyric_ms - self.sync_offset_ms - self.song_offset_ms
    }

    /// The displayed playhead in SMTC space, before offsets.
    ///
    /// Unlike [`ProgressClock::position`], this clamps to `duration + 4000`
    /// rather than the duration: the last line of a track often outlives its
    /// declared duration, and clamping to the duration hid the tail of it.
    pub fn local_pos(&self) -> f64 {
        self.local_pos_at(0.0)
    }

    /// `local_pos` with an explicit clock reading, for replaying a trace.
    pub fn local_pos_at(&self, now_ms: f64) -> f64 {
        let now = if now_ms == 0.0 { self.anchor_at } else { now_ms };
        self.local_pos_raw(now)
    }

    fn local_pos_raw(&self, now_ms: f64) -> f64 {
        if !self.have_pos {
            return 0.0;
        }
        let mut pos = self.anchor_pos;
        if !self.stalled && self.state.as_ref().is_some_and(|s| self.rate_playing(s)) {
            let rate = self.effective_rate();
            pos += (now_ms - self.anchor_at).max(0.0) * rate;
        }
        let duration = self
            .state
            .as_ref()
            .and_then(|s| s.duration_ms)
            .filter(|d| d.is_finite())
            .unwrap_or(0.0);
        if duration > 0.0 {
            pos = pos.clamp(0.0, duration + 4000.0);
        }
        pos
    }

    fn rate_playing(&self, state: &ClockState) -> bool {
        !state.idle
            && state.position_available != Some(false)
            && state
                .status
                .as_ref()
                .is_some_and(PlaybackStatus::is_playing)
    }

    fn effective_rate(&self) -> f64 {
        match self.state.as_ref().and_then(|s| s.rate) {
            Some(rate) if rate > 0.0 => rate,
            _ => 1.0,
        }
    }

    fn reanchor(&mut self, pos: f64, now_ms: f64) {
        self.anchor_pos = pos;
        self.anchor_at = now_ms;
        self.have_pos = true;
    }

    /// Force the next sample to be re-evaluated. `setDoc` and `setSongOffset`
    /// use this in the reference so a rebuild cannot inherit a stale "this is a
    /// duplicate" verdict.
    pub fn reset_transport_state(&mut self) {
        self.last_pkt = LastPacket::default();
        self.same_pos_pkts = 0;
    }

    /// A local preview toggle (`previewPlaying`). It wins over the source for
    /// at most [`STATUS_OVERRIDE_MS`], then the source takes back over.
    pub fn set_status_override(&mut self, status: Option<PlaybackStatus>, now_ms: f64) {
        self.override_at = if status.is_some() { now_ms } else { 0.0 };
        self.status_override = status;
    }

    /// `true` when the override suppressed the packet.
    fn override_wins(&mut self, status: Option<&PlaybackStatus>, now_ms: f64) -> bool {
        let Some(override_status) = self.status_override.as_ref() else {
            return false;
        };
        if status == Some(override_status) {
            // The source caught up with the preview: hand control back.
            self.status_override = None;
            return false;
        }
        if now_ms - self.override_at > STATUS_OVERRIDE_MS {
            self.status_override = None;
            return false;
        }
        true
    }

    /// Apply one state packet. `now_ms` is the same monotonic base passed to
    /// [`Self::tick`] and [`Self::local_pos_at`].
    ///
    /// The reference computes `before` *before* it stores the new state, so a
    /// packet whose arrival also changes the rate is still judged against the
    /// old extrapolation. This port keeps that order.
    pub fn feed(&mut self, packet: Option<&PlayerPacket>, now_ms: f64) -> FeedOutcome {
        let Some(packet) = packet else {
            return FeedOutcome::default();
        };
        let before = self.local_pos_raw(now_ms);

        // Position unavailable and idle both mean "there is no playhead". They
        // clear the anchor rather than freezing it: a frozen anchor would
        // repeat the last position forever, and the first packet after a
        // restart would then be judged a 250 ms jitter instead of a new track.
        if packet.position_available == Some(false) {
            self.have_pos = false;
            self.song_pos = 0.0;
            self.same_pos_pkts = 0;
            self.last_packet_at = Some(now_ms);
            self.pending_forward_spike = None;
            // `stalledNow` is deliberately left alone: the reference only
            // recomputes it in `tick`, and `positionAvailable === false` makes
            // that computation true on the next frame anyway. Clearing it here
            // would report "not stalled" for the one packet that proves the
            // playhead is gone.
            self.clock = ProgressClock::new();
            self.state = Some(ClockState::from(packet));
            return FeedOutcome::default();
        }
        if packet.idle {
            self.have_pos = false;
            self.song_pos = 0.0;
            self.status_override = None;
            self.override_at = 0.0;
            self.last_pkt = LastPacket::default();
            self.last_packet_at = None;
            self.same_pos_pkts = 0;
            self.stalled = false;
            self.pending_forward_spike = None;
            self.clock = ProgressClock::new();
            self.state = Some(ClockState::from(packet));
            self.reanchor(0.0, now_ms);
            self.have_pos = false;
            return FeedOutcome::default();
        }

        self.last_packet_at = Some(now_ms);
        self.profile = sync_profile_for(Some(packet.app.as_str()));
        self.sync_offset_ms = self.profile.offset_ms;

        let position = packet.position_ms;
        let status_changed = packet.status != self.last_pkt.status;

        if self.override_wins(packet.status.as_ref(), now_ms) {
            self.last_pkt = LastPacket {
                pos: Some(position),
                status: packet.status.clone(),
            };
            self.state = Some(ClockState::from(packet));
            return FeedOutcome {
                projected: self.to_lyric_time(self.local_pos_raw(now_ms)),
                held: true,
                overridden: true,
                same_pos_packets: self.same_pos_pkts,
                ..FeedOutcome::default()
            };
        }

        // A backwards sample without a discontinuity flag is still a real seek:
        // dragging the player back produces exactly this, and treating it as
        // jitter would leave the lyrics stranded at the old position.
        let prev_pos = self.last_pkt.pos;
        let baseline_pos = self.pending_forward_spike.or(prev_pos);
        let backwards = baseline_pos.is_some_and(|last| position < last);
        let changed = self.last_pkt.pos.is_none()
            || self.last_pkt.pos != Some(position)
            || packet.track_changed
            || status_changed;
        let first = !self.have_pos;
        self.last_pkt = LastPacket {
            pos: Some(position),
            status: packet.status.clone(),
        };

        if changed {
            self.same_pos_pkts = 0;
        } else {
            self.same_pos_pkts += 1;
        }

        let previous_track = self.clock.track.clone();
        let previous_source = self.clock.source.clone();
        let previous_rate = self.clock.rate;
        self.state = Some(ClockState::from(packet));
        let anchor: AnchorOutcome = self.clock.anchor(
            self.state.as_ref().expect("state just stored"),
            now_ms,
        );
        let projected = self.clock.position(now_ms);
        let clock_changed = previous_track != self.clock.track
            || previous_source != self.clock.source
            || previous_rate != self.clock.rate;

        // Inside the tolerance the anchor has moved but the *displayed* position
        // has not. Re-anchoring here is what produced Apple's per-packet mask
        // rewind; the reference measured it and this gate is the fix.
        let drift = projected - before;
        let steady_playback = !first
            && !packet.track_changed
            && !packet.position_discontinuity
            && !status_changed
            && !clock_changed
            && !backwards;
        if steady_playback && drift.abs() < self.profile.tolerance_ms {
            self.pending_forward_spike = None;
            return FeedOutcome {
                projected: self.to_lyric_time(self.local_pos_raw(now_ms)),
                held: true,
                seek_pending: self.seek_pending,
                same_pos_packets: self.same_pos_pkts,
                overridden: false,
            };
        }
        // Guard against a single-packet mid-song forward SMTC spike on Apple
        // Music (1000..2500 ms) that would otherwise lock the playhead ahead for
        // the rest of the track once the next packet returns inside tolerance.
        if steady_playback
            && self.profile.tolerance_ms > 0.0
            && drift >= self.profile.tolerance_ms
            && drift < 2500.0
        {
            if self.pending_forward_spike.is_none() {
                if changed && !anchor.duplicate {
                    self.pending_forward_spike = Some(prev_pos.unwrap_or(position));
                }
                return FeedOutcome {
                    projected: self.to_lyric_time(self.local_pos_raw(now_ms)),
                    held: true,
                    seek_pending: self.seek_pending,
                    same_pos_packets: self.same_pos_pkts,
                    overridden: false,
                };
            }
            if !changed || anchor.duplicate {
                return FeedOutcome {
                    projected: self.to_lyric_time(self.local_pos_raw(now_ms)),
                    held: true,
                    seek_pending: self.seek_pending,
                    same_pos_packets: self.same_pos_pkts,
                    overridden: false,
                };
            }
        }
        self.pending_forward_spike = None;

        self.song_pos = projected;
        if first
            || packet.track_changed
            || packet.position_discontinuity
            || status_changed
            || (projected - before).abs() >= self.profile.threshold_ms
        {
            self.seek_pending = true;
        }
        self.reanchor(projected, now_ms);
        let _ = anchor;
        FeedOutcome {
            projected: self.to_lyric_time(self.local_pos_raw(now_ms)),
            held: false,
            seek_pending: self.seek_pending,
            same_pos_packets: self.same_pos_pkts,
            overridden: false,
        }
    }

    /// One rendered frame.
    ///
    /// `last_frame_ms` is the caller's previous frame timestamp; pass `None` on
    /// the first frame and whenever the window becomes visible again. Returning
    /// to a hidden window must not fast-forward the springs by minutes' worth
    /// of frames, hence the clamp.
    pub fn tick(&mut self, now_ms: f64, last_frame_ms: Option<f64>) -> FrameTick {
        let dt_ms = match last_frame_ms {
            None => 0.0,
            Some(previous) => (now_ms - previous).clamp(0.0, FRAME_DT_CLAMP_MS),
        };

        let before_stall = self.local_pos_raw(now_ms);
        let was_stalled = self.stalled;
        let nominally_playing = self
            .state
            .as_ref()
            .is_some_and(|state| self.rate_playing(state));
        self.stalled = !nominally_playing
            || !self.have_pos
            || self
                .last_packet_at
                .is_some_and(|at| now_ms - at > STALL_NOPKT_MS);

        // Entering a stall freezes the playhead where it was, so the mask stops
        // instead of drifting past the audio.
        if self.stalled && !was_stalled && self.have_pos {
            self.reanchor(before_stall, now_ms);
        }

        let seek = self.seek_pending;
        self.seek_pending = false;

        FrameTick {
            dt_ms,
            stalled: self.stalled,
            became_stalled: self.stalled && !was_stalled,
            seek,
        }
    }

    /// The playhead the renderer should use, in lyric time.
    pub fn lyric_position(&self, now_ms: f64) -> f64 {
        self.to_lyric_time(self.local_pos_raw(now_ms))
    }

    /// Clear `seek_pending` once the renderer has acted on it.
    pub fn take_seek_pending(&mut self) -> bool {
        let pending = self.seek_pending;
        self.seek_pending = false;
        pending
    }

    pub fn snapshot(&self, now_ms: f64) -> TransportSnapshot {
        TransportSnapshot {
            pos_ms: self.lyric_position(now_ms).round(),
            have_pos: self.have_pos,
            sync_offset_ms: self.sync_offset_ms,
            song_offset_ms: self.song_offset_ms,
            stalled: self.stalled,
            seek_pending: self.seek_pending,
            same_pos_pkts: self.same_pos_pkts,
            pos_since_move_ms: match self.last_packet_at {
                Some(at) => now_ms - at,
                None => -1.0,
            },
            status_override: self.status_override.clone(),
            smtc_status: self.state.as_ref().and_then(|s| s.status.clone()),
        }
    }

    /// The status the renderer should treat as authoritative, applying a live
    /// preview override on top of the source.
    pub fn effective_status(&self) -> Option<PlaybackStatus> {
        self.status_override
            .clone()
            .or_else(|| self.state.as_ref().and_then(|s| s.status.clone()))
    }

    /// True when the lyric renderer should run its animations.
    pub fn is_animating(&self) -> bool {
        !self.stalled
            && self
                .state
                .as_ref()
                .is_some_and(|state| self.rate_playing(state))
    }

    /// The song clock the chrome reads. Kept separate from `local_pos`
    /// deliberately: the progress bar is allowed to lag by a quarter second,
    /// the mask is not.
    pub fn chrome_position(&self, now_ms: f64) -> f64 {
        let Some(state) = self.state.as_ref() else {
            return 0.0;
        };
        if state.position_available == Some(false) || state.idle {
            return 0.0;
        }
        let rate = state.rate.filter(|r| *r > 0.0).unwrap_or(1.0);
        let elapsed = if state.status.as_ref().is_some_and(PlaybackStatus::is_playing) {
            (now_ms - state.last_updated_unix_ms.unwrap_or(now_ms)).max(0.0) * rate
        } else {
            0.0
        };
        let raw = state.position_ms.unwrap_or(0.0) + elapsed;
        match state.duration_ms {
            Some(duration) if duration > 0.0 => raw.min(duration),
            _ => raw.max(0.0),
        }
    }

    pub fn playing_status(&self) -> Option<&PlaybackStatus> {
        self.playing_status.as_ref()
    }
}

/// Whether a packet's status means "playing", exported so callers do not each
/// re-derive the code-4 check.
pub fn packet_is_playing(packet: &PlayerPacket) -> bool {
    is_playing(packet.status.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apple_packet(pos_ms: f64, at_ms: f64) -> PlayerPacket {
        PlayerPacket {
            app: "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            duration_ms: 200_000.0,
            position_ms: pos_ms,
            last_updated_unix_ms: at_ms,
            position_available: Some(true),
            rate: 1.0,
            status: Some(PlaybackStatus::Code(4)),
            timeline_source: "smtc".into(),
            ..Default::default()
        }
    }

    #[test]
    fn apple_single_jitter_inside_tolerance_is_still_held() {
        let mut clock = TransportClock::new();
        let base = 1_000_000.0;
        clock.feed(Some(&apple_packet(10_000.0, base)), base);
        let jitter = clock.feed(Some(&apple_packet(10_400.0, base + 1_000.0)), base + 1_000.0);
        assert!(jitter.held, "sub-1000ms Apple drift must stay held mid-song");
        let jitter2 = clock.feed(Some(&apple_packet(11_400.0, base + 2_000.0)), base + 2_000.0);
        assert!(jitter2.held);
        let jitter3 = clock.feed(Some(&apple_packet(12_400.0, base + 3_000.0)), base + 3_000.0);
        assert!(jitter3.held, "mid-song sub-tolerance drift must never jump the playhead");
        assert!((clock.local_pos_at(base + 3_000.0) - 13_000.0).abs() < 1e-6);
    }

    #[test]
    fn apple_single_mid_song_forward_spike_is_absorbed_while_real_shift_applies() {
        let mut clock = TransportClock::new();
        let base = 1_000_000.0;
        clock.feed(Some(&apple_packet(60_000.0, base)), base);

        // One spurious +1100 ms forward spike followed by normal samples must not jump.
        let spike = clock.feed(Some(&apple_packet(62_100.0, base + 1_000.0)), base + 1_000.0);
        assert!(spike.held, "single 1100ms forward spike must await confirmation");
        let normal = clock.feed(Some(&apple_packet(61_500.0, base + 1_500.0)), base + 1_500.0);
        assert!(normal.held);
        assert!((clock.local_pos_at(base + 1_500.0) - 61_500.0).abs() < 1e-6);

        // Two consecutive forward samples >= 1000 ms confirm a real seek/shift.
        let s1 = clock.feed(Some(&apple_packet(63_500.0, base + 2_000.0)), base + 2_000.0);
        assert!(s1.held);
        let s2 = clock.feed(Some(&apple_packet(64_000.0, base + 2_500.0)), base + 2_500.0);
        assert!(!s2.held);
        assert!((clock.local_pos_at(base + 2_500.0) - 64_000.0).abs() < 1e-6);
    }
}
