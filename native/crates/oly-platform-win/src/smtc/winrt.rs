//! The WinRT half of the SMTC bridge.
//!
//! Everything that needs a live player lives here: one dedicated MTA thread owns
//! the session manager, polls it, and turns the followed session into
//! [`PlayerState`] packets. The arithmetic it uses — selection, throttling, the
//! seek guard — is in the sibling modules and is tested without a player.
//!
//! Two rules shape the loop:
//!
//! * exactly one worker exists per process, regardless of how many windows the
//!   shell opens (the reference bridge had the same constraint, expressed as a
//!   single Tauri app handle);
//! * every WinRT call happens on this thread. Core Audio and SMTC objects are
//!   apartment-affine, and touching one from a renderer thread yields
//!   `RPC_E_WRONG_THREAD`.
//!
//! Polling replaces the reference's `CurrentSessionChanged` / `SessionsChanged`
//! subscriptions. The reference already sampled every 200 ms as its fallback,
//! and transport requests interrupt that wait through the command queue.
//! Dropping subscriptions removes callbacks that can outlive their window.

// COM apartment entry has no safe binding: `CoInitializeEx`/`CoUninitialize`
// are process-global state changes that `windows` exposes raw. Everything else
// in this module is safe WinRT calls.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Foundation::{IReference, TypedEventHandler};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    MediaPropertiesChangedEventArgs,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use super::control::{Action, SeekRequest, apply, seek_by_manager};
use super::cover;
use super::events::{Controls, Dedup, PlayerState};
use super::session::{self, Candidate, IDLE_KEY, key_of};
use super::timeline::{
    POLL_FALLBACK_MS, POSITION_INTERVAL_MS, filetime_to_unix_ms, is_available, ticks_to_ms,
};
use super::{EventSink, Queue, VolumeEvent, now_ms};

/// One queued request for the worker.
pub enum Cmd {
    /// Apply a transport command; the sender receives the player's answer.
    Send(Action, Sender<bool>),
    Guarded(super::control::ControlRequest, Instant),
    /// Apply a guarded seek.
    Seek(SeekRequest),
    /// Force the next poll to bypass throttling and re-read the cover.
    Refresh,
    /// Pin (or unpin when `None`) a specific SMTC `SourceAppUserModelId`.
    SetPinned(Option<String>),
}

/// The shell's handle to the worker.
pub struct SmtcHandle {
    queue: Arc<Queue<Cmd>>,
    shutdown: Arc<AtomicBool>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl SmtcHandle {
    /// Starts the worker. Returns immediately; a machine with no media service
    /// simply never emits, which is the same state as "no player running".
    pub fn spawn(sink: Arc<dyn EventSink>) -> Self {
        let queue = Arc::new(Queue::with_capacity(32));
        let shutdown = Arc::new(AtomicBool::new(false));
        let work = queue.clone();
        let stop = shutdown.clone();
        let join = std::thread::Builder::new()
            .name("smtc-worker".into())
            .spawn(move || worker_loop(sink, work, stop))
            .ok();
        Self {
            queue,
            shutdown,
            join: Mutex::new(join),
        }
    }

    /// Applies a transport command, waiting briefly for the player's answer.
    pub fn send(&self, action: Action) -> bool {
        let (tx, rx) = channel();
        self.queue.push(Cmd::Send(action, tx));
        rx.recv_timeout(Duration::from_millis(500)).unwrap_or(false)
    }

    /// Queue only: the UI thread never waits for a media player.
    pub fn send_guarded(&self, request: super::control::ControlRequest) {
        let queued_at = Instant::now();
        log::info!("[smtc-control] phase=enqueue request_id={} action={:?}", request.request_id, request.action);
        self.queue.push(Cmd::Guarded(request, queued_at));
    }

    pub fn seek(&self, request: SeekRequest) {
        self.queue.push(Cmd::Seek(request));
    }

    /// Re-reads the followed session now instead of waiting for the next poll.
    pub fn refresh(&self) {
        self.queue.push(Cmd::Refresh);
    }

    /// Pins (or clears with `None`) the followed SMTC session by app ID.
    pub fn set_pinned(&self, app: Option<String>) {
        self.queue.push(Cmd::SetPinned(app));
    }

    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self
            .join
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = handle.join();
        }
    }
}

impl Drop for SmtcHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Initialises this thread's apartment. `RPC_E_CHANGED_MODE` means COM is
/// already initialised in another mode — usable, and not an error.
fn init_mta() -> bool {
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_ok() {
        return true;
    }
    hr.0 == RPC_E_CHANGED_MODE
}

enum Tick {
    Emit(Box<PlayerState>),
    Idle,
    Skip,
}

fn worker_loop(sink: Arc<dyn EventSink>, queue: Arc<Queue<Cmd>>, shutdown: Arc<AtomicBool>) {
    if !init_mta() {
        log::error!("[smtc] COM apartment initialization failed");
        return;
    }
    let mut manager: Option<SessionManager> = None;
    let mut sticky: Option<String> = None;
    let mut pinned: Option<String> = None;
    let mut session_titles: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut last_sessions: Vec<super::MediaSessionInfo> = Vec::new();
    let mut sampled: Vec<String> = Vec::new();
    let mut dedup = Dedup::default();
    let mut cover_key = String::new();
    let mut cover: Option<String> = None;
    let mut previous_cover: Option<String> = None;
    let mut cover_retry_until_ms: i64 = 0;
    let cover_dirty = Arc::new(AtomicBool::new(false));
    let mut subscribed: Option<(String, GlobalSystemMediaTransportControlsSession, i64)> = None;
    let mut force = true;
    let mut was_idle = false;
    let mut fail_streak = 0u32;
    let mut control_sample: Option<(i64, Instant)> = None;
    let mut pending = None;

    while !shutdown.load(Ordering::SeqCst) {
        while let Some(cmd) = pending.take().or_else(|| queue.pop()) {
            match cmd {
                Cmd::Send(action, reply) => {
                    let answer = manager
                        .as_ref()
                        .and_then(|m| m.GetCurrentSession().ok())
                        .is_some_and(|session| apply(&session, action));
                    let _ = reply.send(answer);
                    force = true;
                }
                Cmd::Guarded(mut request, mut queued_at) => {
                    let mut superseded_ids = Vec::new();
                    if matches!(
                        request.action,
                        super::control::Action::Play | super::control::Action::Pause
                    ) {
                        while let Some(next_cmd) = queue.pop() {
                            match next_cmd {
                                Cmd::Guarded(next_req, next_at)
                                    if matches!(
                                        next_req.action,
                                        super::control::Action::Play
                                            | super::control::Action::Pause
                                    ) =>
                                {
                                    superseded_ids.push(request.request_id);
                                    request = next_req;
                                    queued_at = next_at;
                                }
                                other => {
                                    pending = Some(other);
                                    break;
                                }
                            }
                        }
                    }
                    log::info!(
                        "[smtc-control] phase=start request_id={} action={:?} queue_ms={} coalesced={}",
                        request.request_id,
                        request.action,
                        queued_at.elapsed().as_millis(),
                        superseded_ids.len()
                    );
                    let started = Instant::now();
                    let accepted = manager
                        .as_ref()
                        .is_some_and(|m| super::control::apply_guarded(m, &request));
                    for id in superseded_ids {
                        sink.control_result(super::ControlResult {
                            request_id: id,
                            accepted,
                        });
                    }
                    sink.control_result(super::ControlResult {
                        request_id: request.request_id,
                        accepted,
                    });
                    log::info!(
                        "[smtc-control] phase=receipt request_id={} accepted={} worker_ms={} total_ms={}",
                        request.request_id,
                        accepted,
                        started.elapsed().as_millis(),
                        queued_at.elapsed().as_millis()
                    );
                    control_sample = Some((request.request_id, queued_at));
                    force = true;
                }
                Cmd::Seek(request) => {
                    if let Some(m) = manager.as_ref() {
                        let _ = seek_by_manager(m, &request);
                    }
                    force = true;
                }
                Cmd::Refresh => force = true,
                Cmd::SetPinned(app) => {
                    pinned = app;
                    force = true;
                }
            }
        }

        if manager.is_none() {
            match super::manager::request() {
                Ok(found) => {
                    manager = Some(found);
                    fail_streak = 0;
                    force = true;
                }
                Err(error) => {
                    log::warn!("[smtc] manager unavailable: {error}");
                    fail_streak = fail_streak.saturating_add(1);
                    sleep(super::manager::backoff_ms(fail_streak));
                    continue;
                }
            }
        }

        let tick = manager.as_ref().map(|m| {
            poll_once(
                m,
                &mut sticky,
                pinned.as_deref(),
                &mut session_titles,
                &mut last_sessions,
                &sink,
                &mut sampled,
                &mut dedup,
                &mut cover_key,
                &mut cover,
                &mut previous_cover,
                &mut cover_retry_until_ms,
                &cover_dirty,
                &mut subscribed,
                &queue,
                force,
            )
        });
        force = false;
        match tick {
            Some(Tick::Emit(state)) => {
                was_idle = false;
                sink.state(state.into_packet(false));
                if let Some((request_id, queued_at)) = control_sample.take() {
                    log::info!("[smtc-control] phase=emit request_id={} total_ms={}", request_id, queued_at.elapsed().as_millis());
                }
            }
            Some(Tick::Idle) => {
                if !was_idle {
                    was_idle = true;
                    cover_key.clear();
                    cover = None;
                    previous_cover = None;
                    cover_retry_until_ms = 0;
                    dedup.clear();
                    if let Some((_, old_session, token)) = subscribed.take() {
                        let _ = old_session.RemoveMediaPropertiesChanged(token);
                    }
                    sink.idle();
                }
            }
            Some(Tick::Skip) => {}
            None => {}
        }
        // Atomically check the queue before sleeping: commands that arrive
        // during polling or during the wait both run without a poll-sized delay.
        pending = queue.pop_wait(Duration::from_millis(POLL_FALLBACK_MS));
    }

    if let Some((_, old_session, token)) = subscribed.take() {
        let _ = old_session.RemoveMediaPropertiesChanged(token);
    }
    sink.idle();
    unsafe { CoUninitialize() };
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Reads the session list with synchronous getters only, then samples the
/// followed session's metadata.
#[allow(clippy::too_many_arguments)]
fn poll_once(
    manager: &SessionManager,
    sticky: &mut Option<String>,
    pinned: Option<&str>,
    session_titles: &mut std::collections::HashMap<String, String>,
    last_sessions: &mut Vec<super::MediaSessionInfo>,
    sink: &Arc<dyn EventSink>,
    sampled: &mut Vec<String>,
    dedup: &mut Dedup,
    cover_key: &mut String,
    cover: &mut Option<String>,
    previous_cover: &mut Option<String>,
    cover_retry_until_ms: &mut i64,
    cover_dirty: &Arc<AtomicBool>,
    subscribed: &mut Option<(String, GlobalSystemMediaTransportControlsSession, i64)>,
    queue: &Arc<Queue<Cmd>>,
    force: bool,
) -> Tick {
    let Ok(sessions) = manager.GetSessions() else {
        return Tick::Skip;
    };
    let Ok(count) = sessions.Size() else {
        return Tick::Skip;
    };
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut handles: Vec<GlobalSystemMediaTransportControlsSession> = Vec::new();
    for index in 0..count {
        let Ok(session) = sessions.GetAt(index) else {
            continue;
        };
        let app = session
            .SourceAppUserModelId()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let status = session
            .GetPlaybackInfo()
            .and_then(|info| info.PlaybackStatus())
            .map(|status| status.0)
            .unwrap_or(0);
        let last_updated_ms = session
            .GetTimelineProperties()
            .and_then(|tl| tl.LastUpdatedTime())
            .map(|time| filetime_to_unix_ms(time.UniversalTime))
            .unwrap_or(0);
        let known_title = sampled.iter().any(|seen| seen == &app);
        candidates.push(Candidate {
            app,
            status,
            last_updated_ms,
            known_title,
        });
        handles.push(session);
    }

    let active_indices: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.status != session::CLOSED && !c.app.is_empty())
        .map(|(i, _)| i)
        .collect();
    let mut sessions_info: Vec<super::MediaSessionInfo> = Vec::with_capacity(active_indices.len());
    for &idx in &active_indices {
        let c = &candidates[idx];
        if active_indices.len() > 1 && !session_titles.contains_key(&c.app) {
            if let Ok(props) = handles[idx]
                .TryGetMediaPropertiesAsync()
                .and_then(|op| op.join())
            {
                if let Ok(t) = props.Title() {
                    let title = t.to_string();
                    if !title.trim().is_empty() {
                        session_titles.insert(c.app.clone(), title);
                    }
                }
            }
        }
        sessions_info.push(super::MediaSessionInfo {
            app: c.app.clone(),
            status: c.status,
            title: session_titles.get(&c.app).cloned().unwrap_or_default(),
        });
    }
    if &sessions_info != last_sessions {
        *last_sessions = sessions_info.clone();
        sink.sessions(sessions_info);
    }

    let Some(index) = session::pick_with_pin(&candidates, sticky.as_deref(), pinned) else {
        return Tick::Idle;
    };
    let session = &handles[index];
    let app = candidates[index].app.clone();
    if sticky.as_deref() != Some(app.as_str()) {
        *sticky = Some(app.as_str().to_string());
    }
    if subscribed.as_ref().is_none_or(|(sub_app, _, _)| sub_app != &app) {
        if let Some((_, old_session, token)) = subscribed.take() {
            let _ = old_session.RemoveMediaPropertiesChanged(token);
        }
        let signal = cover_dirty.clone();
        let wake_queue = queue.clone();
        let handler = TypedEventHandler::<
            GlobalSystemMediaTransportControlsSession,
            MediaPropertiesChangedEventArgs,
        >::new(move |_, _| {
            if !signal.swap(true, Ordering::SeqCst) {
                wake_queue.push(Cmd::Refresh);
            }
            Ok(())
        });
        if let Ok(token) = session.MediaPropertiesChanged(&handler) {
            *subscribed = Some((app.clone(), session.clone(), token));
        }
    }
    if !sampled.iter().any(|seen| seen == &app) {
        sampled.push(app.clone());
        if sampled.len() > 16 {
            sampled.remove(0);
        }
    }

    let Ok(props) = session.TryGetMediaPropertiesAsync().and_then(|op| op.join()) else {
        return Tick::Skip;
    };
    let title = props.Title().map(|t| t.to_string()).unwrap_or_default();
    if !title.trim().is_empty() {
        session_titles.insert(app.clone(), title.clone());
    }
    let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();
    let album = props.AlbumTitle().map(|a| a.to_string()).unwrap_or_default();
    let album_artist = props.AlbumArtist().map(|a| a.to_string()).unwrap_or_default();
    let track_number = props.TrackNumber().unwrap_or(0);

    // A session with no title has been observed before its metadata lands. Do
    // not overwrite a good packet with an empty one.
    if title.trim().is_empty() && artist.trim().is_empty() && !dedup.last_key.is_empty() {
        return Tick::Skip;
    }

    let playback = session.GetPlaybackInfo().ok();
    let status = playback
        .as_ref()
        .and_then(|info| info.PlaybackStatus().ok())
        .map(|status| status.0)
        .unwrap_or(0);
    if status == 0 || oly_contracts::is_idle_track_title(&title) {
        return Tick::Idle;
    }
    let rate = playback
        .as_ref()
        .and_then(|info| info.PlaybackRate().ok())
        .and_then(|rate: IReference<f64>| rate.Value().ok())
        .filter(|rate| *rate > 0.0)
        .unwrap_or(1.0);

    let timeline = session.GetTimelineProperties().ok();
    let position_ms = timeline
        .as_ref()
        .and_then(|tl| tl.Position().ok())
        .map(|span| ticks_to_ms(span.Duration))
        .unwrap_or(0);
    let duration_ms = timeline
        .as_ref()
        .and_then(|tl| tl.EndTime().ok())
        .map(|span| ticks_to_ms(span.Duration))
        .unwrap_or(0);
    let start_ms = timeline
        .as_ref()
        .and_then(|tl| tl.StartTime().ok())
        .map(|span| ticks_to_ms(span.Duration))
        .unwrap_or(0);
    let updated_ms = timeline
        .as_ref()
        .and_then(|tl| tl.LastUpdatedTime().ok())
        .map(|time| filetime_to_unix_ms(time.UniversalTime))
        .unwrap_or(0);

    let available = is_available(position_ms, duration_ms, updated_ms);
    let (position_ms, updated_ms) = if available {
        (position_ms, updated_ms)
    } else {
        (0, 0)
    };

    let controls = playback.as_ref().and_then(|info| info.Controls().ok());
    let can_seek = available
        && controls
            .as_ref()
            .and_then(|c| c.IsPlaybackPositionEnabled().ok())
            .unwrap_or(false);
    let ctrl = Controls {
        next: controls
            .as_ref()
            .and_then(|c| c.IsNextEnabled().ok())
            .unwrap_or(false),
        prev: controls
            .as_ref()
            .and_then(|c| c.IsPreviousEnabled().ok())
            .unwrap_or(false),
        playpause: controls
            .as_ref()
            .and_then(|c| c.IsPlayPauseToggleEnabled().ok())
            .unwrap_or(false),
    };

    let now = now_ms();
    let key = key_of(&app, &title, &artist, &album);
    let track_changed = dedup.last_key != key && !dedup.last_key.is_empty();
    let status_changed = dedup.last_status != Some(status);
    if status_changed { log::info!("[smtc-state] status={} app={}", status, app); }

    let dirty = cover_dirty.swap(false, Ordering::SeqCst);
    let key_changed = key != *cover_key;
    if key_changed {
        *previous_cover = cover.clone();
        *cover_retry_until_ms = now + 3000;
        *cover = cover::read_cover(&props);
        *cover_key = key.clone();
        if cover.is_some() && *cover != *previous_cover {
            *cover_retry_until_ms = 0;
            *previous_cover = None;
        }
    } else if dirty || cover.is_none() || (now < *cover_retry_until_ms && *cover == *previous_cover) {
        let fresh = cover::read_cover(&props);
        if dirty || (fresh.is_some() && (cover.is_none() || fresh != *cover)) {
            *cover = fresh;
        }
        if cover.is_some() && *cover != *previous_cover {
            *cover_retry_until_ms = 0;
            *previous_cover = None;
        }
    } else if *cover_retry_until_ms > 0 && now >= *cover_retry_until_ms {
        *cover_retry_until_ms = 0;
        *previous_cover = None;
    }
    let cover_changed = *cover != dedup.last_sent_cover;
    let cover_field = if track_changed || force || status_changed || cover_changed {
        Some(cover.clone())
    } else {
        None
    };

    if dedup.should_throttle(
        force,
        track_changed,
        status_changed,
        cover_changed,
        now,
        POSITION_INTERVAL_MS,
    ) {
        return Tick::Skip;
    }

    let state = PlayerState {
        app,
        title,
        artist,
        album,
        album_artist,
        track_number,
        status,
        rate,
        duration_ms,
        position_ms,
        start_ms,
        last_updated_unix_ms: updated_ms,
        position_available: available,
        can_seek,
        ctrl,
        track_changed,
        cover: cover_field,
    };
    dedup.last_key = key;
    dedup.last_status = Some(status);
    dedup.last_emit_tick = now_ms();
    dedup.last_sent_cover = cover.clone();
    Tick::Emit(Box::new(state))
}

/// Status names are part of the wire contract; exposed for the debug HUD.
pub fn status_label(status: GlobalSystemMediaTransportControlsSessionPlaybackStatus) -> &'static str {
    session::status_name(status.0)
}

/// Unused today, but keeps the idle sentinel referenced on this path so a
/// future "manager went away" branch cannot forget it.
pub const IDLE: &str = IDLE_KEY;

/// Volume answers produced by this module's sibling worker are emitted through
/// the same sink; this helper exists so a test can construct one without
/// reaching into the internal fields.
pub fn volume_event(app: &str, value: Option<f32>, request_id: i64) -> VolumeEvent {
    VolumeEvent {
        app: app.to_string(),
        available: value.is_some(),
        value,
        mode: "mixer".into(),
        request_id,
        reason: None,
    }
}
