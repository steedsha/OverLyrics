//! Per-player compensation and adapter constants, ported from
//! `src/render/adapter.ts`.
//!
//! This module is deliberately small: it holds only the values and predicates
//! the clock in [`crate::transport`] needs. Keeping them separate means the
//! profile table can be asserted against the reference constants without
//! constructing a clock, and it keeps `transport.rs` readable.
//!
//! Three entries here are load-bearing and each has a measured failure behind
//! it:
//!
//!   * Apple reports its position about two seconds late, so its profile keeps
//!     a 1 s correction threshold *and* a 1 s clock tolerance. With tolerance
//!     `0` every packet re-anchors the lyric clock and the word mask rewinds.
//!   * Every other player keeps tolerance `0`. A tolerance of 300 ms (the
//!     obvious misreading of `SYNC_DEFAULT`) leaves the lyric clock parked up
//!     to 300 ms behind the audio; the reference measured a flat 30-40 ms and
//!     up to 154 ms of word-onset lag from exactly that mistake.
//!   * `FRAME_DT_CLAMP` stops a background window from fast-forwarding the
//!     springs by minutes worth of frames when it regains focus.

use oly_contracts::PlaybackStatus;

/// `GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing`.
pub const STATUS_PLAYING: i64 = 4;
/// No packet for this long while nominally playing means the clock is stalled.
pub const STALL_NOPKT_MS: f64 = 3000.0;
/// A frame delta is clamped so a hidden window cannot jump the springs.
pub const FRAME_DT_CLAMP_MS: f64 = 120.0;
/// Mode collapse fades the outgoing content for 100 ms first.
pub const MODE_COLLAPSE_DELAY_MS: f64 = 100.0;
/// A local preview toggle wins over the source for at most this long.
pub const STATUS_OVERRIDE_MS: f64 = 3000.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SyncProfile {
    /// Added to the source position to compensate a known reporting delay.
    pub offset_ms: f64,
    /// A correction at least this large is applied and requests a seek.
    pub threshold_ms: f64,
    /// A correction smaller than this is dropped entirely.
    pub tolerance_ms: f64,
}

pub const APPLE_PROFILE: SyncProfile = SyncProfile {
    offset_ms: 2000.0,
    threshold_ms: 1000.0,
    tolerance_ms: 1000.0,
};

pub const SPOTIFY_PROFILE: SyncProfile = SyncProfile {
    offset_ms: 0.0,
    threshold_ms: 300.0,
    tolerance_ms: 0.0,
};

/// `SYNC_DEFAULT` has no `clockToleranceMs`; the reference reads it as `|| 0`.
pub const DEFAULT_PROFILE: SyncProfile = SyncProfile {
    offset_ms: 0.0,
    threshold_ms: 300.0,
    tolerance_ms: 0.0,
};

/// Case-insensitive substring match, mirroring `/apple/i` in the reference.
pub fn sync_profile_for(app: Option<&str>) -> SyncProfile {
    let app = app.unwrap_or("").to_ascii_lowercase();
    if app.contains("apple") {
        APPLE_PROFILE
    } else if app.contains("spotify") {
        SPOTIFY_PROFILE
    } else {
        DEFAULT_PROFILE
    }
}

/// Stable, dot-free configuration key for per-player global sync offsets.
pub fn player_offset_key(app: Option<&str>) -> String {
    let trimmed = app.unwrap_or("").trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("fixture") || trimmed.eq_ignore_ascii_case("default") {
        return "default".to_string();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("apple") {
        return "apple".to_string();
    }
    if lower.contains("spotify") {
        return "spotify".to_string();
    }
    let before_bang = lower.split('!').next().unwrap_or(lower.as_str()).trim();
    let without_exe = before_bang.strip_suffix(".exe").unwrap_or(before_bang);
    let normalized = match without_exe.rsplit_once('_') {
        // WinRT AppUserModelIds end with a 13-character package family hash.
        Some((prefix, suffix))
            if prefix.contains('.')
                && suffix.len() == 13
                && suffix.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            prefix
        }
        _ => without_exe,
    };
    let clean: String = normalized
        .chars()
        .map(|c| match c {
            '.' | ' ' | '/' | '\\' => '_',
            _ => c,
        })
        .collect();
    if clean.is_empty() {
        "default".to_string()
    } else {
        clean
    }
}

/// Human-friendly display name for a [`player_offset_key`].
pub fn player_display_name(key: &str) -> String {
    match key {
        "apple" => "Apple Music".to_string(),
        "spotify" => "Spotify".to_string(),
        "qqmusic" => "QQ 音乐".to_string(),
        "cloudmusic" => "网易云音乐".to_string(),
        "kugou" => "酷狗音乐".to_string(),
        "kwmusic" => "酷我音乐".to_string(),
        "foobar2000" => "foobar2000".to_string(),
        "musicbee" => "MusicBee".to_string(),
        "potplayer" | "potplayermini" | "potplayermini64" => "PotPlayer".to_string(),
        "msedge" => "Microsoft Edge".to_string(),
        "chrome" => "Google Chrome".to_string(),
        "default" | "" => "其他播放器".to_string(),
        other => other.to_string(),
    }
}

/// The adapter's seek decision after a packet: apply and seek, or hold.
///
/// `discontinuity` is passed in rather than read from the packet: the Windows
/// bridge reports a seek as a separate signal, and the frozen trace has no such
/// field, so the adapter carries it alongside the packet.
pub fn seek_decision(
    first: bool,
    track_changed: bool,
    discontinuity: bool,
    status_changed: bool,
    projected: f64,
    before: f64,
    threshold_ms: f64,
) -> bool {
    first
        || track_changed
        || discontinuity
        || status_changed
        || (projected - before).abs() >= threshold_ms
}

/// `isPlaying(status)` from the reference: the integer code or its enum name.
pub fn is_playing(status: Option<&PlaybackStatus>) -> bool {
    status.is_some_and(PlaybackStatus::is_playing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_match_the_measured_compensation_table() {
        assert_eq!(sync_profile_for(Some("AppleInc.AppleMusic")), APPLE_PROFILE);
        assert_eq!(sync_profile_for(Some("Spotify.exe")), SPOTIFY_PROFILE);
        assert_eq!(sync_profile_for(Some("MSEdge")), DEFAULT_PROFILE);
        assert_eq!(sync_profile_for(None), DEFAULT_PROFILE);
        // The default tolerance is zero, not the threshold. Reading it as 300
        // is the mistake that parked the lyric clock behind the audio.
        assert_eq!(DEFAULT_PROFILE.tolerance_ms, 0.0);
        assert_eq!(DEFAULT_PROFILE.threshold_ms, 300.0);
        assert_eq!(APPLE_PROFILE.offset_ms, 2000.0);
        assert_eq!(APPLE_PROFILE.tolerance_ms, 1000.0);
        assert_eq!(SPOTIFY_PROFILE.offset_ms, 0.0);
        assert_eq!(SPOTIFY_PROFILE.tolerance_ms, 0.0);
    }

    #[test]
    fn the_profile_match_is_case_insensitive_and_substring_based() {
        assert_eq!(sync_profile_for(Some("APPLEMUSIC")), APPLE_PROFILE);
        assert_eq!(sync_profile_for(Some("Microsoft.ZuneMusic")), DEFAULT_PROFILE);
        assert_eq!(sync_profile_for(Some("spotify")), SPOTIFY_PROFILE);
    }

    #[test]
    fn only_status_four_and_its_name_mean_playing() {
        assert!(is_playing(Some(&PlaybackStatus::Code(STATUS_PLAYING))));
        assert!(is_playing(Some(&PlaybackStatus::Name("Playing".into()))));
        assert!(!is_playing(Some(&PlaybackStatus::Code(5))));
        assert!(!is_playing(Some(&PlaybackStatus::Name("Paused".into()))));
        assert!(!is_playing(None));
    }

    #[test]
    fn a_seek_is_requested_for_the_first_sample_and_for_real_corrections() {
        assert!(seek_decision(true, false, false, false, 0.0, 0.0, 300.0));
        assert!(!seek_decision(false, false, false, false, 100.0, 0.0, 300.0));
        assert!(seek_decision(false, false, false, false, 300.0, 0.0, 300.0));
        assert!(seek_decision(false, true, false, false, 0.0, 0.0, 300.0));
        assert!(seek_decision(false, false, true, false, 0.0, 0.0, 300.0));
        assert!(seek_decision(false, false, false, true, 0.0, 0.0, 300.0));
    }

    #[test]
    fn player_offset_key_normalizes_smtc_identifiers_without_dots() {
        assert_eq!(player_offset_key(Some("AppleInc.AppleMusicWin_nzyj5cx40ttqa!App")), "apple");
        assert_eq!(player_offset_key(Some("Spotify.exe")), "spotify");
        assert_eq!(player_offset_key(Some("QQMusic.exe")), "qqmusic");
        assert_eq!(player_offset_key(Some("cloudmusic.exe")), "cloudmusic");
        assert_eq!(player_offset_key(Some("Foo.Bar_1234567890abc!App")), "foo_bar");
        assert_eq!(player_offset_key(Some("fixture")), "default");
        assert_eq!(player_offset_key(None), "default");
        assert_eq!(player_display_name("apple"), "Apple Music");
        assert_eq!(player_display_name("qqmusic"), "QQ 音乐");
        assert_eq!(player_display_name("default"), "其他播放器");
    }
}
