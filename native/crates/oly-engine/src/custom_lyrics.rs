//! Local custom-imported lyrics store.
//!
//! Imported files (`.lrc`, `.ttml`, `.qrc`, `.yrc`, `.txt`) are parsed and
//! stored inside the application's own `custom-lyrics/` directory, bound to the
//! track key. Deleting an imported entry only removes it from the app's list
//! and store — never the user's original local file.

use std::fs;
use std::path::{Path, PathBuf};

use oly_contracts::LyricDoc;
use serde::{Deserialize, Serialize};

use crate::doc::{build_doc, RawLyrics, ScoredCandidate};
use crate::parsers::parse_imported_lyrics;
use crate::types::{BuiltDoc, Candidate};

pub const SUPPORTED_EXTENSIONS: &[&str] = &["lrc", "ttml", "qrc", "yrc", "txt"];

pub fn is_supported_lyric_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            let low = ext.to_ascii_lowercase();
            SUPPORTED_EXTENSIONS.contains(&low.as_str())
        })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomLyricEntry {
    pub id: String,
    pub track_key: String,
    pub file_name: String,
    pub ext: String,
    pub word_timed: bool,
    pub has_translation: bool,
    pub line_count: usize,
    pub imported_at_ms: u64,
    pub built: BuiltDoc,
}

impl CustomLyricEntry {
    pub fn doc(&self) -> LyricDoc {
        self.built.to_lyric_doc()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CustomLyricsIndex {
    entries: Vec<CustomLyricEntry>,
}

pub struct CustomLyricsStore {
    root: PathBuf,
    entries: Vec<CustomLyricEntry>,
}

impl CustomLyricsStore {
    pub fn load(data_root: impl Into<PathBuf>) -> Self {
        let root = data_root.into().join("custom-lyrics");
        let index_path = root.join("index.json");
        let entries = fs::read_to_string(&index_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<CustomLyricsIndex>(&raw).ok())
            .map(|idx| idx.entries)
            .unwrap_or_default();
        Self { root, entries }
    }

    pub fn entries_for(&self, track_key: &str) -> Vec<&CustomLyricEntry> {
        if track_key.trim().is_empty() {
            return Vec::new();
        }
        self.entries
            .iter()
            .filter(|e| e.track_key == track_key)
            .collect()
    }

    pub fn get_doc(&self, id: &str) -> Option<LyricDoc> {
        self.entries.iter().find(|e| e.id == id).map(CustomLyricEntry::doc)
    }

    pub fn import_file(
        &mut self,
        track_key: &str,
        title: &str,
        artist: &str,
        duration_ms: Option<f64>,
        path: &Path,
        now_ms: u64,
    ) -> Result<CustomLyricEntry, String> {
        if track_key.trim().is_empty() {
            return Err("当前没有正在播放的曲目".into());
        }
        if !is_supported_lyric_file(path) {
            return Err("不支持的歌词文件格式".into());
        }
        let raw = fs::read_to_string(path).map_err(|e| format!("读取歌词文件失败: {e}"))?;
        let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
            .to_ascii_lowercase();
        let parsed = parse_imported_lyrics(raw, &ext)
            .ok_or_else(|| "歌词文件为空或无法解析".to_string())?;
        if parsed.lines.is_empty() {
            return Err("歌词文件没有可显示的行".into());
        }

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("custom.lrc")
            .to_string();

        let id = format!(
            "custom-{}-{}",
            now_ms,
            self.entries.len().wrapping_add(1)
        );
        let scored = ScoredCandidate {
            id: id.clone(),
            best: Candidate {
                source: "custom".into(),
                id: id.clone(),
                title: if title.trim().is_empty() {
                    file_name.clone()
                } else {
                    title.to_string()
                },
                artist: (!artist.trim().is_empty()).then(|| artist.to_string()),
                duration_sec: duration_ms.map(|d| d / 1000.0),
                percent: Some(100.0),
                cap_trans: Some(parsed.lines.iter().any(|l| l.trans.is_some())),
                cap_word: Some(parsed.has_word_level),
                best_of_source: Some(true),
                ..Default::default()
            },
        };
        let built = build_doc(
            &scored,
            &RawLyrics {
                raw: raw.to_string(),
                ..Default::default()
            },
        )
        .ok_or_else(|| "构建歌词文档失败".to_string())?;
        let doc = built.to_lyric_doc();

        let word_timed = doc.lines.iter().any(|l| !l.words.is_empty());
        let has_translation = doc.lines.iter().any(|l| !l.trans.trim().is_empty());

        let entry = CustomLyricEntry {
            id,
            track_key: track_key.to_string(),
            file_name,
            ext,
            word_timed,
            has_translation,
            line_count: doc.lines.len(),
            imported_at_ms: now_ms,
            built,
        };

        // Prepend so newly imported lyrics always appear at the top of the list.
        self.entries.insert(0, entry.clone());
        self.save();
        Ok(entry)
    }

    /// Removes an imported entry from the list and store only; never touches the original local file.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        if self.entries.len() != before {
            self.save();
            true
        } else {
            false
        }
    }

    fn save(&self) {
        if fs::create_dir_all(&self.root).is_err() {
            return;
        }
        let path = self.root.join("index.json");
        let tmp = self.root.join("index.tmp");
        let payload = CustomLyricsIndex {
            entries: self.entries.clone(),
        };
        if let Ok(json) = serde_json::to_vec(&payload) {
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(&tmp, path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_prepends_and_removes_without_deleting_original_file() {
        let base = std::env::temp_dir().join(format!("oly-custom-lyrics-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let local_lrc = base.join("song.lrc");
        fs::write(&local_lrc, "[00:01.00]Hello world\n[00:03.00]Second line").unwrap();

        let mut store = CustomLyricsStore::load(&base);
        let first = store
            .import_file("track-a", "Song", "Artist", Some(120_000.0), &local_lrc, 100)
            .expect("import lrc");
        let local_yrc = base.join("song.yrc");
        fs::write(&local_yrc, "[1000,2000](1000,500,0)Hel(1500,500,0)lo").unwrap();
        let second = store
            .import_file("track-a", "Song", "Artist", Some(120_000.0), &local_yrc, 200)
            .expect("import yrc");

        let list = store.entries_for("track-a");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second.id);
        assert!(list[0].word_timed);
        assert_eq!(list[1].id, first.id);

        assert!(store.remove(&second.id));
        assert_eq!(store.entries_for("track-a").len(), 1);
        assert!(local_yrc.exists(), "original local file must never be deleted");

        let _ = fs::remove_dir_all(&base);
    }
}
