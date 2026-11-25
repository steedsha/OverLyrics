//! Playback state as the platform layer reports it.
//!
//! `PlayerPacket` is the shape the SMTC bridge hands the renderer. It is
//! transcribed from `src/platform/ipc.ts` and the 742-packet trace frozen in
//! `native/tests/fixtures/packets/`, not inferred from the clock code: the
//! clock only reads a subset, and a port that modelled just that subset would
//! silently drop fields the tray, the picker and the debug HUD still need.
//!
//! Field names stay camelCase in JSON because the Windows bridge, the legacy
//! fixtures and the debug tooling all speak that shape. Renaming them at the
//! boundary would make every recorded packet a special case.

use serde::{Deserialize, Deserializer, Serialize};

/// `GlobalSystemMediaTransportControlsSessionPlaybackStatus`.
///
/// The .NET sidecar serialised the enum name; the Rust bridge serialises the
/// raw integer. Both reach the renderer depending on which host produced the
/// packet, so both are accepted here and compared through [`Self::is_playing`]
/// rather than at each call site.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlaybackStatus {
    /// Raw enum code: `4` is Playing.
    Code(i64),
    /// Enum name, e.g. `"Playing"`.
    Name(String),
}

impl PlaybackStatus {
    pub fn is_playing(&self) -> bool {
        match self {
            Self::Code(code) => *code == 4,
            Self::Name(name) => name == "Playing",
        }
    }

    pub fn is_closed(&self) -> bool {
        match self {
            Self::Code(code) => *code == 0,
            Self::Name(name) => name.eq_ignore_ascii_case("closed"),
        }
    }
}

/// Returns `true` when `title` is empty or a player idle placeholder such as
/// `"未在播放"` or `"Not Playing"`.
pub fn is_idle_track_title(title: &str) -> bool {
    let trimmed = title.trim();
    trimmed.is_empty()
        || matches!(
            trimmed,
            "未在播放"
                | "未播放"
                | "暂无播放"
                | "停止播放"
                | "当前未在播放"
                | "没有正在播放的媒体"
                | "没有正在播放的歌曲"
        )
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "not playing"
                | "nothing playing"
                | "nothing is playing"
                | "no media playing"
                | "no song playing"
        )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportControls {
    #[serde(default)]
    pub next: bool,
    #[serde(default)]
    pub playpause: bool,
    #[serde(default)]
    pub prev: bool,
}

/// One sample of the system media session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerPacket {
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub album_artist: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub can_seek: bool,
    /// Cover art as a local path or data URI; absent when the source has none.
    #[serde(default)]
    pub cover: Option<String>,
    /// Explicitly replaces artwork, including clearing it with None.
    /// False + None means an unchanged cover was omitted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cover_updated: bool,
    #[serde(default)]
    pub ctrl: TransportControls,
    #[serde(default)]
    pub duration_ms: f64,
    #[serde(default)]
    pub idle: bool,
    #[serde(default)]
    pub last_updated_unix_ms: f64,
    /// `Some(false)` means the source has metadata but no timeline.
    ///
    /// Absent is *not* the same as false: the reference tests
    /// `packet.positionAvailable === false`, so an idle packet that omits the
    /// field takes the idle branch rather than the no-timeline branch, and the
    /// two clear different state.
    #[serde(default)]
    pub position_available: Option<bool>,
    /// A local explicit seek, carried alongside the packet. This is separate
    /// from `track_changed`: a seek inside the same track reports a position
    /// that may move backwards, and the clock must treat it as real.
    #[serde(default)]
    pub position_discontinuity: bool,
    #[serde(default)]
    pub position_ms: f64,
    #[serde(default)]
    pub rate: f64,
    #[serde(default)]
    pub start_ms: f64,
    #[serde(default)]
    pub status: Option<PlaybackStatus>,
    #[serde(default)]
    pub timeline_source: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub track_changed: bool,
    #[serde(default)]
    pub track_number: i64,
    /// `"state"` for a normal sample; other kinds are diagnostics.
    #[serde(default, rename = "type")]
    pub kind: String,
}

fn is_false(value: &bool) -> bool { !*value }

impl PlayerPacket {
    /// The identity the clock uses to tell two tracks apart.
    ///
    /// Matches `trackKey || app \u0001 title \u0001 artist` from the legacy
    /// adapter; the separator is the unit separator, chosen so a title
    /// containing a dash or a comma cannot collide with another track.
    pub fn track_key(&self) -> String {
        format!("{}\u{1}{}\u{1}{}", self.app, self.title, self.artist)
    }

    pub fn is_playing(&self) -> bool {
        self.status.as_ref().is_some_and(PlaybackStatus::is_playing)
    }

    pub fn is_idle_state(&self) -> bool {
        self.idle
            || is_idle_track_title(&self.title)
            || self.status.as_ref().is_some_and(PlaybackStatus::is_closed)
    }
}

fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<f64>>, D::Error>
where
    D: Deserializer<'de>,
{
    // `None` means the field was absent; `Some(None)` means it was present and
    // null. The legacy clock distinguishes the two: an absent
    // `samplePositionMs` falls back to `positionMs`, while an explicit null is
    // a sample identity of null.
    Option::<f64>::deserialize(deserializer).map(Some)
}

/// The slice of a packet the progress clock consumes.
///
/// Kept separate from [`PlayerPacket`] so the clock can be replayed from the
/// frozen scenario fixtures (which record exactly these fields) without
/// inventing transport-control values, and so a caller can anchor a clock from
/// a synthetic state in a unit test.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockState {
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub track_key: Option<String>,
    #[serde(default)]
    pub track_changed: bool,
    #[serde(default)]
    pub idle: bool,
    #[serde(default)]
    pub position_available: Option<bool>,
    #[serde(default)]
    pub status: Option<PlaybackStatus>,
    #[serde(default)]
    pub rate: Option<f64>,
    #[serde(default)]
    pub position_ms: Option<f64>,
    #[serde(default)]
    pub duration_ms: Option<f64>,
    #[serde(default)]
    pub last_updated_unix_ms: Option<f64>,
    #[serde(default)]
    pub timeline_source: Option<String>,
    #[serde(default)]
    pub position_discontinuity: bool,
    /// `Some(None)` when the source explicitly reported a null sample.
    #[serde(default, deserialize_with = "double_option")]
    pub sample_position_ms: Option<Option<f64>>,
}

impl From<&PlayerPacket> for ClockState {
    fn from(packet: &PlayerPacket) -> Self {
        Self {
            app: Some(packet.app.clone()),
            title: Some(packet.title.clone()),
            artist: Some(packet.artist.clone()),
            album: Some(packet.album.clone()),
            track_key: Some(packet.track_key()),
            track_changed: packet.track_changed,
            idle: packet.idle,
            position_available: packet.position_available,
            status: packet.status.clone(),
            rate: Some(packet.rate),
            position_ms: Some(packet.position_ms),
            duration_ms: Some(packet.duration_ms),
            last_updated_unix_ms: Some(packet.last_updated_unix_ms),
            timeline_source: Some(packet.timeline_source.clone()),
            position_discontinuity: packet.position_discontinuity,
            sample_position_ms: None,
        }
    }
}
