//! Transport control and seek.
//!
//! Every call is "try": a player is allowed to refuse, and a refused seek must
//! never be retried against a different session. [`seek_guard`] is the pure part
//! — deadline, identity, capability and clamp — so it is testable without a
//! live player; [`seek`] adds the WinRT re-read that makes a queued lyric click
//! unable to land on the next song.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Next = 0,
    Prev = 1,
    Play = 2,
    Pause = 3,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SeekRequest {
    pub request_id: i64,
    pub ms: i64,
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub deadline_unix_ms: i64,
}

/// The identity a seek must still match when it is finally applied.
#[derive(Clone, Debug, PartialEq)]
pub struct SeekGuard {
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// A user action targeted at exactly the song visible when it was clicked.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlRequest {
    pub action: Action,
    pub request_id: i64,
    pub identity: SeekGuard,
    pub deadline_unix_ms: i64,
}

pub fn control_guard(request: &ControlRequest, now: i64, current: &SeekGuard, enabled: bool) -> bool {
    enabled && now < request.deadline_unix_ms
        && !request.identity.app.is_empty() && !request.identity.title.is_empty()
        && request.identity == *current
}

impl SeekRequest {
    pub fn identity(&self) -> SeekGuard {
        SeekGuard {
            app: self.app.clone(),
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
        }
    }
}

/// The pure guard chain, returning the position to apply.
///
/// `None` means "do not call the player": the request expired, is negative,
/// overflows the 100 ns conversion, targets a different track, or the player
/// does not expose a writable position. `duration_ms <= 0` means "unknown", not
/// "zero length", so the requested position is kept.
pub fn seek_guard(
    request: &SeekRequest,
    now_ms: i64,
    current: &SeekGuard,
    position_enabled: bool,
    duration_ms: i64,
) -> Option<i64> {
    if now_ms >= request.deadline_unix_ms || request.ms < 0 {
        return None;
    }
    if request.ms > i64::MAX / 10_000 {
        return None;
    }
    if current != &request.identity() {
        return None;
    }
    if !position_enabled {
        return None;
    }
    Some(if duration_ms > 0 {
        request.ms.min(duration_ms)
    } else {
        request.ms
    })
}

#[cfg(windows)]
pub use winrt::{apply, apply_guarded, seek, seek_by_manager};

#[cfg(windows)]
mod winrt {
    use windows::Media::Control::GlobalSystemMediaTransportControlsSession;
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;

    use super::{Action, SeekGuard, SeekRequest, seek_guard};

    /// Applies a transport command. `false` means the player declined.
    pub fn apply(session: &GlobalSystemMediaTransportControlsSession, action: Action) -> bool {
        // `TryTogglePlayPause` is deliberately not used: a stale queued "pause"
        // arriving after the track changed on its own would start playback.
        let result = match action {
            Action::Next => session.TrySkipNextAsync(),
            Action::Prev => session.TrySkipPreviousAsync(),
            Action::Play => session.TryPlayAsync(),
            Action::Pause => session.TryPauseAsync(),
        };
        result.and_then(|op| op.join()).unwrap_or(false)
    }

    fn live_identity(session: &GlobalSystemMediaTransportControlsSession) -> Option<SeekGuard> {
        let app = session
            .SourceAppUserModelId()
            .map(|h| h.to_string())
            .unwrap_or_default();
        let props = session
            .TryGetMediaPropertiesAsync()
            .and_then(|op| op.join())
            .ok()?;
        Some(SeekGuard {
            app,
            title: props.Title().map(|h| h.to_string()).unwrap_or_default(),
            artist: props.Artist().map(|h| h.to_string()).unwrap_or_default(),
            album: props.AlbumTitle().map(|h| h.to_string()).unwrap_or_default(),
        })
    }

    /// Resolve the exact visible identity, never fall back to another player.
    pub fn apply_guarded(mgr: &GlobalSystemMediaTransportControlsSessionManager, request: &super::ControlRequest) -> bool {
        let guard_started = std::time::Instant::now();
        if super::super::now_ms() >= request.deadline_unix_ms { return false; }
        let Ok(sessions) = mgr.GetSessions() else { return false; };
        let mut target = None;
        for i in 0..sessions.Size().unwrap_or(0) {
            let Ok(session) = sessions.GetAt(i) else { continue; };
            if session.SourceAppUserModelId().map(|id| id.to_string()).unwrap_or_default() != request.identity.app { continue; }
            if live_identity(&session).as_ref() == Some(&request.identity) {
                if target.is_some() { return false; } // ambiguous duplicate sessions
                target = Some(session);
            }
        }
        let Some(session) = target else { return false; };
        let Some(identity) = live_identity(&session) else { return false; };
        let controls = session.GetPlaybackInfo().and_then(|info| info.Controls());
        let enabled = controls.and_then(|c| match request.action {
            Action::Next => c.IsNextEnabled(),
            Action::Prev => c.IsPreviousEnabled(),
            Action::Play => match c.IsPlayEnabled() { Ok(true) => Ok(true), _ => c.IsPlayPauseToggleEnabled() },
            Action::Pause => match c.IsPauseEnabled() { Ok(true) => Ok(true), _ => c.IsPlayPauseToggleEnabled() },
        }).unwrap_or(false);
        if !super::control_guard(request, super::super::now_ms(), &identity, enabled) { return false; }
        log::info!("[smtc-control] phase=guard request_id={} guard_ms={}", request.request_id, guard_started.elapsed().as_millis());
        let action_started = std::time::Instant::now();
        let accepted = apply(&session, request.action);
        log::info!("[smtc-control] phase=action request_id={} accepted={} action_ms={}", request.request_id, accepted, action_started.elapsed().as_millis());
        accepted
    }

    /// Applies a seek against the live manager, refusing anything that no longer
    /// matches the request's identity. Returns whether the player accepted.
    pub fn seek_by_manager(
        mgr: &GlobalSystemMediaTransportControlsSessionManager,
        request: &SeekRequest,
    ) -> bool {
        // Re-read from the live manager: `session` was captured when the command
        // was enqueued, and the user may have switched players since.
        let Ok(sessions) = mgr.GetSessions() else {
            return false;
        };
        let mut current = None;
        for i in 0..sessions.Size().unwrap_or(0) {
            let Ok(s) = sessions.GetAt(i) else { continue; };
            if s.SourceAppUserModelId().map(|id| id.to_string()).unwrap_or_default() != request.app {
                continue;
            }
            if live_identity(&s).as_ref() == Some(&request.identity()) {
                current = Some(s);
                break;
            }
        }
        let Some(current) = current else {
            return false;
        };
        let Some(identity) = live_identity(&current) else {
            return false;
        };
        if identity != request.identity() {
            return false;
        }
        let enabled = current
            .GetPlaybackInfo()
            .and_then(|i| i.Controls())
            .and_then(|c| c.IsPlaybackPositionEnabled())
            .unwrap_or(false);
        let duration = current
            .GetTimelineProperties()
            .and_then(|tl| tl.EndTime())
            .map(|t| t.Duration / 10_000)
            .unwrap_or(0);
        let Some(target) = seek_guard(request, super::super::now_ms(), &identity, enabled, duration)
        else {
            return false;
        };
        current
            .TryChangePlaybackPositionAsync(target * 10_000)
            .and_then(|op| op.join())
            .unwrap_or(false)
    }

    pub fn seek(
        mgr: &GlobalSystemMediaTransportControlsSessionManager,
        _session: &GlobalSystemMediaTransportControlsSession,
        request: &SeekRequest,
    ) -> bool {
        seek_by_manager(mgr, request)
    }
}
