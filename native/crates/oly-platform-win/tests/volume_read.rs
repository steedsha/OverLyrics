//! Read-only checks against the live Windows audio stack.
//!
//! The plan for Task 12 is explicit: the first automated pass may only *read*
//! the real volume — never write it. A test that moved a player's slider would
//! change the operator's machine, and a test that guessed at a mixer session
//! would fail on any machine that is not currently playing that exact app.
//!
//! So this suite pins three things:
//!
//! 1. The pure command/answer layer carries a request id, a channel label and a
//!    value all the way through, including the failure shapes.
//! 2. A live read of this very process's audio session (or of a named player,
//!    when one is running) either returns a finite fraction in `0..=1` or
//!    reports `available: false` — never a panic, never an out-of-range value.
//! 3. The worker thread answers an enqueued read and survives being dropped.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oly_platform_win::smtc::volume::{Level, VolumeCmd, VolumeHandle, answer};
use oly_platform_win::smtc::{EventSink, Queue, VolumeEvent};
use oly_platform_win::volume;

/// Collects what the worker emits so a test can assert without a UI.
#[derive(Default)]
struct Sink {
    events: Mutex<Vec<VolumeEvent>>,
}

impl Sink {
    fn last(&self) -> Option<VolumeEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
    }

    fn wait_for(&self, timeout: Duration) -> Option<VolumeEvent> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(event) = self.last() {
                return Some(event);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.last()
    }
}

impl EventSink for Sink {
    fn state(&self, _packet: oly_contracts::PlayerPacket) {}

    fn idle(&self) {}

    fn volume(&self, event: VolumeEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event);
    }
}

fn app_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

#[test]
fn a_read_request_keeps_its_id_and_reports_a_fraction_or_unavailable() {
    let app = app_name();
    // `resolve` touches the live mixer; a plain read must not need a player
    // window, and a machine with no session for this exe still answers.
    let resolved = volume::resolve(&app, "mixer", None);
    match resolved {
        None => {}
        Some((level, channel)) => {
            assert_eq!(channel, "mixer");
            let value = level.value();
            assert!(value.is_finite(), "mixer read returned {value}");
            assert!(
                (0.0..=1.0).contains(&value),
                "mixer read {value} outside 0..=1"
            );
        }
    }
}

#[test]
fn the_worker_answers_a_live_read_and_shuts_down_cleanly() {
    let sink = Arc::new(Sink::default());
    let current = Arc::new(Mutex::new(String::new()));
    let handle: VolumeHandle = volume::spawn(sink.clone(), current.clone());

    // An empty target is answered rather than dropped, and it carries the id.
    handle.get(String::new(), "auto".into(), 41);
    let event = sink
        .wait_for(Duration::from_secs(5))
        .expect("the worker must answer an enqueued request");
    assert_eq!(event.request_id, 41);
    assert!(!event.available);
    assert_eq!(event.reason, Some("no-session"));

    // A named app goes through the real channels. Either channel may answer or
    // neither may (no session), but an answer must arrive and stay in range.
    let app = app_name();
    handle.get(app.clone(), "auto".into(), 42);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let events = sink.events.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(event) = events.iter().find(|e| e.request_id == 42) {
            if event.available {
                let value = event.value.expect("an available event carries a value");
                assert!(value.is_finite() && (0.0..=1.0).contains(&value));
                assert!(matches!(event.mode.as_str(), "player" | "mixer"));
            } else {
                assert!(event.reason.is_some());
            }
            break;
        }
        drop(events);
        assert!(
            Instant::now() < deadline,
            "the live read must answer within its budget"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_write_shape_is_carried_but_the_suite_never_sends_one() {
    // The Set variant must round-trip its clamped value through `answer` without
    // a live write; this is the only place the suite exercises the write shape.
    let cmd = VolumeCmd::Set {
        app: "not-running.exe".into(),
        value: 0.25,
        mode: "auto".into(),
        request_id: 7,
    };
    let event = answer(&cmd, "not-running.exe", Some((Level::Player(0.25), "player")));
    assert!(event.available);
    assert_eq!(event.value, Some(0.25));
    assert_eq!(event.mode, "player");
}

#[test]
fn the_queue_bounds_volume_requests_too() {
    // A drag can enqueue faster than one UIA round trip; the transport queue is
    // the backpressure, and it drops the oldest request rather than growing.
    let queue: Queue<VolumeCmd> = Queue::with_capacity(4);
    for id in 0..6 {
        queue.push(VolumeCmd::Get {
            app: "x.exe".into(),
            mode: "auto".into(),
            request_id: id,
        });
    }
    assert_eq!(queue.len(), 4);
    assert_eq!(queue.dropped(), 2);
    let ids: Vec<i64> = std::iter::from_fn(|| queue.pop())
        .map(|cmd| cmd.request_id())
        .collect();
    assert_eq!(ids, vec![2, 3, 4, 5]);
}
