use oly_contracts::{PlaybackStatus, PlayerPacket};
use oly_gpui_app::live::LiveModel;
fn packet(title: &str, pos: f64) -> PlayerPacket {
    PlayerPacket {
        title: title.into(),
        artist: "Artist".into(),
        app: "test".into(),
        position_ms: pos,
        duration_ms: 180000.0,
        status: Some(PlaybackStatus::Code(4)),
        ..Default::default()
    }
}
#[test]
fn repeated_packets_do_not_restart_search_and_old_song_results_are_rejected() {
    let mut m = LiveModel::default();
    assert!(m.feed(packet("First", 1000.0), 1.0).is_some());
    let first = m.generation();
    assert!(m.feed(packet("First", 1200.0), 201.0).is_none());
    assert!(m.accepts(first));
    assert!(m.feed(packet("Second", 0.0), 220.0).is_some());
    assert!(!m.accepts(first));
    m.idle();
    assert!(!m.accepts(m.generation()));
    assert_eq!(m.position(500.0), 0.0);
}
#[test]
fn pause_uses_shared_clock_and_never_replays_the_elapsed_pause() {
    let mut m = LiveModel::default();
    m.feed(packet("First", 1000.0), 1.0);
    m.tick(101.0);
    let mut paused = packet("First", 1100.0);
    paused.status = Some(PlaybackStatus::Code(5));
    m.feed(paused, 101.0);
    let frozen = m.position(101.0);
    m.tick(10101.0);
    assert_eq!(m.position(10101.0), frozen);
}
#[test]
fn album_metadata_updates_search_without_restarting_the_playhead() {
    let mut m = LiveModel::default();
    m.feed(packet("First", 1000.0), 1.0);
    let old = m.generation();
    let mut updated = packet("First", 1200.0);
    updated.album = "Album".into();
    assert!(m.feed(updated, 201.0).is_some());
    assert!(!m.accepts(old));
    assert!((m.position(201.0) - 1200.0).abs() < 10.0);
}

use oly_engine::{
    net::{HttpError, HttpRequest, HttpResponse, Transport},
    providers::{LyricsSource, ProviderError, ProviderResult, SearchOptions, Supports},
    service::LyricsService,
    types::{Candidate, Track},
};
use oly_gpui_app::live::{SearchAnswer, SearchWorker};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
struct NoNetwork;
impl Transport for NoNetwork {
    fn send(&self, _: &HttpRequest) -> Result<HttpResponse, HttpError> {
        panic!("test must never use real HTTP")
    }
}
struct Source {
    calls: mpsc::Sender<String>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl LyricsSource for Source {
    fn id(&self) -> &'static str {
        "lrclib"
    }
    fn name(&self) -> &'static str {
        "test"
    }
    fn supports(&self) -> Supports {
        Supports {
            word: false,
            trans: false,
            roma: false,
        }
    }
    fn search(
        &self,
        _: &dyn Transport,
        track: &Track,
        _: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError> {
        self.calls.send(track.title.clone()).unwrap();
        if track.title == "First Song" {
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }
        Ok(vec![Candidate {
            source: "lrclib".into(),
            id: track.title.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            duration_sec: Some(180.0),
            ..Default::default()
        }])
    }
    fn lyrics(&self, _: &dyn Transport, _: &Candidate) -> Result<ProviderResult, ProviderError> {
        Ok(ProviderResult {
            raw: "[00:00.00]first line\n[00:02.00]second line\n[00:04.00]third line\n".into(),
            format: "lrc".into(),
            ..Default::default()
        })
    }
}
fn track(title: &str) -> Track {
    Track {
        title: title.into(),
        artist: Some("Artist".into()),
        duration_ms: Some(180000.0),
        ..Default::default()
    }
}
fn wait_answer(worker: &SearchWorker) -> SearchAnswer {
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(answer) = worker.take() {
            if !answer.searching {
                return answer;
            }
        }
        assert!(Instant::now() < until, "worker answer timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn worker_keeps_only_latest_pending_track_and_builds_a_real_render_document() {
    let (calls_tx, calls) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let dir = std::env::temp_dir().join(format!("oly-live-flow-{}", std::process::id()));
    let service = Arc::new(LyricsService::with_sources(
        &dir,
        Arc::new(|| serde_json::json!({})),
        Arc::new(|_| {}),
        vec![Box::new(Source {
            calls: calls_tx,
            release: Mutex::new(release_rx),
        })],
    ));
    let worker = SearchWorker::start(service, Arc::new(NoNetwork));
    worker.request(1, track("First Song"), None);
    assert_eq!(
        calls.recv_timeout(Duration::from_secs(2)).unwrap(),
        "First Song"
    );
    worker.request(2, track("Discarded Song"), None);
    worker.request(3, track("Latest Song"), None);
    release.send(()).unwrap();
    assert_eq!(
        calls.recv_timeout(Duration::from_secs(2)).unwrap(),
        "Latest Song"
    );
    let answer = wait_answer(&worker);
    assert_eq!(answer.generation, 3);
    assert!(!answer.manual);
    assert!(!answer.searching);
    assert!(answer.error.is_empty(), "{}", answer.error);
    let doc = answer.doc.unwrap();
    assert_eq!(doc.title, "Latest Song");
    assert_eq!(doc.lines.len(), 3);
    assert_eq!(doc.duration_sec, 180.0);
    assert_eq!(doc.lines[1].start, Some(2000));
    assert!(
        !dir.join("source-memory.json").exists(),
        "auto choice must not persist memory"
    );
    assert!(calls.try_recv().is_err());
    std::fs::create_dir_all(&dir).unwrap();
    worker.request(3, track("Latest Song"), Some(answer.candidates[0].clone()));
    let manual = wait_answer(&worker);
    assert!(manual.manual);
    assert!(!manual.searching);
    assert!(manual.doc.is_some(), "{}", manual.error);
    let saved = std::fs::read_to_string(dir.join("source-memory.json")).unwrap();
    assert!(saved.contains("Latest Song"));
    assert!(dir.join("lyrics-cache").exists(), "validated lyrics should be cached immediately");
    std::fs::remove_dir_all(&dir).unwrap();
}
#[test]
fn stale_scheduled_epoch_never_starts_a_search() {
    let service = LyricsService::with_sources(
        std::env::temp_dir(),
        Arc::new(|| serde_json::json!({})),
        Arc::new(|_| {}),
        vec![],
    );
    let epoch = service.epoch();
    service.invalidate();
    assert!(
        service
            .search_all_at(&NoNetwork, &track("Old Song"), Default::default(), epoch)
            .cancelled
    );
}

#[test]
fn cover_decode_is_background_work_and_retains_generation() {
    use base64::Engine;
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 3)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let source = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(encoded.into_inner())
    );
    let worker = oly_gpui_app::live::ArtworkWorker::start();
    assert!(worker.submit(7, Some(source)));
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some((generation, cover)) = worker.take() {
            assert_eq!(generation, 7);
            let cover = cover.unwrap();
            assert_eq!((cover.width, cover.height), (2, 3));
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn ordinary_position_packets_keep_cover_and_late_duration_restarts_match() {
    let mut model = LiveModel::default();
    let mut first = packet("Song", 1000.0);
    first.cover = Some("data:image/png;base64,example".into());
    first.duration_ms = 0.0;
    model.feed(first, 1.0);
    let generation = model.generation();
    assert!(model.feed(packet("Song", 1200.0), 201.0).is_some());
    assert!(model.generation() > generation);
    assert!(model.packet.as_ref().unwrap().cover.is_some());
    assert!(model.feed(packet("Song", 1400.0), 401.0).is_none());
    assert!(model.packet.as_ref().unwrap().cover.is_some());
}

#[test]
fn explicit_cover_clear_differs_from_an_omitted_cover() {
    let mut model = LiveModel::default();
    let mut first = packet("Song", 1000.0);
    first.cover = Some("artwork".into());
    model.feed(first, 1.0);
    let mut clear = packet("Song", 1200.0);
    clear.cover_updated = true;
    model.feed(clear, 201.0);
    assert!(model.packet.as_ref().unwrap().cover.is_none());
}
#[test]
fn coalescing_mailbox_preserves_cover_updates() {
    use oly_gpui_app::commands::{Inbox, ShellSink, UiEvent};
    use oly_platform_win::smtc::EventSink;
    let sink = ShellSink::new(Arc::new(Mutex::new(Inbox::default())));
    let mut first = packet("Song", 1000.0);
    first.cover = Some("artwork".into());
    first.cover_updated = true;
    sink.state(first);
    sink.state(packet("Song", 1200.0));
    let UiEvent::State(merged) = sink.inbox().lock().unwrap().drain().remove(0) else {
        panic!()
    };
    assert_eq!(merged.cover.as_deref(), Some("artwork"));
    assert!(merged.cover_updated);
}


#[test]
fn control_answers_are_fenced_by_request_and_track_and_never_fake_playback() {
    use oly_platform_win::smtc::control::Action;
    let mut model = LiveModel::default();
    let mut song = packet("Song", 1000.0);
    song.ctrl.playpause = true;
    model.feed(song, 1.0);
    let first = model.prepare_control(Action::Pause, 1000).unwrap();
    let second = model.prepare_control(Action::Pause, 1100).unwrap();
    assert!(model.packet.as_ref().unwrap().is_playing());
    assert_eq!(model.control_result(first.request_id, true), None);
    assert_eq!(model.control_result(second.request_id, false), Some(false));
    let old = model.prepare_control(Action::Pause, 1200).unwrap();
    model.feed(packet("New Song", 0.0), 300.0);
    assert_eq!(model.control_result(old.request_id, true), None);
    assert!(model.prepare_control(Action::Next, 1300).is_none());
    model.idle();
    assert!(model.prepare_control(Action::Play, 1300).is_none());
}

#[test]
fn transport_receipts_are_bounded_and_detached_host_never_claims_delivery() {
    use oly_platform_win::smtc::{ControlResult, EventSink};
    use oly_platform_win::smtc::control::Action;
    use oly_gpui_app::commands::{Inbox, ShellSink, ShellHost, HostCommand, UiEvent};
    let sink = ShellSink::new(Arc::new(Mutex::new(Inbox::default())));
    for request_id in 0..100 { sink.control_result(ControlResult { request_id, accepted: false }); }
    assert!(!sink.inbox().lock().unwrap().is_empty());
    let events = sink.inbox().lock().unwrap().drain();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], UiEvent::Control(ControlResult { request_id: 99, accepted: false })));
    assert!(sink.inbox().lock().unwrap().is_empty());
    let mut model = LiveModel::default();
    let mut song = packet("Song", 0.0); song.ctrl.playpause = true;
    model.feed(song, 1.0);
    let request = model.prepare_control(Action::Pause, 1).unwrap();
    assert!(!ShellHost::detached().dispatch(HostCommand::GuardedTransport(request)));
}

#[test]
fn unix_clock_keeps_apple_and_default_heartbeats_locked_while_elapsed_time_oscillates() {
    // Reproduces live-mode lyric progress instability when process-relative
    // elapsed time (~0..10_000 ms) is mixed with Unix-epoch SMTC timestamps
    // (~1.79e12 ms), discarding `(now - last_updated_unix_ms)` sample age.
    let epoch = 1_793_919_296_000.0;
    let unix_packet = |app: &str, position_ms: f64, last_updated_unix_ms: f64| PlayerPacket {
        app: app.into(),
        title: "Cruel World".into(),
        artist: "Taylor Swift".into(),
        album: "Test".into(),
        duration_ms: 200_000.0,
        position_ms,
        last_updated_unix_ms,
        position_available: Some(true),
        rate: 1.0,
        status: Some(oly_contracts::PlaybackStatus::Code(4)),
        timeline_source: "smtc".into(),
        ..Default::default()
    };

    // Apple Music (`APPLE_PROFILE`: offset=1400, tolerance=1000) emits updates
    // at irregular cadence while WinRT sends 500ms heartbeats; without Unix
    // sample-age compensation `(now - last_updated_unix_ms)`, stale updates
    // rewind backwards and fresh updates surge forwards.
    let apple_steps = [
        // (elapsed_ms, reported_pos_ms, packet_updated_unix_ms, expected_media_pos)
        (100.0, 10_000.0, epoch + 80.0, 10_020.0),
        (1_600.0, 10_400.0, epoch + 450.0, 11_550.0),
        (2_100.0, 12_000.0, epoch + 2_080.0, 12_020.0),
        (3_100.0, 12_900.0, epoch + 2_980.0, 13_020.0),
    ];

    let mut fixed = LiveModel::default();
    let mut broken = LiveModel::default();
    let mut broken_positions = Vec::new();
    for &(elapsed, pos, updated, _expected_media) in &apple_steps {
        let pkt = unix_packet("AppleInc.AppleMusicWin_nzyj5cx40ttqa!App", pos, updated);
        let unix_now = epoch + elapsed;
        let locked_media = apple_steps[0].3 + (elapsed - apple_steps[0].0);
        fixed.feed(pkt.clone(), unix_now);
        fixed.tick(unix_now);
        assert!(
            (fixed.position(unix_now) - (locked_media + 1_400.0)).abs() < 1e-6,
            "Unix-aligned Apple clock must stay locked at +1400ms offset without lagging or rushing"
        );
        assert!(
            (fixed.media_position(unix_now) - locked_media).abs() < 1e-6,
            "Unix-aligned media position must advance strictly linearly"
        );

        broken.feed(pkt, elapsed);
        broken.tick(elapsed);
        broken_positions.push(broken.position(elapsed));
    }
    // With elapsed time, losing the 1.15s sample age at 1.6s breaks
    // `tolerance_ms = 1000` and rewinds backwards, then 2.1s surges forward.
    assert!(
        broken_positions[1] < broken_positions[0] + 1_000.0,
        "expected broken elapsed clock to lag/rewind on stale Apple sample: {broken_positions:?}"
    );
    assert!(
        broken_positions[3] - broken_positions[2] > 1_000.0,
        "expected broken elapsed clock to jump ahead after rewind: {broken_positions:?}"
    );

    // Default player profile (`MSEdge`, tolerance=0) also stays steady on
    // Unix time across samples whose age exceeds ProgressClock's 250ms gate.
    let edge_steps = [
        (100.0, 10_000.0, epoch + 80.0, 10_020.0),
        (600.0, 10_160.0, epoch + 240.0, 10_520.0),
        (1_100.0, 11_000.0, epoch + 1_080.0, 11_020.0),
    ];
    let mut fixed_edge = LiveModel::default();
    let mut broken_edge = LiveModel::default();
    let mut broken_edge_positions = Vec::new();
    for &(elapsed, pos, updated, expected_media) in &edge_steps {
        let pkt = unix_packet("MSEdge", pos, updated);
        let unix_now = epoch + elapsed;
        fixed_edge.feed(pkt.clone(), unix_now);
        fixed_edge.tick(unix_now);
        assert!((fixed_edge.position(unix_now) - expected_media).abs() < 1e-6);

        broken_edge.feed(pkt, elapsed);
        broken_edge.tick(elapsed);
        broken_edge_positions.push(broken_edge.position(elapsed));
    }
    assert!(
        broken_edge_positions[1] < broken_edge_positions[0] + 250.0,
        "expected broken default clock to snap backward when lost age > 250ms: {broken_edge_positions:?}"
    );
    assert!(
        broken_edge_positions[2] - broken_edge_positions[1] > 700.0,
        "expected broken default clock to jump forward on fresh sample: {broken_edge_positions:?}"
    );
}

#[cfg(windows)]
#[test]
fn resolve_player_volume_script_materializes_script_into_data_root() {
    let dir = std::env::temp_dir().join(format!(
        "oly_vol_script_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let path = oly_gpui_app::live::resolve_player_volume_script(&dir)
        .expect("bundled PlayerVolume.ps1 must resolve");
    let body = std::fs::read_to_string(&path).expect("script must be readable");
    assert!(!body.contains("AppleMusic"), "AppleMusic should fall back directly to mixer");
    assert!(body.contains("Spotify"), "script must include Spotify support");
    assert!(body.contains("QQMusic"), "script must include QQMusic support");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(windows)]
#[test]
fn motion_worker_disabled_and_cancel_behave_cleanly() {
    let dir = std::env::temp_dir().join(format!(
        "oly_motion_worker_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let worker = oly_gpui_app::live::MotionWorker::start(dir.join("motion-cache"));
    worker.request(
        7,
        "track-7".into(),
        "Style".into(),
        "Taylor Swift".into(),
        "1989".into(),
        false,
        500,
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut answer = None;
    while Instant::now() < deadline {
        if let Some(a) = worker.take() {
            answer = Some(a);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let answer = answer.expect("disabled motion request should finish immediately offline");
    assert_eq!(answer.generation, 7);
    assert_eq!(answer.file, None);
    worker.cancel();
    assert_eq!(worker.take(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apple_music_anchors_once_on_first_advance_and_ignores_pos_zero_timestamp_churn() {
    let mut m = LiveModel::default();
    let apple = |title: &str, pos: f64, upd: f64| PlayerPacket {
        title: title.into(),
        artist: "Birdy".into(),
        app: "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App".into(),
        position_ms: pos,
        duration_ms: 181_000.0,
        last_updated_unix_ms: upd,
        status: Some(PlaybackStatus::Code(4)),
        ..Default::default()
    };
    // Track starts at pos=0; Apple Music keeps refreshing LastUpdatedTime every ~100ms
    // while buffering / before crossing 1000ms.
    m.feed(apple("Wild Horses", 0.0, 10_000.0), 10_030.0);
    m.feed(apple("Wild Horses", 0.0, 11_020.0), 11_100.0);
    // Must NOT have been rewound back to ~80ms by LastUpdatedTime churn.
    let before_advance = m.media_position(11_100.0);
    assert!(
        before_advance > 900.0,
        "pos=0 timestamp churn must not rewind Apple Music clock, got {before_advance}"
    );
    // First real advance 0 -> 1000ms after ~1.8s buffering anchors cleanly once.
    m.feed(apple("Wild Horses", 1000.0, 11_800.0), 11_850.0);
    let anchored = m.media_position(11_850.0);
    assert!(
        (anchored - 1050.0).abs() < 20.0,
        "first 0->1000ms advance should anchor cleanly, got {anchored}"
    );
    // Subsequent whole-second quantization jitter within ±1000ms stays locked.
    m.feed(apple("Wild Horses", 2000.0, 13_400.0), 13_500.0);
    let steady = m.media_position(13_500.0);
    assert!(
        (steady - (anchored + 1650.0)).abs() < 20.0,
        "steady playback after initial anchor must remain locked, got {steady}"
    );
}
