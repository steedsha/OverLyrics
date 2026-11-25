//! The chrome clock, ported from `tests/render/player-clock.test.ts`.
//!
//! The window keeps two clocks on purpose. The adapter's [`TransportClock`] is
//! frame-accurate and drives the word mask; this one is coarse, is allowed to
//! lag, and only feeds the progress bar and the time labels. They read the same
//! packet, so a regression in one is invisible in the other.
//!
//! Every case below is the reference test's own input, verbatim: the packet is
//! `positionMs 45000`, `durationMs 180000`, `lastUpdatedUnixMs 10000`, playing.
//! Keeping the numbers identical is what makes this a parity test instead of a
//! second, independently invented spec.

use oly_contracts::PlayerPacket;
use oly_core::TransportClock;

/// The reference fixture packet: playing, one minute in, three minutes long.
fn packet() -> PlayerPacket {
    let json = r#"{
        "type": "state",
        "idle": false,
        "app": "test",
        "title": "test",
        "artist": "",
        "album": "",
        "albumArtist": "",
        "trackNumber": 0,
        "status": 4,
        "rate": 1,
        "durationMs": 180000,
        "positionMs": 45000,
        "startMs": 0,
        "lastUpdatedUnixMs": 10000,
        "positionAvailable": true,
        "timelineSource": "smtc",
        "canSeek": true,
        "ctrl": { "playpause": true, "prev": true, "next": true },
        "trackChanged": false
    }"#;
    serde_json::from_str(json).expect("reference packet")
}

/// Feed one packet and read the chrome clock at `now`, the way the window does
/// between heartbeats.
fn chrome(packet: PlayerPacket, now_ms: f64) -> f64 {
    let mut clock = TransportClock::new();
    clock.feed(Some(&packet), 0.0);
    clock.chrome_position(now_ms)
}

#[test]
fn a_paused_clock_stops_extrapolating_even_when_the_timestamp_is_old() {
    // Two seconds have passed since `lastUpdatedUnixMs`, but the source says
    // paused: the bar must sit still rather than creep forward.
    let mut paused = packet();
    paused.status = Some(oly_contracts::PlaybackStatus::Code(5));
    assert_eq!(chrome(paused, 20000.0), 45000.0);
}

#[test]
fn a_playing_clock_advances_by_rate_and_never_passes_the_duration() {
    let mut fast = packet();
    fast.rate = 2.0;
    // 2 s elapsed at rate 2 = 4 000 ms on top of 45 000.
    assert_eq!(chrome(fast, 12000.0), 49000.0);
    // A stale timestamp on a playing source must not invent time past the end.
    assert_eq!(chrome(packet(), 1_000_000.0), 180000.0);
}

#[test]
fn an_unavailable_timeline_reads_zero_rather_than_the_last_known_position() {
    // `Some(false)` means the source has metadata but no timeline. The chrome
    // must not show a frozen 45 s as if it were real progress.
    let mut unavailable = packet();
    unavailable.position_available = Some(false);
    assert_eq!(chrome(unavailable, 20000.0), 0.0);
}

#[test]
fn the_two_clocks_disagree_on_purpose() {
    // Same packet, same instant: the mask clock extrapolates from the anchor,
    // the chrome clock happens to agree here only because both start at the
    // packet position. After a stall they diverge, which is why the window
    // never feeds one from the other.
    let mut clock = TransportClock::new();
    clock.feed(Some(&packet()), 10000.0);
    let mask = clock.lyric_position(10000.0);
    let chrome = clock.chrome_position(10000.0);
    assert!(
        (mask - 45000.0).abs() < 1e-9,
        "mask clock at the sample instant should report the sample, got {mask}"
    );
    assert_eq!(chrome, 45000.0);
}
