//! Windows SMTC bridge, in-process and without a webview.
//!
//! The Tauri build owned every WinRT object on one dedicated MTA thread and
//! pushed `smtc:state` events into the renderer through `AppHandle::emit`. This
//! port keeps the thread and the apartment rule — WinRT objects are
//! apartment-affine, and touching one from a tokio worker produces
//! `RPC_E_WRONG_THREAD` at best — but replaces the emit with an [`EventSink`]
//! the GPUI shell implements. Nothing here knows about windows, widgets or the
//! config store.
//!
//! The pure half (`session`, `timeline`, `events`, `control`, `manager`) is
//! compiled and tested on every host, so the selection / throttling / guard
//! arithmetic is pinned by `tests/packet_replay.rs` against the frozen
//! 742-packet trace rather than by eyeballing a live player.

pub mod control;
pub mod cover;
pub mod events;
pub mod manager;
pub mod session;
pub mod timeline;
pub mod volume;
#[cfg(windows)]
pub mod winrt;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use oly_contracts::PlayerPacket;

/// Where the SMTC worker publishes. Implemented by the GPUI shell on the UI
/// thread; every method must be cheap and non-blocking, because the worker calls
/// them while it holds no lock of its own.
pub trait EventSink: Send + Sync {
    /// A full state packet, already throttled and identity-checked.
    fn state(&self, packet: PlayerPacket);
    /// The session list went empty (or the manager is unavailable).
    fn idle(&self);
    /// One answer to a volume read/write.
    fn volume(&self, event: VolumeEvent);
    /// Receipt for one user-initiated guarded transport action.
    fn control_result(&self, _event: ControlResult) {}
    /// Active SMTC sessions snapshot for manual media switching.
    fn sessions(&self, _sessions: Vec<MediaSessionInfo>) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSessionInfo {
    pub app: String,
    pub status: i32,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlResult {
    pub request_id: i64,
    pub accepted: bool,
}

/// One answer to a volume request, in the shape the volume panel reads.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeEvent {
    pub app: String,
    pub available: bool,
    pub value: Option<f32>,
    pub mode: String,
    pub request_id: i64,
    /// `Some("no-session")` when the app has no audio session at all; `None`
    /// when the request simply succeeded or failed silently.
    pub reason: Option<&'static str>,
}

/// A bounded multi-producer queue.
///
/// `std::sync::mpsc` cannot be shared between the watchdog and a replacement
/// worker, and an unbounded channel lets a stalled consumer grow without limit
/// while a player fires events — so this is a `VecDeque` with an explicit
/// capacity that drops the oldest command, plus a counter the health check can
/// read.
pub struct Queue<T> {
    items: Mutex<VecDeque<T>>,
    cv: Condvar,
    capacity: usize,
    dropped: AtomicU64,
}

impl<T> Queue<T> {
    pub fn new() -> Self {
        Self::with_capacity(256)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            cv: Condvar::new(),
            capacity: capacity.max(1),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn push(&self, item: T) {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        while items.len() >= self.capacity {
            items.pop_front();
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
        items.push_back(item);
        drop(items);
        self.cv.notify_all();
    }

    pub fn pop(&self) -> Option<T> {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    /// Waits up to `timeout` for an item. Returns `None` on timeout.
    pub fn pop_wait(&self, timeout: Duration) -> Option<T> {
        let guard = self.items.lock().unwrap_or_else(|e| e.into_inner());
        let (mut guard, _) = self
            .cv
            .wait_timeout_while(guard, timeout, |items| items.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        guard.pop_front()
    }

    pub fn len(&self) -> usize {
        self.items.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Commands discarded because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::SeqCst)
    }
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Milliseconds since the Unix epoch, the base every packet timestamp uses.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Monotonic milliseconds for interval measurement, immune to wall-clock jumps.
pub fn mono_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_queue_reports_its_capacity_pressure() {
        let queue: Queue<u8> = Queue::with_capacity(2);
        queue.push(1);
        queue.push(2);
        assert!(queue.dropped() == 0);
        queue.push(3);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped(), 1);
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(3));
        assert!(queue.is_empty());
    }

    #[test]
    fn a_prequeued_command_does_not_wait_for_the_poll_timeout() {
        let queue = Queue::with_capacity(4);
        queue.push(9_u8);
        let started = Instant::now();
        assert_eq!(queue.pop_wait(Duration::from_millis(400)), Some(9));
        assert!(started.elapsed() < Duration::from_millis(200),
            "an already queued command waited for the poll timeout: {:?}", started.elapsed());
    }

    #[test]
    fn a_waiting_consumer_wakes_on_push_and_times_out_cleanly() {
        let queue: Queue<u8> = Queue::with_capacity(4);
        assert_eq!(queue.pop_wait(Duration::from_millis(1)), None);
        queue.push(9);
        assert_eq!(queue.pop_wait(Duration::from_millis(1)), Some(9));
    }
}
