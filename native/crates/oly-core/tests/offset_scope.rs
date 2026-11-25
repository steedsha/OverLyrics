//! Task 11 — per-track lyric offset scope.
//!
//! `src/state/offsets.ts` is the contract. Two properties decide whether a
//! nudge helps or quietly breaks a different song, so both are pinned here:
//!
//! * an offset is scoped per **provider**, because the same song can be a
//!   second off on QQ and perfect on AMLLDB;
//! * a track that has both an album and an artist shares one offset with the
//!   whole album, and everything else falls back to the song key.
//!
//! The Apple Music fold (`Artist — Album` in the artist field, empty album) is
//! the one player convention the reference recovers, so it is exercised too.

use std::path::PathBuf;

use oly_core::offsets::{LyricOffsets, OffsetScope, offset_scope, song_of_packet};
use oly_contracts::PlayerPacket;
use serde_json::json;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oly-offsets-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn packet(title: &str, artist: &str, album: &str, album_artist: &str) -> PlayerPacket {
    PlayerPacket {
        title: title.into(),
        artist: artist.into(),
        album: album.into(),
        album_artist: album_artist.into(),
        app: "Spotify.exe".into(),
        duration_ms: 210_000.0,
        ..Default::default()
    }
}

// ------------------------------------------------------------------ scope --

#[test]
fn an_album_track_shares_one_scope_across_its_songs() {
    let first = song_of_packet(Some(&packet(
        "Track One",
        "The Band",
        "The Album",
        "The Band",
    )))
    .expect("song");
    let second = song_of_packet(Some(&packet(
        "Track Two",
        "The Band",
        "The Album",
        "The Band",
    )))
    .expect("song");

    let a = offset_scope(&first, "kugou").expect("scope");
    let b = offset_scope(&second, "kugou").expect("scope");
    assert_eq!(a.scope, OffsetScope::Album);
    assert_eq!(a.key, b.key, "one album shares a single offset");
    assert_eq!(a.album, "The Album");

    // A different provider is a different nudge.
    let other = offset_scope(&first, "qq").expect("scope");
    assert_ne!(a.key, other.key);
}

#[test]
fn a_track_without_an_album_scopes_to_the_song() {
    let song = song_of_packet(Some(&packet("Single", "The Band", "", ""))).expect("song");
    let scope = offset_scope(&song, "kugou").expect("scope");
    assert_eq!(scope.scope, OffsetScope::Song);

    let other = song_of_packet(Some(&packet("Another", "The Band", "", ""))).expect("song");
    let other = offset_scope(&other, "kugou").expect("scope");
    assert_ne!(scope.key, other.key);
}

#[test]
fn placeholder_album_names_do_not_create_a_shared_scope() {
    for placeholder in ["unknown album", "Unknown", "未知专辑", "不详"] {
        let song = song_of_packet(Some(&packet("Song", "Artist", placeholder, "Artist")))
            .expect("song");
        let scope = offset_scope(&song, "kugou").expect("scope");
        assert_eq!(
            scope.scope,
            OffsetScope::Song,
            "{placeholder} must not become an album key"
        );
    }
}

#[test]
fn apple_music_folds_artist_and_album_into_a_scope() {
    // Apple leaves AlbumTitle empty and puts "Artist — Album" in the artist
    // field. Reproducing that convention is the only way those tracks get an
    // album-scoped offset instead of one key per song.
    let mut p = packet("Track", "Lorde — Melodrama", "", "");
    p.app = "AppleMusic.exe".into();
    let song = song_of_packet(Some(&p)).expect("song");
    let scope = offset_scope(&song, "amlldb").expect("scope");
    assert_eq!(scope.scope, OffsetScope::Album);
    assert_eq!(scope.album, "Melodrama");

    // The same shape on a non-Apple player is not folded: another player may
    // legitimately have a dash in the artist name.
    let mut other = packet("Track", "Lorde — Melodrama", "", "");
    other.app = "Spotify.exe".into();
    let other = song_of_packet(Some(&other)).expect("song");
    let other = offset_scope(&other, "amlldb").expect("scope");
    assert_eq!(other.scope, OffsetScope::Song);
}

#[test]
fn nothing_playing_means_no_scope() {
    assert!(song_of_packet(None).is_none());
    let idle = PlayerPacket {
        idle: true,
        ..packet("Song", "Artist", "Album", "Artist")
    };
    assert!(song_of_packet(Some(&idle)).is_none());

    let blank = packet("   ", "Artist", "Album", "Artist");
    assert!(song_of_packet(Some(&blank)).is_none());

    let song = song_of_packet(Some(&packet("Song", "Artist", "Album", "Artist"))).expect("song");
    assert!(offset_scope(&song, "").is_none());
    assert!(offset_scope(&song, "   ").is_none());
}

// ------------------------------------------------------------- storage --

#[test]
fn a_nudge_round_trips_through_the_file_and_is_clamped() {
    let dir = temp_dir("nudge");
    let offsets = LyricOffsets::load(&dir).expect("load");
    let song = song_of_packet(Some(&packet("Song", "Artist", "Album", "Artist"))).expect("song");

    assert_eq!(offsets.get(&song, "kugou").ms, 0.0);
    let after = offsets.nudge(&song, "kugou", 320.0).expect("nudge");
    assert_eq!(after.ms, 320.0);

    let again = LyricOffsets::load(&dir).expect("reload");
    assert_eq!(again.get(&song, "kugou").ms, 320.0);

    // Positive means "lyrics earlier" and keeps accumulating.
    let more = again.nudge(&song, "kugou", -120.0).expect("nudge");
    assert_eq!(more.ms, 200.0);

    // A runaway script cannot park the offset minutes away from the audio.
    let clamped = again.nudge(&song, "kugou", 10_000.0).expect("nudge");
    assert_eq!(clamped.ms, 5000.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_write_rolls_the_value_back() {
    let dir = temp_dir("rollback");
    let offsets = LyricOffsets::load(&dir).expect("load");
    let song = song_of_packet(Some(&packet("Song", "Artist", "Album", "Artist"))).expect("song");
    offsets.nudge(&song, "kugou", 100.0).expect("nudge");

    // Occupy the temp path so the atomic write cannot land.
    let blocker = dir.join("lyric-offsets.json.tmp");
    std::fs::create_dir_all(&blocker).unwrap();
    let err = offsets.nudge(&song, "kugou", 50.0);
    assert!(err.is_err(), "a write that cannot land must report failure");
    assert_eq!(
        offsets.get(&song, "kugou").ms,
        100.0,
        "the rejected nudge must not be visible"
    );

    std::fs::remove_dir_all(&blocker).unwrap();
    assert_eq!(offsets.nudge(&song, "kugou", 50.0).expect("nudge").ms, 150.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_legacy_v1_file_is_read_but_rewritten_in_the_current_shape() {
    let dir = temp_dir("legacy");
    std::fs::create_dir_all(&dir).unwrap();
    // Build the fixture through serde_json so the escape round-trips exactly the
    // way the reference build wrote it: the key text carries `\u0001`, and the
    // file's JSON layer escapes that backslash a second time.
    let identity = format!("song{}artist", '\u{1}');
    let key = serde_json::to_string(&["song", identity.as_str(), "kugou"]).expect("key");
    let mut offsets = serde_json::Map::new();
    offsets.insert(key.clone(), json!(-150));
    let doc = json!({ "version": 1, "offsets": offsets });
    std::fs::write(
        dir.join("lyric-offsets.json"),
        serde_json::to_string(&doc).expect("encode"),
    )
    .unwrap();

    let offsets = LyricOffsets::load(&dir).expect("load");
    let song = song_of_packet(Some(&packet("Song", "Artist", "", ""))).expect("song");
    assert_eq!(offsets.get(&song, "kugou").ms, -150.0);

    // Any later nudge rewrites the file in the current shape.
    offsets.nudge(&song, "kugou", 10.0).expect("nudge");
    let text = std::fs::read_to_string(dir.join("lyric-offsets.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(doc.get("offsets").is_none(), "v1 shape must not survive a write");
    assert_eq!(
        doc.get(key).and_then(|e| e.get("offsetMs")).and_then(|v| v.as_f64()),
        Some(-140.0)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_unscoped_legacy_file_is_blanked_but_not_deleted() {
    let dir = temp_dir("song-offsets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("song-offsets.json"), r#"{"Song\u0001Artist":-300}"#).unwrap();

    let _ = LyricOffsets::load(&dir).expect("load");
    // Keys carry no provider, so the numbers cannot be converted; the file is
    // emptied rather than removed so a rollback cannot inherit stale data.
    assert_eq!(
        std::fs::read_to_string(dir.join("song-offsets.json")).unwrap(),
        "{}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_broken_file_is_treated_as_empty() {
    let dir = temp_dir("broken");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lyric-offsets.json"), "{oops").unwrap();

    let offsets = LyricOffsets::load(&dir).expect("a broken file must not stop playback");
    let song = song_of_packet(Some(&packet("Song", "Artist", "Album", "Artist"))).expect("song");
    assert_eq!(offsets.get(&song, "kugou").ms, 0.0);
    assert_eq!(offsets.nudge(&song, "kugou", 75.0).expect("nudge").ms, 75.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clear_removes_every_saved_offset() {
    let dir = temp_dir("clear");
    let offsets = LyricOffsets::load(&dir).expect("load");
    let song = song_of_packet(Some(&packet("Song", "Artist", "Album", "Artist"))).expect("song");
    offsets.nudge(&song, "kugou", 400.0).expect("nudge");
    offsets.clear().expect("clear");

    let again = LyricOffsets::load(&dir).expect("reload");
    assert_eq!(again.get(&song, "kugou").ms, 0.0);
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("lyric-offsets.json")).unwrap())
            .unwrap();
    assert_eq!(doc, json!({}));

    let _ = std::fs::remove_dir_all(&dir);
}
