//! Position arithmetic shared by every player.
//!
//! A track title is not evidence of a usable timeline: the same capability rule
//! applies to every player, and a zero-sample clock is reported as unavailable
//! rather than invented. The emitted state carries the *raw* sample
//! (`positionMs` + `lastUpdatedUnixMs`); the renderer keeps its own clock and
//! only re-anchors on a packet.

/// A pure position update is worth at most this much traffic; the renderer
/// extrapolates between packets, so a denser stream costs wakeups and buys
/// nothing.
pub const POSITION_INTERVAL_MS: i64 = 500;

/// Polling fallback. Events are not reliable on every player, so the loop also
/// samples on its own; 200 ms keeps a skip inside the 400 ms budget.
pub const POLL_FALLBACK_MS: u64 = 200;

pub fn is_available(position_ms: i64, duration_ms: i64, updated_ms: i64) -> bool {
    updated_ms > 0 && position_ms >= 0 && (duration_ms > 0 || position_ms > 0)
}

pub fn extrapolate(
    position_ms: i64,
    duration_ms: i64,
    updated_ms: i64,
    rate: f64,
    now_ms: i64,
) -> i64 {
    let rate = if rate > 0.0 { rate } else { 1.0 };
    let elapsed = (now_ms - updated_ms).max(0) as f64;
    let mut pos = position_ms as f64 + elapsed * rate;
    if duration_ms > 0 {
        pos = pos.min(duration_ms as f64);
    }
    if pos < 0.0 {
        0
    } else {
        pos as i64
    }
}

/// WinRT timestamps are 100 ns ticks since 1601-01-01; `lastUpdatedUnixMs` is
/// Unix milliseconds. Mixing the two would make every position sample look like
/// the future and freeze the renderer's clock.
pub fn filetime_to_unix_ms(ft: i64) -> i64 {
    const EPOCH_DIFF_MS: i64 = 11_644_473_600_000;
    if ft <= 0 {
        return 0;
    }
    ft / 10_000 - EPOCH_DIFF_MS
}

/// `TimeSpan` is also 100 ns ticks, but relative to zero.
pub fn ticks_to_ms(d: i64) -> i64 {
    d / 10_000
}
