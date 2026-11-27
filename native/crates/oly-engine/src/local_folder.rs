//! Background read-only scanner and matcher for a user-selected local lyrics folder.
//!
//! Strictly read-only: never creates, modifies, moves, or deletes any file in the
//! user's chosen directory. Builds a lightweight in-memory index on a background
//! thread and reuses `oly_engine::matching` + `oly_engine::identity` to match
//! playing tracks against `.lrc` / `.ttml` / `.qrc` / `.yrc` / `.txt` files.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use oly_contracts::LyricDoc;

use crate::custom_lyrics::is_supported_lyric_file;
use crate::doc::{RawLyrics, ScoredCandidate, build_doc};
use crate::identity::identity_gate;
use crate::matching::compare_track;
use crate::parsers::parse_imported_lyrics;
use crate::types::{Candidate, Track};

const MAX_SCAN_DEPTH: usize = 8;
const MAX_INDEXED_FILES: usize = 20_000;
const MAX_PARSED_CACHE: usize = 64;

#[derive(Clone, Debug)]
struct IndexedFile {
    id: String,
    path: PathBuf,
    file_name: String,
    ext: String,
    stem: String,
    parent_name: String,
}

#[derive(Clone, Debug)]
pub struct LocalFolderMatch {
    pub id: String,
    pub path: PathBuf,
    pub file_name: String,
    pub ext: String,
    pub percent: f64,
    pub word_timed: bool,
    pub has_translation: bool,
    pub line_count: usize,
    pub doc: LyricDoc,
}

#[derive(Default)]
struct ScannerState {
    dir: Option<PathBuf>,
    files: Vec<IndexedFile>,
    by_id: HashMap<String, usize>,
    parsed_cache: HashMap<String, LyricDoc>,
}

#[derive(Clone)]
pub struct LocalFolderScanner {
    state: Arc<Mutex<ScannerState>>,
    generation: Arc<AtomicU64>,
    scan_version: Arc<AtomicU64>,
}

impl Default for LocalFolderScanner {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LocalFolderScanner {
    pub fn new(dir: Option<PathBuf>) -> Self {
        let scanner = Self {
            state: Arc::new(Mutex::new(ScannerState::default())),
            generation: Arc::new(AtomicU64::new(0)),
            scan_version: Arc::new(AtomicU64::new(0)),
        };
        if dir.is_some() {
            scanner.set_dir(dir);
        }
        scanner
    }

    pub fn scan_version(&self) -> u64 {
        self.scan_version.load(Ordering::SeqCst)
    }

    pub fn set_dir(&self, dir: Option<PathBuf>) {
        let clean = dir.and_then(|p| {
            let s = p.to_string_lossy();
            (!s.trim().is_empty()).then_some(p)
        });
        let scan_gen = self.generation.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.dir = clean.clone();
            st.files.clear();
            st.by_id.clear();
            st.parsed_cache.clear();
        }
        let Some(root) = clean else {
            return;
        };
        let state = self.state.clone();
        let generation = self.generation.clone();
        let scan_version = self.scan_version.clone();
        let _ = std::thread::Builder::new()
            .name("local-lyrics-scan".into())
            .spawn(move || {
                let files = scan_dir_readonly(&root);
                if generation.load(Ordering::SeqCst) != scan_gen {
                    return;
                }
                let mut by_id = HashMap::with_capacity(files.len());
                for (i, f) in files.iter().enumerate() {
                    by_id.insert(f.id.clone(), i);
                }
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                if generation.load(Ordering::SeqCst) != scan_gen {
                    return;
                }
                st.files = files;
                st.by_id = by_id;
                scan_version.fetch_add(1, Ordering::SeqCst);
            });
    }

    /// Synchronous scan helper primarily for deterministic unit tests.
    pub fn set_dir_sync(&self, dir: Option<PathBuf>) {
        let clean = dir.and_then(|p| {
            let s = p.to_string_lossy();
            (!s.trim().is_empty()).then_some(p)
        });
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.dir = clean.clone();
        st.files.clear();
        st.by_id.clear();
        st.parsed_cache.clear();
        if let Some(root) = clean {
            let files = scan_dir_readonly(&root);
            for (i, f) in files.iter().enumerate() {
                st.by_id.insert(f.id.clone(), i);
            }
            st.files = files;
        }
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).dir.clone()
    }

    pub fn has_any(&self) -> bool {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        !st.files.is_empty()
    }

    pub fn file_count(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).files.len()
    }

    pub fn matches_for(&self, track: &Track, threshold: f64) -> Vec<LocalFolderMatch> {
        if track.title.trim().is_empty() {
            return Vec::new();
        }
        let ranked: Vec<(f64, IndexedFile)> = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.files.is_empty() {
                return Vec::new();
            }
            let mut hits = Vec::new();
            for file in &st.files {
                if let Some(percent) = score_indexed_file(track, file) {
                    if percent >= threshold {
                        hits.push((percent, file.clone()));
                    }
                }
            }
            hits.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| ext_priority(&b.1.ext).cmp(&ext_priority(&a.1.ext)))
            });
            hits.truncate(3);
            hits
        };

        let mut out = Vec::with_capacity(ranked.len());
        for (percent, file) in ranked {
            if let Some(doc) = self.load_or_parse(&file, track) {
                let word_timed = doc.lines.iter().any(|l| !l.words.is_empty());
                let has_translation = doc.lines.iter().any(|l| !l.trans.trim().is_empty());
                let line_count = doc.lines.len();
                out.push(LocalFolderMatch {
                    id: file.id,
                    path: file.path,
                    file_name: file.file_name,
                    ext: file.ext,
                    percent,
                    word_timed,
                    has_translation,
                    line_count,
                    doc,
                });
            }
        }
        out
    }

    pub fn get_doc(&self, id: &str) -> Option<LyricDoc> {
        let (cached, file) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(doc) = st.parsed_cache.get(id) {
                return Some(doc.clone());
            }
            let idx = *st.by_id.get(id)?;
            (None::<LyricDoc>, st.files.get(idx)?.clone())
        };
        let _ = cached;
        self.load_or_parse(&file, &Track::default())
    }

    fn load_or_parse(&self, file: &IndexedFile, track: &Track) -> Option<LyricDoc> {
        {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(doc) = st.parsed_cache.get(&file.id) {
                return Some(doc.clone());
            }
        }
        let raw = fs::read_to_string(&file.path).ok()?;
        let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(&raw);
        let parsed = parse_imported_lyrics(raw, &file.ext)?;
        if parsed.lines.is_empty() {
            return None;
        }
        let scored = ScoredCandidate {
            id: file.id.clone(),
            best: Candidate {
                source: "custom".into(),
                id: file.id.clone(),
                title: if track.title.trim().is_empty() {
                    file.stem.clone()
                } else {
                    track.title.clone()
                },
                artist: track
                    .artist
                    .as_ref()
                    .filter(|a| !a.trim().is_empty())
                    .cloned(),
                duration_sec: track.duration_ms.map(|d| d / 1000.0),
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
        )?;
        let doc = built.to_lyric_doc();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.parsed_cache.len() >= MAX_PARSED_CACHE {
            st.parsed_cache.clear();
        }
        st.parsed_cache.insert(file.id.clone(), doc.clone());
        Some(doc)
    }
}

fn ext_priority(ext: &str) -> u8 {
    match ext {
        "ttml" => 5,
        "qrc" | "yrc" => 4,
        "lrc" => 3,
        "txt" => 1,
        _ => 0,
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn scan_dir_readonly(root: &Path) -> Vec<IndexedFile> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_SCAN_DEPTH || out.len() >= MAX_INDEXED_FILES {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_INDEXED_FILES {
                break;
            }
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.') || name_str.eq_ignore_ascii_case("node_modules") {
                    continue;
                }
                if depth < MAX_SCAN_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if ft.is_file() && is_supported_lyric_file(&path) {
                let file_name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                let ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if stem.is_empty() {
                    continue;
                }
                let parent_name = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let id = format!(
                    "localdir-{:016x}",
                    fnv1a64(path.to_string_lossy().as_bytes())
                );
                out.push(IndexedFile {
                    id,
                    path,
                    file_name,
                    ext,
                    stem,
                    parent_name,
                });
            }
        }
    }
    out
}

fn strip_leading_track_num(stem: &str) -> &str {
    let bytes = stem.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 3 {
        return stem;
    }
    let rest = &stem[i..];
    if let Some(after) = rest.strip_prefix(". ") {
        return after.trim();
    }
    if let Some(after) = rest.strip_prefix(" - ") {
        return after.trim();
    }
    if let Some(after) = rest.strip_prefix('.') {
        return after.trim();
    }
    stem
}

fn parent_artist_hint(parent: &str) -> Option<String> {
    let p = parent.trim();
    if p.is_empty() {
        return None;
    }
    if let Some((_, right)) = p.rsplit_once(" - ") {
        let r = right.trim();
        if !r.is_empty() {
            return Some(r.to_string());
        }
    }
    Some(p.to_string())
}

fn score_indexed_file(track: &Track, file: &IndexedFile) -> Option<f64> {
    let clean_stem = strip_leading_track_num(&file.stem);
    let mut variants: Vec<(String, Option<String>)> = Vec::with_capacity(3);
    if let Some((left, right)) = clean_stem.split_once(" - ") {
        let l = left.trim();
        let r = right.trim();
        if !l.is_empty() && !r.is_empty() {
            // Try both "Artist - Title" and "Title - Artist"
            variants.push((r.to_string(), Some(l.to_string())));
            variants.push((l.to_string(), Some(r.to_string())));
        }
    } else {
        let hint = parent_artist_hint(&file.parent_name);
        if hint.is_some() {
            variants.push((clean_stem.to_string(), hint));
        }
        variants.push((clean_stem.to_string(), None));
    }

    let mut best: Option<f64> = None;
    for (cand_title, cand_artist) in variants {
        let cand = Candidate {
            source: "custom".into(),
            id: file.id.clone(),
            title: cand_title,
            artist: cand_artist,
            ..Default::default()
        };
        if !identity_gate(&cand, track) {
            continue;
        }
        let cmp = compare_track(track, &cand);
        let pct = cmp.percent;
        if best.is_none_or(|b| pct > b) {
            best = Some(pct);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_matches_artist_title_and_nested_folder_readonly() {
        let root = std::env::temp_dir().join(format!(
            "oly-local-folder-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let sub = root.join("范特西 - 周杰伦");
        fs::create_dir_all(&sub).unwrap();
        let f1 = root.join("周杰伦 - 七里香.lrc");
        let f2 = sub.join("01. 简单爱.lrc");
        fs::write(&f1, "[00:01.00]窗外的麻雀\n[00:05.00]在电线杆上多嘴\n").unwrap();
        fs::write(&f2, "[00:02.00]说不上为什么\n[00:06.00]我变得很主动\n").unwrap();

        let scanner = LocalFolderScanner::new(None);
        scanner.set_dir_sync(Some(root.clone()));
        assert_eq!(scanner.file_count(), 2);

        let t1 = Track {
            title: "七里香".into(),
            artist: Some("周杰伦".into()),
            ..Default::default()
        };
        let m1 = scanner.matches_for(&t1, 70.0);
        assert_eq!(m1.len(), 1);
        assert_eq!(m1[0].file_name, "周杰伦 - 七里香.lrc");
        assert!(m1[0].line_count >= 2);

        let t2 = Track {
            title: "简单爱".into(),
            artist: Some("周杰伦".into()),
            ..Default::default()
        };
        let m2 = scanner.matches_for(&t2, 70.0);
        assert_eq!(m2.len(), 1);
        assert_eq!(m2[0].file_name, "01. 简单爱.lrc");

        // Original files remain untouched
        assert!(f1.exists());
        assert!(f2.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
