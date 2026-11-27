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
    #[serde(default)]
    pub rel_path: Option<String>,
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

    pub fn has_any(&self) -> bool {
        !self.entries.is_empty()
    }

    pub fn root_dir(&self) -> &Path {
        &self.root
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
        album: &str,
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

        let folder_name = custom_folder_name(album, artist);
        let base_stem = {
            let clean_title = sanitize_path_component(title);
            if !clean_title.is_empty() {
                clean_title
            } else {
                let stem = Path::new(&file_name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("lyric");
                let clean = sanitize_path_component(stem);
                if clean.is_empty() {
                    "lyric".to_string()
                } else {
                    clean
                }
            }
        };
        let sub_dir = self.root.join(&folder_name);
        let _ = fs::create_dir_all(&sub_dir);
        let mut stored_name = format!("{base_stem}.{ext}");
        let mut dest_path = sub_dir.join(&stored_name);
        let mut suffix = 2usize;
        while dest_path.exists() {
            stored_name = format!("{base_stem} ({suffix}).{ext}");
            dest_path = sub_dir.join(&stored_name);
            suffix += 1;
        }
        let rel_path = if fs::write(&dest_path, raw.as_bytes()).is_ok() {
            Some(format!("{folder_name}/{stored_name}"))
        } else {
            None
        };

        let entry = CustomLyricEntry {
            id,
            track_key: track_key.to_string(),
            file_name,
            ext,
            word_timed,
            has_translation,
            line_count: doc.lines.len(),
            imported_at_ms: now_ms,
            rel_path,
            built,
        };

        // Prepend so newly imported lyrics always appear at the top of the list.
        self.entries.insert(0, entry.clone());
        self.save();
        Ok(entry)
    }

    /// Removes an imported entry from the list and store only; never touches the original local file.
    pub fn remove(&mut self, id: &str) -> bool {
        let removed_rel = self
            .entries
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.rel_path.clone());
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        if self.entries.len() != before {
            if let Some(rel) = removed_rel {
                let stored = self.root.join(&rel);
                let _ = fs::remove_file(&stored);
                if let Some(parent) = stored.parent() {
                    if parent != self.root {
                        let _ = fs::remove_dir(parent);
                    }
                }
            }
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

fn sanitize_path_component(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => ' ',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).trim();
    trimmed.chars().take(80).collect()
}

fn custom_folder_name(album: &str, artist: &str) -> String {
    let alb = sanitize_path_component(album);
    let art = sanitize_path_component(artist);
    match (alb.is_empty(), art.is_empty()) {
        (false, false) => format!("{alb} - {art}"),
        (true, false) => art,
        (false, true) => alb,
        (true, true) => "未分类".to_string(),
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
        assert!(!store.has_any());
        let first = store
            .import_file("track-a", "Song", "Artist", "My Album", Some(120_000.0), &local_lrc, 100)
            .expect("import lrc");
        assert!(store.has_any());
        assert_eq!(first.rel_path.as_deref(), Some("My Album - Artist/Song.lrc"));
        assert!(store.root_dir().join("My Album - Artist/Song.lrc").is_file());

        let local_yrc = base.join("song.yrc");
        fs::write(&local_yrc, "[1000,2000](1000,500,0)Hel(1500,500,0)lo").unwrap();
        let second = store
            .import_file("track-a", "Song", "Artist", "", Some(120_000.0), &local_yrc, 200)
            .expect("import yrc");
        assert_eq!(second.rel_path.as_deref(), Some("Artist/Song.yrc"));
        assert!(store.root_dir().join("Artist/Song.yrc").is_file());

        let list = store.entries_for("track-a");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second.id);
        assert!(list[0].word_timed);
        assert_eq!(list[1].id, first.id);

        assert!(store.remove(&second.id));
        assert_eq!(store.entries_for("track-a").len(), 1);
        assert!(!store.root_dir().join("Artist/Song.yrc").exists());
        assert!(local_yrc.exists(), "original local file must never be deleted");

        let _ = fs::remove_dir_all(&base);
    }
}
