//! Per-track lyric timing offsets, persisted to `lyric-offsets.json`.
//!
//! Ported 1:1 from `src/state/offsets.ts`. A source's first line often lands a
//! few hundred milliseconds early or late (a live recording, a remaster with a
//! longer intro); the user nudges it once and expects the fix to survive a
//! restart. Offsets therefore live in their own file rather than in the config:
//! they are tuned per track, not per install.
//!
//! Scope is decided once, from the song, not from the source's own idea of an
//! album:
//!
//! * **album** — the track has both an album and an artist, so every track on
//!   that album shares one offset;
//! * **song** — anything else (singles without an album tag, placeholder album
//!   names), keyed on `trackKey`.
//!
//! Every save goes through `.tmp` + rename, and a failed write rolls the
//! in-memory value back: a panel that shows "+0.35 s" while the disk still says
//! 0 would be worse than no control at all.
//!
//! One deliberate deviation from the TypeScript: the reference normalises
//! identity strings with Unicode NFKC before comparing, and this port only
//! lowercases and collapses whitespace. No shipped key depends on compatibility
//! folding, and the crate carries no Unicode normalisation dependency; the
//! difference is recorded in `docs/gpui/RESULTS.md`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use oly_contracts::PlayerPacket;
use serde_json::{Map, Value};

/// The on-disk file, in the shape the reference build writes.
pub const OFFSET_FILE: &str = "lyric-offsets.json";
/// The pre-scope file: read for nothing, blanked on sight.
pub const LEGACY_OFFSET_FILE: &str = "song-offsets.json";
/// Guard against a stuck key or a runaway script; the original had no bound.
pub const MAX_ABS_MS: i64 = 5000;

/// Keys that must never travel through a load or a save.
const FORBIDDEN_KEYS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// One saved offset, in the on-disk shape.
#[derive(Clone, Debug, PartialEq)]
struct OffsetEntry {
    offset_ms: i64,
    /// UNIX milliseconds of the last nudge; kept for support, never read back.
    updated_at: i64,
}

/// What a nudge applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetScope {
    Album,
    Song,
}

impl OffsetScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Song => "song",
        }
    }
}

/// The song a nudge belongs to, or `None` when nothing is playing.
#[derive(Clone, Debug, PartialEq)]
pub struct SongRef {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub duration_ms: f64,
    pub app: String,
}

/// The storage key for one (song, provider) pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OffsetContext {
    /// The on-disk key: `JSON.stringify([scope, ...identity, provider])`.
    pub key: String,
    pub scope: OffsetScope,
    /// Album display name, for a panel that says "整张专辑" instead of silently
    /// changing more than the user expected.
    pub album: String,
    pub source: String,
}

/// What the panel renders: the resolved scope plus the stored value.
#[derive(Clone, Debug, PartialEq)]
pub struct OffsetSnapshot {
    /// `None` when there is no song or no provider to key on.
    pub context: Option<OffsetContext>,
    pub ms: f64,
}

/// Why a write could not land. The in-memory value has already been restored.
#[derive(Debug)]
pub enum OffsetError {
    Write(std::io::Error),
    Encode(serde_json::Error),
}

impl fmt::Display for OffsetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(err) => write!(f, "lyric offset save failed: {err}"),
            Self::Encode(err) => write!(f, "lyric offset encode failed: {err}"),
        }
    }
}

impl std::error::Error for OffsetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Write(err) => Some(err),
            Self::Encode(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for OffsetError {
    fn from(err: std::io::Error) -> Self {
        Self::Write(err)
    }
}

impl From<serde_json::Error> for OffsetError {
    fn from(err: serde_json::Error) -> Self {
        Self::Encode(err)
    }
}

/// NFKC + lowercase + collapsed spaces: the identity the engine uses.
///
/// NFKC itself is not applied (see the module note); lowercasing and whitespace
/// collapsing cover every identity the shipped keys are built from.
fn norm(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The lead artist, with Apple Music's "Artist — Album" suffix stripped.
fn primary_artist(song: &SongRef) -> String {
    let raw = if song.artist.is_empty() {
        String::new()
    } else {
        song.artist.clone()
    };
    let first = raw
        .split(['\u{2014}', '\u{2013}'])
        .next()
        .unwrap_or("")
        .trim();
    // A plain " - " separator (with spaces) is the other dash form the engine
    // accepts; splitting on it keeps "Love-Part 1" intact.
    let first = first.split(" - ").next().unwrap_or("").trim();
    if !first.is_empty() {
        first.to_string()
    } else {
        // No separator: a lone em dash is not a separator either, so fall back
        // to the whole trimmed value.
        raw.split(" - ").next().unwrap_or("").trim().to_string()
    }
}

/// Cache identity: title + lead artist, lowercased with all whitespace removed.
fn track_key(song: &SongRef) -> String {
    let norm = |s: &str| {
        s.to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    format!("{}\u{1}{}", norm(&song.title), norm(&primary_artist(song)))
}

/// The song a packet describes, or `None` when nothing is playing.
pub fn song_of_packet(packet: Option<&PlayerPacket>) -> Option<SongRef> {
    let packet = packet?;
    if packet.idle {
        return None;
    }
    let title = packet.title.trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some(SongRef {
        title,
        artist: packet.artist.trim().to_string(),
        album: packet.album.trim().to_string(),
        album_artist: packet.album_artist.trim().to_string(),
        duration_ms: if packet.duration_ms.is_finite() {
            packet.duration_ms
        } else {
            0.0
        },
        app: packet.app.clone(),
    })
}

/// Placeholder album names: a compilation or a player with no album tag writes
/// one of these, and keying an album scope on it would merge unrelated songs.
fn is_placeholder_album(album: &str) -> bool {
    let lower = album.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "unknown" | "unknown album" | "未知" | "未知专辑" | "不详"
    )
}

/// Apple Music can leave `AlbumTitle` empty and put artist + album in the
/// `Artist` field. Recover only that known player convention.
fn apple_fold(song: &SongRef) -> Option<(String, String)> {
    if !song.app.to_lowercase().contains("applemusic") {
        return None;
    }
    let (left, right) = song.artist.split_once(" \u{2014} ")?;
    let left = left.trim();
    let right = right.trim();
    if left.is_empty() || right.is_empty() {
        return None;
    }
    Some((left.to_string(), right.to_string()))
}

/// The storage key for one (song, provider) pair.
pub fn offset_scope(song: &SongRef, provider: &str) -> Option<OffsetContext> {
    // A whitespace-only title is not a song identity: it would key every such
    // packet onto the same entry.
    if song.title.trim().is_empty() {
        return None;
    }
    let source = provider.trim();
    if source.is_empty() {
        return None;
    }

    let mut album_name = song.album.trim().to_string();
    if is_placeholder_album(&album_name) {
        album_name.clear();
    }
    let mut artist = if !song.album_artist.trim().is_empty() {
        norm(&song.album_artist)
    } else {
        norm(&primary_artist(song))
    };
    if let Some((folded_artist, folded_album)) = apple_fold(song) {
        if album_name.is_empty() {
            album_name = folded_album.clone();
        }
        let album_matches = norm(&album_name) == norm(&folded_album);
        let artist_unset = song.album_artist.trim().is_empty()
            || norm(&song.album_artist) == norm(&song.artist);
        if album_matches && artist_unset {
            artist = norm(&folded_artist);
        }
    }

    let album = norm(&album_name);
    let scope = if !album.is_empty() && !artist.is_empty() {
        OffsetScope::Album
    } else {
        OffsetScope::Song
    };
    let identity: Vec<String> = match scope {
        OffsetScope::Album => vec![artist, album],
        OffsetScope::Song => vec![track_key(song)],
    };
    let mut parts: Vec<String> = Vec::with_capacity(identity.len() + 2);
    parts.push(scope.as_str().to_string());
    parts.extend(identity);
    parts.push(source.to_string());
    let key = serde_json::to_string(&parts).ok()?;

    Some(OffsetContext {
        key,
        scope,
        album: album_name,
        source: source.to_string(),
    })
}

/// The per-song key an album scope falls back to while an old per-song value
/// exists. Once an album entry exists it always wins for every track.
fn song_scope_key(song: &SongRef, source: &str) -> Option<String> {
    let parts = vec![
        "song".to_string(),
        track_key(song),
        source.to_string(),
    ];
    serde_json::to_string(&parts).ok()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The app-wide store. Construct one against an explicit directory.
pub struct LyricOffsets {
    dir: PathBuf,
    values: Mutex<BTreeMap<String, OffsetEntry>>,
}

impl LyricOffsets {
    /// Read the file once. A truncated or hand-broken file must not stop
    /// playback, so it loads as empty and the next nudge rewrites it.
    pub fn load(dir: &Path) -> Result<Self, OffsetError> {
        let mut values: BTreeMap<String, OffsetEntry> = BTreeMap::new();

        if let Ok(text) = std::fs::read_to_string(dir.join(OFFSET_FILE)) {
            if let Ok(Value::Object(root)) = serde_json::from_str::<Value>(&text) {
                for (key, value) in &root {
                    if FORBIDDEN_KEYS.contains(&key.as_str()) {
                        continue;
                    }
                    let Some(object) = value.as_object() else {
                        continue;
                    };
                    let Some(ms) = object.get("offsetMs").and_then(Value::as_f64) else {
                        continue;
                    };
                    if !ms.is_finite() {
                        continue;
                    }
                    let at = object
                        .get("updatedAt")
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite())
                        .unwrap_or(0.0);
                    values.insert(
                        key.clone(),
                        OffsetEntry {
                            offset_ms: ms.round() as i64,
                            updated_at: at.round() as i64,
                        },
                    );
                }
                // The reference build wrote `{ version: 1, offsets: { key: ms } }`.
                // Those numbers are real tuning the user did: read them across,
                // then let the next save rewrite the file in the current shape.
                if root.get("version").and_then(Value::as_u64) == Some(1) {
                    if let Some(legacy) = root.get("offsets").and_then(Value::as_object) {
                        for (key, value) in legacy {
                            let Some(ms) = value.as_f64() else { continue };
                            if !ms.is_finite() {
                                continue;
                            }
                            values.entry(key.clone()).or_insert(OffsetEntry {
                                offset_ms: ms.round() as i64,
                                updated_at: 0,
                            });
                        }
                    }
                }
            }
        }

        // `song-offsets.json` was a single unscoped number per song and cannot be
        // converted (its keys carry no provider). Blank it, but do not delete it:
        // a user who rolls back must not inherit stale data.
        let legacy_path = dir.join(LEGACY_OFFSET_FILE);
        if let Ok(legacy_text) = std::fs::read_to_string(&legacy_path) {
            let trimmed = legacy_text.trim();
            if !trimmed.is_empty() && trimmed != "{}" {
                let _ = std::fs::write(&legacy_path, "{}");
            }
        }

        Ok(Self {
            dir: dir.to_path_buf(),
            values: Mutex::new(values),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.dir
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, OffsetEntry>> {
        match self.values.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Current offset for a song+source. Zero when nothing is stored.
    pub fn get(&self, song: &SongRef, provider: &str) -> OffsetSnapshot {
        let Some(context) = offset_scope(song, provider) else {
            return OffsetSnapshot {
                context: None,
                ms: 0.0,
            };
        };
        let values = self.lock();
        let direct = values.get(&context.key).map(|e| e.offset_ms as f64);
        let ms = direct
            .or_else(|| {
                if context.scope == OffsetScope::Album {
                    song_scope_key(song, &context.source)
                        .and_then(|key| values.get(&key).map(|e| e.offset_ms as f64))
                } else {
                    None
                }
            })
            .unwrap_or(0.0);
        OffsetSnapshot {
            context: Some(context),
            ms,
        }
    }

    /// Add `delta` milliseconds and persist.
    ///
    /// Positive means "lyrics earlier": the adapter adds the offset to the
    /// player position before looking the line up, so a positive number reaches
    /// further into the document. Returns the stored value; on a write failure
    /// the previous value is restored and the error is reported.
    pub fn nudge(
        &self,
        song: &SongRef,
        provider: &str,
        delta: f64,
    ) -> Result<OffsetSnapshot, OffsetError> {
        let Some(context) = offset_scope(song, provider) else {
            return Ok(OffsetSnapshot {
                context: None,
                ms: 0.0,
            });
        };
        if !delta.is_finite() {
            let base = self.get(song, provider).ms;
            return Ok(OffsetSnapshot {
                context: Some(context),
                ms: base,
            });
        }

        let mut values = self.lock();
        let base = values
            .get(&context.key)
            .map(|e| e.offset_ms as f64)
            .or_else(|| {
                song_scope_key(song, &context.source)
                    .and_then(|key| values.get(&key).map(|e| e.offset_ms as f64))
            })
            .unwrap_or(0.0);
        let next = (base + delta).round().clamp(-(MAX_ABS_MS as f64), MAX_ABS_MS as f64);
        if next == base {
            return Ok(OffsetSnapshot {
                context: Some(context),
                ms: base,
            });
        }

        let previous = values.get(&context.key).cloned();
        values.insert(
            context.key.clone(),
            OffsetEntry {
                offset_ms: next as i64,
                updated_at: now_ms(),
            },
        );
        let snapshot = values.clone();
        if let Err(err) = self.save(&snapshot) {
            match previous {
                Some(entry) => {
                    values.insert(context.key.clone(), entry);
                }
                None => {
                    values.remove(&context.key);
                }
            }
            return Err(err);
        }
        Ok(OffsetSnapshot {
            context: Some(context),
            ms: next,
        })
    }

    /// Drop every saved offset; used by "restore defaults".
    pub fn clear(&self) -> Result<(), OffsetError> {
        let mut values = self.lock();
        let previous = values.clone();
        values.clear();
        if let Err(err) = self.save(&values) {
            *values = previous;
            return Err(err);
        }
        Ok(())
    }

    fn save(&self, values: &BTreeMap<String, OffsetEntry>) -> Result<(), OffsetError> {
        let mut doc = Map::new();
        for (key, entry) in values {
            let mut object = Map::new();
            object.insert("offsetMs".into(), Value::from(entry.offset_ms));
            object.insert("updatedAt".into(), Value::from(entry.updated_at));
            doc.insert(key.clone(), Value::Object(object));
        }
        let text = serde_json::to_string_pretty(&Value::Object(doc))?;
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(OFFSET_FILE);
        let tmp = self.dir.join(format!("{OFFSET_FILE}.tmp"));
        std::fs::write(&tmp, text)?;
        if let Err(err) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(OffsetError::Write(err));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(title: &str, artist: &str, album: &str) -> SongRef {
        SongRef {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            album_artist: artist.into(),
            duration_ms: 200_000.0,
            app: "Spotify.exe".into(),
        }
    }

    #[test]
    fn primary_artist_strips_the_apple_suffix_only() {
        assert_eq!(primary_artist(&song("t", "Lorde \u{2014} Melodrama", "")), "Lorde");
        assert_eq!(primary_artist(&song("t", "Love-Part 1", "")), "Love-Part 1");
        assert_eq!(primary_artist(&song("t", "A - B", "")), "A");
    }

    #[test]
    fn placeholder_albums_are_not_album_scopes() {
        for placeholder in ["Unknown", "unknown album", "未知专辑", "不详"] {
            assert!(is_placeholder_album(placeholder));
        }
        assert!(!is_placeholder_album("Melodrama"));
    }

    #[test]
    fn a_song_scope_key_is_json_around_title_and_artist() {
        let s = song("Song", "Artist", "");
        let context = offset_scope(&s, "kugou").expect("scope");
        assert_eq!(context.key, "[\"song\",\"song\\u0001artist\",\"kugou\"]");
    }
}
