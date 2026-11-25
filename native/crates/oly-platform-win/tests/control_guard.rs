use oly_platform_win::smtc::control::{Action, ControlRequest, SeekGuard, control_guard};
fn request(action: Action) -> ControlRequest {
    ControlRequest {
        action,
        request_id: 1,
        deadline_unix_ms: 2000,
        identity: SeekGuard {
            app: "Player".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
        },
    }
}
#[test]
fn rejects_expired_unsupported_and_changed_song_commands() {
    let req = request(Action::Pause);
    assert!(control_guard(&req, 1000, &req.identity, true));
    assert!(!control_guard(&req, 2000, &req.identity, true));
    assert!(!control_guard(&req, 1000, &req.identity, false));
    for field in 0..4 {
        let mut changed = req.identity.clone();
        match field {
            0 => changed.app.push('x'),
            1 => changed.title.push('x'),
            2 => changed.artist.push('x'),
            _ => changed.album.push('x'),
        }
        assert!(!control_guard(&req, 1000, &changed, true));
    }
}
#[test]
fn missing_identity_is_never_a_writable_target() {
    let mut req = request(Action::Play);
    req.identity.title.clear();
    assert!(!control_guard(&req, 1000, &req.identity, true));
    req = request(Action::Next);
    req.identity.app.clear();
    assert!(!control_guard(&req, 1000, &req.identity, true));
}
