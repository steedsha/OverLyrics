//! Manager acquisition with the reference bridge's retry ladder.
//!
//! `RequestAsync` fails while the shell's media services are still starting —
//! most often right after logon, and always right after `explorer.exe` restarts
//! (which rebuilds every SMTC session). The very first attempt gets the tight
//! 200 ms × 25 ladder because startup waits on it; after that the loop is
//! already running and a slower 2 s → 30 s backoff keeps an idle window quiet.

/// Attempts × interval for the first acquisition: 25 × 200 ms = 5 s.
pub const FIRST_ATTEMPTS: u32 = 25;
pub const FIRST_INTERVAL_MS: u64 = 200;

/// Ceiling of the exponential ladder used after the first success.
pub const MAX_BACKOFF_MS: u64 = 30_000;

/// 2 s, 4 s, 8 s, 16 s, 30 s, 30 s, …
pub fn backoff_ms(fail_streak: u32) -> u64 {
    let shift = fail_streak.min(4);
    (2_000u64 << shift).min(MAX_BACKOFF_MS)
}

#[cfg(windows)]
pub fn request() -> Result<
    windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager,
    String,
> {
    use std::time::Duration;

    let mut last = String::from("no attempt");
    for attempt in 0..FIRST_ATTEMPTS {
        match windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .and_then(|op| op.join())
        {
            Ok(manager) => return Ok(manager),
            Err(error) => {
                last = error.to_string();
                if attempt + 1 < FIRST_ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(FIRST_INTERVAL_MS));
                }
            }
        }
    }
    Err(last)
}
