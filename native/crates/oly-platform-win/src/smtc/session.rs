//! Which player owns the lyrics.
//!
//! Windows keeps its own notion of a "current" session, but it is sticky to the
//! last app that touched the transport controls, so a paused Spotify tab can
//! keep the lyrics while YouTube is actually playing. The rule below prefers the
//! session that is audibly playing, then the one we already selected, then the
//! most recently updated one. Selection is re-evaluated every poll against a
//! cheap snapshot: only `AUMID` / status / `LastUpdatedTime`, never an async
//! metadata read per candidate.

use std::cmp::Reverse;

/// `GlobalSystemMediaTransportControlsSessionPlaybackStatus::Closed`
pub const CLOSED: i32 = 0;
pub const OPENED: i32 = 1;
pub const CHANGING: i32 = 2;
pub const STOPPED: i32 = 3;
pub const PLAYING: i32 = 4;
pub const PAUSED: i32 = 5;

/// The sentinel the bridge writes when the session list goes empty. A real
/// AUMID can never contain a NUL, so this cannot collide with a track.
pub const IDLE_KEY: &str = "\u{0}idle";
/// Forces the next emit to bypass throttling (used by a refresh request).
pub const FORCE_KEY: &str = "\u{0}force";

pub fn status_name(status: i32) -> &'static str {
    match status {
        CLOSED => "Closed",
        OPENED => "Opened",
        CHANGING => "Changing",
        STOPPED => "Stopped",
        PLAYING => "Playing",
        PAUSED => "Paused",
        _ => "Unknown",
    }
}

/// Everything selection needs from one session, gathered with synchronous
/// getters only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub app: String,
    pub status: i32,
    pub last_updated_ms: i64,
    /// True when a previous tick sampled this AUMID's metadata, so we know it
    /// actually publishes a title. The first sighting scores as unknown rather
    /// than paying an async read for every candidate on every poll.
    pub known_title: bool,
}

/// Stable track identity. `\u{1}` cannot appear in a title, so the key cannot
/// collide across fields.
pub fn key_of(app: &str, title: &str, artist: &str, album: &str) -> String {
    format!("{app}\u{1}{title}\u{1}{artist}\u{1}{album}")
}

/// Ranking tuple: playing, sticky, has-title, `LastUpdatedTime`, AUMID tiebreak.
type Rank<'a> = (u8, u8, u8, i64, Reverse<&'a str>);

/// Index of the session to follow, or `None` when every candidate is closed or
/// nameless.
pub fn pick(cands: &[Candidate], sticky: Option<&str>) -> Option<usize> {
    let mut best: Option<(usize, Rank<'_>)> = None;
    for (i, c) in cands.iter().enumerate() {
        if c.status == CLOSED || c.app.is_empty() {
            continue;
        }
        let key = (
            u8::from(c.status == PLAYING),
            u8::from(sticky == Some(c.app.as_str())),
            u8::from(c.known_title),
            c.last_updated_ms,
            Reverse(c.app.as_str()),
        );
        match &best {
            Some((_, bk)) if *bk >= key => {}
            _ => best = Some((i, key)),
        }
    }
    best.map(|(i, _)| i)
}

/// Picks the user-pinned session when it is still alive (non-closed), else falls back to `pick`.
pub fn pick_with_pin(cands: &[Candidate], sticky: Option<&str>, pinned: Option<&str>) -> Option<usize> {
    if let Some(pin) = pinned {
        if let Some(idx) = cands
            .iter()
            .position(|c| c.status != CLOSED && !c.app.is_empty() && c.app == pin)
        {
            return Some(idx);
        }
    }
    pick(cands, sticky)
}
