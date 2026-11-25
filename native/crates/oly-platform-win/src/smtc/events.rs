//! The wire shape of a state packet, plus the throttling state that decides when
//! a packet is worth sending.
//!
//! Field names and units are a contract: the renderer extrapolates its own clock
//! from `positionMs` + `lastUpdatedUnixMs` + `rate`, and the lyric adapter reads
//! `status === 4` for "playing". Neither may drift.

use oly_contracts::{PlaybackStatus, PlayerPacket};

use super::session;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    pub next: bool,
    pub prev: bool,
    pub playpause: bool,
}

/// Everything the worker knows about the followed session.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track_number: i32,
    /// Integer, not a name: `4` is `Playing`.
    pub status: i32,
    pub rate: f64,
    pub duration_ms: i64,
    pub position_ms: i64,
    pub start_ms: i64,
    pub last_updated_unix_ms: i64,
    pub position_available: bool,
    pub can_seek: bool,
    pub ctrl: Controls,
    pub track_changed: bool,
    /// `None` omits the field; `Some(None)` sends an explicit null. The
    /// renderer clears artwork on `null`, and re-serialising a cached 100 KB
    /// data URL on every sample is what the omission avoids.
    pub cover: Option<Option<String>>,
}

impl PlayerState {
    /// The idle packet: `{"type":"state","idle":true}` and nothing else.
    pub fn into_packet(self, track_changed: bool) -> PlayerPacket {
        let _ = track_changed;
        PlayerPacket {
            album: self.album,
            album_artist: self.album_artist,
            app: self.app,
            artist: self.artist,
            can_seek: self.can_seek,
            cover_updated: self.cover.is_some(),
            cover: self.cover.and_then(|c| c),
            ctrl: oly_contracts::TransportControls {
                next: self.ctrl.next,
                prev: self.ctrl.prev,
                playpause: self.ctrl.playpause,
            },
            duration_ms: self.duration_ms as f64,
            idle: false,
            last_updated_unix_ms: self.last_updated_unix_ms as f64,
            position_available: Some(self.position_available),
            position_discontinuity: false,
            position_ms: self.position_ms as f64,
            rate: self.rate,
            start_ms: self.start_ms as f64,
            status: Some(PlaybackStatus::Code(self.status as i64)),
            timeline_source: if self.position_available {
                "smtc".into()
            } else {
                "unavailable".into()
            },
            title: self.title,
            track_changed: self.track_changed,
            track_number: self.track_number as i64,
            kind: "state".into(),
        }
    }
}

/// The comparison state that makes an unchanged 500 ms sample a no-op.
#[derive(Clone, Debug, Default)]
pub struct Dedup {
    pub last_key: String,
    pub last_status: Option<i32>,
    pub last_emit_tick: i64,
    pub cover_cache: Option<String>,
    pub last_sent_cover: Option<String>,
}

impl Dedup {
    pub fn clear(&mut self) {
        self.last_key = String::new();
        self.last_status = None;
        self.cover_cache = None;
        self.last_sent_cover = None;
    }

    /// True when a pure position sample arrives too soon to be worth a packet.
    /// Everything semantic (`force`, track change, status change, a refreshed
    /// cover, leaving idle) bypasses this.
    pub fn should_throttle(
        &self,
        force: bool,
        track_changed: bool,
        status_changed: bool,
        refresh_cover: bool,
        now_ms: i64,
        interval_ms: i64,
    ) -> bool {
        !force
            && !track_changed
            && !status_changed
            && !refresh_cover
            && self.last_key != session::IDLE_KEY
            && now_ms - self.last_emit_tick < interval_ms
    }
}

/// Serialises every emit attempt.
///
/// Two producers (the poll loop and a WinRT callback-driven refresh) can reach
/// the emit path at once. Without the gate they race on `last_key` and one of
/// them loses a track change — visible as lyrics that keep showing the previous
/// song.
#[derive(Clone, Default)]
pub struct EmitGate {
    inner: std::sync::Arc<std::sync::Mutex<()>>,
}

impl EmitGate {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}
