//! The shell's half of the platform bridge: one inbound mailbox, one outbound
//! command surface, and exactly one worker pair per process.
//!
//! The Tauri build reached the renderer through `AppHandle::emit` and reached
//! the platform through `invoke` handlers. Neither exists here. What the two
//! halves of the product actually need is narrower:
//!
//! * **worker → UI**: a stream of state packets, idle transitions and volume
//!   answers. Packets arrive several times a second and are sampled by a 60 Hz
//!   render loop, so only the newest one is worth keeping — an unbounded queue
//!   would let a stalled UI fall minutes behind and then replay every stale
//!   position in a burst. Volume answers are different: each one carries a
//!   request id the panel is waiting on, so they queue in arrival order and
//!   only a full queue drops (oldest first) with a counter.
//! * **UI → worker**: transport actions, seeks, refreshes and volume
//!   get/set. These are fire-and-forget "try" calls; a player may refuse, and
//!   a refused call is never retried against a different session.
//!
//! The one-worker rule matters for cost, not just tidiness: every extra SMTC
//! worker polls every player at 200 ms and holds its own WinRT subscriptions,
//! and every extra volume worker can spawn its own PowerShell child. So the
//! worker pair is claimed once, through [`WorkerGuard`], and only one window
//! ever owns it — a second claim is refused rather than quietly doubling the
//! polling.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use oly_contracts::PlayerPacket;
use oly_platform_win::smtc::control::{Action, SeekRequest};
use oly_platform_win::smtc::volume::VolumeHandle;
use oly_platform_win::smtc::{EventSink, VolumeEvent};

/// How many volume answers the UI may fall behind before the oldest is dropped.
pub const INBOX_CAPACITY: usize = 32;

/// One thing the render loop must apply, in arrival order.
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    /// A complete playback sample; supersedes any older sample or idle mark.
    State(PlayerPacket),
    /// The session list went empty; supersedes any older sample.
    Idle,
    /// One answer to a volume request.
    Volume(VolumeEvent),
    Control(oly_platform_win::smtc::ControlResult),
    Sessions(Vec<oly_platform_win::smtc::MediaSessionInfo>),
}

/// Bounded, coalescing inbound mailbox.
///
/// The state slot is latest-wins on purpose: a render loop that runs at 60 Hz
/// cannot show more than one sample per frame, and a UI that was blocked for a
/// second must resume from the truth rather than replay the backlog. Volume
/// answers keep their identity instead, because a panel awaiting request 7
/// must not be handed the answer to request 3.
#[derive(Debug)]
pub struct Inbox {
    state: Option<UiEvent>,
    control: Option<oly_platform_win::smtc::ControlResult>,
    sessions: Option<Vec<oly_platform_win::smtc::MediaSessionInfo>>,
    state_dropped: u64,
    volume: VecDeque<VolumeEvent>,
    volume_dropped: u64,
    capacity: usize,
}

impl Default for Inbox {
    fn default() -> Self {
        Self::with_capacity(INBOX_CAPACITY)
    }
}

impl Inbox {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            state: None,
            control: None,
            sessions: None,
            state_dropped: 0,
            volume: VecDeque::new(),
            volume_dropped: 0,
            capacity: capacity.max(1),
        }
    }

    fn push_state(&mut self, mut event: UiEvent) {
        if let (Some(UiEvent::State(old)), UiEvent::State(new)) = (&mut self.state, &mut event) {
            if !new.cover_updated && new.cover.is_none() && !new.track_changed && old.track_key() == new.track_key() {
                new.cover = old.cover.take();
                new.cover_updated = old.cover_updated;
            }
        }
        if self.state.replace(event).is_some() {
            self.state_dropped += 1;
        }
    }

    fn push_volume(&mut self, event: VolumeEvent) {
        if self.volume.len() >= self.capacity {
            self.volume.pop_front();
            self.volume_dropped += 1;
        }
        self.volume.push_back(event);
    }

    /// Takes everything pending. Called once per frame by the render loop.
    pub fn drain(&mut self) -> Vec<UiEvent> {
        let mut out = Vec::with_capacity(1 + self.volume.len());
        if let Some(state) = self.state.take() {
            out.push(state);
        }
        out.extend(self.volume.drain(..).map(UiEvent::Volume));
        if let Some(control) = self.control.take() { out.push(UiEvent::Control(control)); }
        if let Some(sessions) = self.sessions.take() { out.push(UiEvent::Sessions(sessions)); }
        out
    }

    /// Samples superseded before the UI could draw them.
    pub fn state_dropped(&self) -> u64 {
        self.state_dropped
    }

    /// Volume answers dropped because the UI was not draining.
    pub fn volume_dropped(&self) -> u64 {
        self.volume_dropped
    }

    pub fn is_empty(&self) -> bool {
        self.state.is_none() && self.volume.is_empty() && self.control.is_none() && self.sessions.is_none()
    }
}

/// The worker-facing end of the mailbox.
#[derive(Clone, Debug)]
pub struct ShellSink {
    inbox: Arc<Mutex<Inbox>>,
}

impl ShellSink {
    pub fn new(inbox: Arc<Mutex<Inbox>>) -> Self {
        Self { inbox }
    }

    /// The shared inbox, for the render loop that drains it.
    pub fn inbox(&self) -> Arc<Mutex<Inbox>> {
        self.inbox.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl EventSink for ShellSink {
    fn state(&self, packet: PlayerPacket) {
        self.lock().push_state(UiEvent::State(packet));
    }

    fn idle(&self) {
        self.lock().push_state(UiEvent::Idle);
    }

    fn volume(&self, event: VolumeEvent) { self.lock().push_volume(event); }
    fn control_result(&self, event: oly_platform_win::smtc::ControlResult) { self.lock().control = Some(event); }
    fn sessions(&self, sessions: Vec<oly_platform_win::smtc::MediaSessionInfo>) { self.lock().sessions = Some(sessions); }
}

/// One outbound platform command.
#[derive(Clone, Debug, PartialEq)]
pub enum HostCommand {
    /// Skip next / previous, or play / pause the followed session.
    Transport(Action),
    GuardedTransport(oly_platform_win::smtc::control::ControlRequest),
    /// Seek the followed session, guarded by identity and deadline.
    Seek(SeekRequest),
    /// Re-read metadata and cover art for the followed session.
    Refresh,
    /// Pin (or clear with `None`) the followed SMTC session by app ID.
    SetPinnedSession(Option<String>),
    VolumeGet {
        app: String,
        mode: String,
        request_id: i64,
    },
    VolumeSet {
        app: String,
        value: f32,
        mode: String,
        request_id: i64,
    },
}

impl HostCommand {
    /// Commands that must reach a worker; used by the shell to decide whether a
    /// missing worker is a bug or just an idle machine.
    pub fn needs_worker(&self) -> bool {
        true
    }
}

/// One-shot claim that keeps the process to a single worker pair.
#[derive(Debug, Default)]
pub struct WorkerGuard {
    claimed: AtomicBool,
}

impl WorkerGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// True on the first call only.
    pub fn claim(&self) -> bool {
        !self.claimed.swap(true, Ordering::SeqCst)
    }

    pub fn is_claimed(&self) -> bool {
        self.claimed.load(Ordering::SeqCst)
    }
}

/// The shell's handle on the platform workers.
///
/// Constructed once by `main`; every window shares it. `detached()` is the
/// no-worker shape the window unit tests use — dispatch reports "not
/// delivered" instead of pretending a player was reached.
pub struct ShellHost {
    #[cfg(windows)]
    smtc: Option<oly_platform_win::smtc::winrt::SmtcHandle>,
    /// The app the SMTC worker follows, shared with the volume worker so a
    /// request that names no app still targets the playing one.
    current_app: Arc<Mutex<String>>,
    volume: Option<VolumeHandle>,
    guard: WorkerGuard,
}

impl ShellHost {
    /// Starts the process-wide worker pair against `sink`.
    ///
    /// Returns `None` when a worker pair was already claimed: two SMTC pollers
    /// on one machine double the polling cost and race each other's
    /// subscriptions, so refusing is the honest answer.
    pub fn start(sink: Arc<dyn EventSink>) -> Option<Self> {
        let guard = WorkerGuard::new();
        if !guard.claim() {
            return None;
        }
        let current_app = Arc::new(Mutex::new(String::new()));
        #[cfg(windows)]
        let smtc = Some(oly_platform_win::smtc::winrt::SmtcHandle::spawn(sink.clone()));
        #[cfg(not(windows))]
        let _ = &sink;
        let volume = Some(oly_platform_win::volume::spawn(sink, current_app.clone()));
        Some(Self {
            #[cfg(windows)]
            smtc,
            current_app,
            volume,
            guard,
        })
    }

    /// The no-worker shape: every dispatch is refused.
    pub fn detached() -> Self {
        Self {
            #[cfg(windows)]
            smtc: None,
            current_app: Arc::new(Mutex::new(String::new())),
            volume: None,
            guard: WorkerGuard::new(),
        }
    }

    /// Records the followed app; volume requests without a target use it.
    pub fn follow(&self, app: &str) {
        *self.current_app.lock().unwrap_or_else(|e| e.into_inner()) = app.to_string();
    }

    pub fn current_app(&self) -> String {
        self.current_app
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn has_workers(&self) -> bool {
        #[cfg(windows)]
        {
            self.smtc.is_some() && self.volume.is_some()
        }
        #[cfg(not(windows))]
        {
            self.volume.is_some()
        }
    }

    pub fn guard(&self) -> &WorkerGuard {
        &self.guard
    }

    /// Delivers one command. `false` means "no worker accepted it".
    pub fn dispatch(&self, command: HostCommand) -> bool {
        match command {
            HostCommand::GuardedTransport(request) => {
                #[cfg(windows)]
                { self.smtc.as_ref().is_some_and(|smtc| { smtc.send_guarded(request); true }) }
                #[cfg(not(windows))]
                { let _ = request; false }
            }
            HostCommand::Transport(action) => {
                #[cfg(windows)]
                {
                    self.smtc.as_ref().is_some_and(|smtc| smtc.send(action))
                }
                #[cfg(not(windows))]
                {
                    let _ = action;
                    false
                }
            }
            HostCommand::Seek(request) => {
                #[cfg(windows)]
                {
                    match self.smtc.as_ref() {
                        Some(smtc) => {
                            smtc.seek(request);
                            true
                        }
                        None => false,
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = request;
                    false
                }
            }
            HostCommand::Refresh => {
                #[cfg(windows)]
                {
                    match self.smtc.as_ref() {
                        Some(smtc) => {
                            smtc.refresh();
                            true
                        }
                        None => false,
                    }
                }
                #[cfg(not(windows))]
                {
                    false
                }
            }
            HostCommand::SetPinnedSession(app) => {
                #[cfg(windows)]
                {
                    match self.smtc.as_ref() {
                        Some(smtc) => {
                            smtc.set_pinned(app);
                            true
                        }
                        None => false,
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = app;
                    false
                }
            }
            HostCommand::VolumeGet {
                app,
                mode,
                request_id,
            } => match &self.volume {
                Some(volume) => {
                    volume.get(app, mode, request_id);
                    true
                }
                None => false,
            },
            HostCommand::VolumeSet {
                app,
                value,
                mode,
                request_id,
            } => match &self.volume {
                Some(volume) => {
                    volume.set(app, value, mode, request_id);
                    true
                }
                None => false,
            },
        }
    }

    /// Stops the SMTC worker; the volume thread exits with the process.
    pub fn shutdown(&mut self) {
        #[cfg(windows)]
        if let Some(smtc) = self.smtc.as_mut() {
            smtc.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oly_contracts::PlayerPacket;

    fn packet(title: &str) -> PlayerPacket {
        PlayerPacket {
            title: title.into(),
            app: "MSEdge".into(),
            ..Default::default()
        }
    }

    fn volume_event(id: i64) -> VolumeEvent {
        VolumeEvent {
            app: "msedge.exe".into(),
            available: true,
            value: Some(0.5),
            mode: "mixer".into(),
            request_id: id,
            reason: None,
        }
    }

    #[test]
    fn a_burst_of_packets_collapses_to_the_newest_one() {
        let sink = ShellSink::new(Arc::new(Mutex::new(Inbox::default())));
        for index in 0..50 {
            sink.state(packet(&format!("track {index}")));
        }
        let mut inbox = sink.inbox.lock().unwrap();
        assert_eq!(inbox.state_dropped(), 49);
        let drained = inbox.drain();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            UiEvent::State(packet) => assert_eq!(packet.title, "track 49"),
            other => panic!("expected the newest state, got {other:?}"),
        }
        assert!(inbox.is_empty());
    }

    #[test]
    fn an_idle_after_a_packet_replaces_it_and_a_packet_after_idle_replaces_that() {
        let sink = ShellSink::new(Arc::new(Mutex::new(Inbox::default())));
        sink.state(packet("a"));
        sink.idle();
        assert!(matches!(
            sink.inbox.lock().unwrap().drain().as_slice(),
            [UiEvent::Idle]
        ));
        sink.idle();
        sink.state(packet("b"));
        let drained = sink.inbox.lock().unwrap().drain();
        assert!(matches!(&drained[0], UiEvent::State(p) if p.title == "b"));
    }

    #[test]
    fn volume_answers_keep_their_order_and_drop_the_oldest_when_full() {
        let sink = ShellSink::new(Arc::new(Mutex::new(Inbox::with_capacity(3))));
        for id in 0..5 {
            sink.volume(volume_event(id));
        }
        let mut inbox = sink.inbox.lock().unwrap();
        assert_eq!(inbox.volume_dropped(), 2);
        let ids: Vec<i64> = inbox
            .drain()
            .into_iter()
            .map(|event| match event {
                UiEvent::Volume(v) => v.request_id,
                other => panic!("expected volume only, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec![2, 3, 4]);
    }

    #[test]
    fn a_detached_host_refuses_every_command_instead_of_pretending() {
        let host = ShellHost::detached();
        assert!(!host.has_workers());
        assert!(!host.dispatch(HostCommand::Transport(Action::Next)));
        assert!(!host.dispatch(HostCommand::Refresh));
        assert!(!host.dispatch(HostCommand::VolumeGet {
            app: "x.exe".into(),
            mode: "auto".into(),
            request_id: 1,
        }));
        assert_eq!(host.current_app(), "");
        host.follow("Spotify.exe");
        assert_eq!(host.current_app(), "Spotify.exe");
    }

    #[test]
    fn a_worker_pair_is_claimed_only_once() {
        let guard = WorkerGuard::new();
        assert!(guard.claim(), "the first owner wins");
        assert!(!guard.claim(), "a second window must not start a second poller");
        assert!(guard.is_claimed());
    }

    #[test]
    fn the_transport_actions_map_one_to_one() {
        // The numeric discriminants are the wire values the legacy bridge used;
        // a renumbered enum would silently swap skip-next and play.
        assert_eq!(Action::Next as i32, 0);
        assert_eq!(Action::Prev as i32, 1);
        assert_eq!(Action::Play as i32, 2);
        assert_eq!(Action::Pause as i32, 3);
    }
}
