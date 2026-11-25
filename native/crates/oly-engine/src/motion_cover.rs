//! Apple Music motion cover lookup, m3u8 variant selection, and LRU disk cache.
//!
//! Ported 1:1 from `src/engine/motion-cover.ts`:
//! iTunes Search -> collectionId -> SSR album page -> `motionDetailSquare.video`
//! (.m3u8) -> H.264 <= 1080p variant -> `#EXT-X-MAP` .mp4 -> atomic disk cache.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::Deserialize;

use crate::net::{self, Transport};

pub const SEARCH_LIMIT: usize = 10;
pub const TRY_CANDIDATES: usize = 3;
pub const MIN_SCORE: i32 = 5;

pub const T_SEARCH_MS: u64 = 8_000;
pub const T_PAGE_MS: u64 = 12_000;
pub const T_M3U8_MS: u64 = 8_000;
pub const T_DL_MS: u64 = 90_000;

pub const MAX_HEIGHT: u32 = 1080;
pub const MEMO_LIMIT: usize = 256;
pub const MISSING_TTL_MS: f64 = 30.0 * 60.0 * 1000.0;
pub const URL_TTL_MS: f64 = 6.0 * 60.0 * 60.0 * 1000.0;
pub const MIN_ASSET_BYTES: u64 = 50_000;
// Match the browser UA Apple Music SSR expects before embedding `motionDetailSquare`.
const MOTION_UA: (&str, &str) = (
    "User-Agent",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
);

static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\u3000]+").expect("ws regex"));
static BRACKETS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[「」【】\[\]（）()]").expect("bracket regex"));
static SINGLE_EP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s*[-–—]\s*(single|ep)\s*$").expect("single ep regex"));
static EDITION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*[（(]\s*(deluxe|explicit|clean)[^)）]*[)）]\s*$").expect("edition regex")
});
static FEAT_TAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(?:feat\.?|ft\.?|featuring)\s+.+$").expect("feat regex"));
static ARTIST_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s+(?:feat\.?|ft\.?|featuring)\s+|\s*[,&/、，]\s*").expect("split regex")
});
static SMTC_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?)\s+[—–]\s+(.+)$").expect("smtc split regex"));
static LATIN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z]").expect("latin regex"));
static CJK_HANGUL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\u3040-\u30ff\u3400-\u9fff\uac00-\ud7af]").expect("cjk regex"));
static MOTION_M3U8_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)"motionDetailSquare".{0,1200}?"video":"([^"]+\.m3u8)""#).expect("motion regex")
});
static CODECS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"CODECS="([^"]+)""#).expect("codecs regex"));
static RES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"RESOLUTION=(\d+)x(\d+)").expect("res regex"));
static BW_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"BANDWIDTH=(\d+)").expect("bw regex"));
static MAP_URI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"#EXT-X-MAP:URI="([^"]+)""#).expect("map uri regex"));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionTrackInput {
    pub title: String,
    pub artist: String,
    pub album: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionMatch {
    pub collection_id: String,
    pub score: i32,
    pub track_name: String,
    pub artist_name: String,
    pub collection_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionResult {
    pub ok: bool,
    pub file: Option<PathBuf>,
    pub collection_id: Option<String>,
    pub cached: bool,
    pub album_hit: bool,
    pub match_info: Option<MotionMatch>,
    pub reason: Option<&'static str>,
}

impl MotionResult {
    fn fail(reason: &'static str) -> Self {
        Self {
            ok: false,
            file: None,
            collection_id: None,
            cached: false,
            album_hit: false,
            match_info: None,
            reason: Some(reason),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MotionCacheFile {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified_ms: u128,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub count: usize,
    pub bytes: u64,
}

pub trait MotionStore {
    fn ensure_dir(&self, dir: &Path) -> Result<(), String>;
    fn size(&self, file: &Path) -> Option<u64>;
    fn touch(&self, file: &Path);
    fn list_mp4(&self, dir: &Path) -> Vec<MotionCacheFile>;
    fn remove(&self, file: &Path) -> Result<(), String>;
    fn clear_dir(&self, dir: &Path) -> CacheStats;
    fn download(&self, url: &str, dest: &Path, timeout_ms: u64) -> Result<u64, String>;
}

/// Real filesystem + caller-injected downloader store.
pub struct DiskMotionStore<D> {
    downloader: D,
}

impl<D> DiskMotionStore<D> {
    pub fn new(downloader: D) -> Self {
        Self { downloader }
    }
}

impl<D> MotionStore for DiskMotionStore<D>
where
    D: Fn(&str, &Path, u64) -> Result<u64, String>,
{
    fn ensure_dir(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())
    }

    fn size(&self, file: &Path) -> Option<u64> {
        std::fs::metadata(file).ok().map(|m| m.len())
    }

    fn touch(&self, file: &Path) {
        if let Ok(meta) = std::fs::metadata(file) {
            let needs_touch = meta
                .modified()
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_none_or(|age| age.as_secs() >= 600);
            if !needs_touch {
                return;
            }
        }
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(file) {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    fn list_mp4(&self, dir: &Path) -> Vec<MotionCacheFile> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_string_lossy().into_owned();
                if !name.ends_with(".mp4") {
                    return None;
                }
                let meta = entry.metadata().ok()?;
                let modified_ms = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                Some(MotionCacheFile {
                    name,
                    path,
                    size: meta.len(),
                    modified_ms,
                })
            })
            .collect()
    }

    fn remove(&self, file: &Path) -> Result<(), String> {
        std::fs::remove_file(file).map_err(|e| e.to_string())
    }

    fn clear_dir(&self, dir: &Path) -> CacheStats {
        let mut removed = 0usize;
        let mut bytes = 0u64;
        for file in self.list_mp4(dir) {
            if std::fs::remove_file(&file.path).is_ok() {
                removed += 1;
                bytes += file.size;
            }
        }
        CacheStats { count: removed, bytes }
    }

    fn download(&self, url: &str, dest: &Path, timeout_ms: u64) -> Result<u64, String> {
        (self.downloader)(url, dest, timeout_ms)
    }
}

pub fn norm(value: &str) -> String {
    let lower = value.to_lowercase();
    let collapsed = WS_RE.replace_all(&lower, " ");
    BRACKETS_RE.replace_all(&collapsed, "").trim().to_string()
}

pub fn strip_suffix(value: &str) -> String {
    let lower = value.to_lowercase();
    let raw = WS_RE.replace_all(&lower, " ");
    let raw = raw.trim();
    let without_single = SINGLE_EP_RE.replace(raw, "");
    let without_edition = EDITION_RE.replace(&without_single, "");
    norm(&without_edition)
}

pub fn title_key(value: &str) -> String {
    let stripped = strip_suffix(value);
    FEAT_TAIL_RE.replace(&stripped, "").trim().to_string()
}

fn fold_latin_accents(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => 'a',
            'é' | 'è' | 'ê' | 'ë' | 'ē' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' => 'o',
            'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            'ý' | 'ÿ' => 'y',
            other => other,
        })
        .collect()
}

pub fn artist_key(value: &str) -> String {
    let normalized = fold_latin_accents(&norm(value));
    ARTIST_SPLIT_RE
        .split(&normalized)
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn split_smtc_artist_album(artist: &str, album: &str) -> (String, String) {
    let raw_artist = artist.trim();
    if let Some(caps) = SMTC_SPLIT_RE.captures(raw_artist) {
        let parsed_artist = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        let parsed_album = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("");
        let final_album = if album.trim().is_empty() {
            parsed_album
        } else {
            album.trim()
        };
        (parsed_artist.to_string(), final_album.to_string())
    } else {
        (raw_artist.to_string(), album.trim().to_string())
    }
}

pub fn same_artist_album(
    cand_artist: &str,
    cand_album: &str,
    artist: &str,
    album: &str,
) -> bool {
    if !artist.is_empty() {
        let ca = artist_key(cand_artist);
        let wa = artist_key(artist);
        if ca.is_empty() || ca != wa {
            return false;
        }
    }
    if !album.is_empty() {
        let actual = strip_suffix(cand_album);
        let wanted = strip_suffix(album);
        if actual.is_empty() || wanted.is_empty() {
            return false;
        }
        let close = actual == wanted
            || (actual.chars().count() >= 6
                && wanted.chars().count() >= 6
                && (actual.contains(&wanted) || wanted.contains(&actual)));
        if !close {
            return false;
        }
    }
    true
}

pub fn score_of(
    cand_title: &str,
    cand_artist: &str,
    cand_album: &str,
    title: &str,
    artist: &str,
    album: &str,
) -> i32 {
    let t = title_key(cand_title);
    let wanted_t = title_key(title);
    let a = norm(cand_artist);
    let wanted_a = norm(artist);
    let al = norm(cand_album);
    let wanted_al = strip_suffix(album);

    let mut score = 0;
    if !t.is_empty() && t == wanted_t {
        score += 5;
    } else if !t.is_empty() && !wanted_t.is_empty() && (t.contains(&wanted_t) || wanted_t.contains(&t)) {
        score += 2;
    }

    if !a.is_empty() && a == wanted_a {
        score += 3;
    } else if !a.is_empty() && !wanted_a.is_empty() && (a.contains(&wanted_a) || wanted_a.contains(&a)) {
        score += 1;
    }

    if !wanted_al.is_empty() && al == wanted_al {
        score += 4;
    } else if !wanted_al.is_empty() && !al.is_empty() && (al.contains(&wanted_al) || wanted_al.contains(&al)) {
        score += 1;
    }
    score
}

pub fn resolve_url(base: &str, rel: &str) -> Option<String> {
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return Some(rel.to_string());
    }
    let scheme_end = base.find("://")?;
    let scheme = &base[..scheme_end];
    if rel.starts_with("//") {
        return Some(format!("{scheme}:{rel}"));
    }
    let after_scheme = &base[scheme_end + 3..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .map(|i| scheme_end + 3 + i)
        .unwrap_or(base.len());
    let origin = &base[..authority_end];
    if rel.starts_with('/') {
        return Some(format!("{origin}{rel}"));
    }
    let path_only = base[authority_end..]
        .split(['?', '#'])
        .next()
        .unwrap_or("/");
    let mut segments: Vec<&str> = match path_only.rfind('/') {
        Some(idx) => path_only[..=idx]
            .split('/')
            .filter(|s| !s.is_empty())
            .collect(),
        None => Vec::new(),
    };
    let (rel_path, rel_tail) = match rel.find(['?', '#']) {
        Some(idx) => (&rel[..idx], &rel[idx..]),
        None => (rel, ""),
    };
    for seg in rel_path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    let joined = segments.join("/");
    Some(format!("{origin}/{joined}{rel_tail}"))
}

pub fn pick_variant_from_playlist(text: &str, master_url: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut best: Option<(u64, String)> = None;
    for (i, line) in lines.iter().enumerate() {
        if !line.starts_with("#EXT-X-STREAM-INF") {
            continue;
        }
        let codecs = CODECS_RE
            .captures(line)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .unwrap_or("");
        let lower_codecs = codecs.to_lowercase();
        if lower_codecs.contains("hvc1") || lower_codecs.contains("hev1") {
            continue;
        }
        let Some(res_caps) = RES_RE.captures(line) else {
            continue;
        };
        let height: u32 = res_caps.get(2)?.as_str().parse().ok()?;
        if height == 0 || height > MAX_HEIGHT {
            continue;
        }
        let bandwidth: u64 = BW_RE
            .captures(line)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);

        let mut uri = "";
        for cand in &lines[i + 1..] {
            let trimmed = cand.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('#') {
                break;
            }
            uri = trimmed;
            break;
        }
        if uri.is_empty() {
            continue;
        }
        let replace = match &best {
            None => true,
            Some((best_bw, _)) => bandwidth > *best_bw,
        };
        if replace {
            best = Some((bandwidth, uri.to_string()));
        }
    }
    let (_, uri) = best?;
    resolve_url(master_url, &uri)
}

pub fn variant_mp4_from_playlist(text: &str, variant_url: &str) -> Option<String> {
    let uri = if let Some(caps) = MAP_URI_RE.captures(text) {
        caps.get(1)?.as_str().to_string()
    } else {
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))?
            .to_string()
    };
    resolve_url(variant_url, &uri)
}

pub fn extract_master_m3u8(html: &str) -> Option<String> {
    let caps = MOTION_M3U8_RE.captures(html)?;
    let raw = caps.get(1)?.as_str();
    Some(
        raw.replace("\\u002F", "/")
            .replace("\\u002f", "/")
            .replace("\\/", "/"),
    )
}

struct MemoryMemo<T> {
    order: VecDeque<String>,
    entries: HashMap<String, (T, f64)>,
}

impl<T: Clone> MemoryMemo<T> {
    fn new() -> Self {
        Self {
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    fn get(&mut self, key: &str, now_ms: f64) -> Option<T> {
        let expires = self.entries.get(key).map(|(_, exp)| *exp)?;
        if now_ms >= expires {
            self.delete(key);
            return None;
        }
        self.touch_order(key);
        self.entries.get(key).map(|(v, _)| v.clone())
    }

    fn set(&mut self, key: String, value: T, expires_at_ms: f64) {
        self.delete(&key);
        self.order.push_back(key.clone());
        self.entries.insert(key, (value, expires_at_ms));
        while self.entries.len() > MEMO_LIMIT {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn delete(&mut self, key: &str) {
        self.entries.remove(key);
        self.order.retain(|k| k != key);
    }

    fn clear(&mut self) {
        self.order.clear();
        self.entries.clear();
    }

    fn touch_order(&mut self, key: &str) {
        self.order.retain(|k| k != key);
        self.order.push_back(key.to_string());
    }
}

#[derive(Clone, Debug)]
struct AlbumMemo {
    collection_id: String,
    file: PathBuf,
    match_info: MotionMatch,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchHit>,
}

#[derive(Clone, Deserialize)]
struct SearchHit {
    #[serde(rename = "collectionId")]
    collection_id: Option<serde_json::Value>,
    #[serde(rename = "trackName")]
    track_name: Option<String>,
    #[serde(rename = "artistName")]
    artist_name: Option<String>,
    #[serde(rename = "collectionName")]
    collection_name: Option<String>,
}

fn collection_id_string(val: Option<&serde_json::Value>) -> Option<String> {
    match val? {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

fn now_unix_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

pub struct MotionCoverResolver<S> {
    dir: PathBuf,
    store: S,
    cache_epoch: u64,
    url_memo: MemoryMemo<Option<String>>,
    album_hit: MemoryMemo<AlbumMemo>,
    album_miss: MemoryMemo<bool>,
}

impl<S: MotionStore> MotionCoverResolver<S> {
    pub fn new(dir: PathBuf, store: S) -> Self {
        if let Err(error) = store.ensure_dir(&dir) {
            log::warn!("motion: 建缓存目录失败 {error}");
        }
        Self {
            dir,
            store,
            cache_epoch: 0,
            url_memo: MemoryMemo::new(),
            album_hit: MemoryMemo::new(),
            album_miss: MemoryMemo::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn cache_epoch(&self) -> u64 {
        self.cache_epoch
    }

    pub fn cache_file(&self, collection_id: &str) -> PathBuf {
        self.dir.join(format!("{collection_id}.mp4"))
    }

    pub fn stats(&self) -> CacheStats {
        let files = self.store.list_mp4(&self.dir);
        CacheStats {
            count: files.len(),
            bytes: files.iter().map(|f| f.size).sum(),
        }
    }

    pub fn clear_cache(&mut self) -> CacheStats {
        self.cache_epoch = self.cache_epoch.saturating_add(1);
        self.url_memo.clear();
        self.album_hit.clear();
        self.album_miss.clear();
        self.store.clear_dir(&self.dir)
    }

    pub fn prune(&self, max_cache_mb: u64) {
        let cap = max_cache_mb.max(100) * 1024 * 1024;
        let mut files = self.store.list_mp4(&self.dir);
        let mut total: u64 = files.iter().map(|f| f.size).sum();
        if total <= cap {
            return;
        }
        files.sort_by_key(|f| f.modified_ms);
        let mut removed = 0usize;
        for file in files {
            if total <= cap {
                break;
            }
            if self.store.remove(&file.path).is_ok() {
                total = total.saturating_sub(file.size);
                removed += 1;
            }
        }
        if removed > 0 {
            log::info!(
                "motion: LRU 清理 {removed} 个（现 {:.0}MB）",
                total as f64 / 1_048_576.0
            );
        }
    }

    pub fn resolve(
        &mut self,
        input: &MotionTrackInput,
        enabled: bool,
        max_cache_mb: u64,
        transport: &dyn Transport,
    ) -> MotionResult {
        if !enabled {
            return MotionResult::fail("disabled");
        }
        if input.title.trim().is_empty() {
            return MotionResult::fail("no-title");
        }
        let (artist, album) = split_smtc_artist_album(&input.artist, &input.album);
        let title = input.title.as_str();
        let now = now_unix_ms();
        let epoch = self.cache_epoch;

        let ak = if album.is_empty() {
            String::new()
        } else {
            format!("{}|{}", norm(&album), norm(&artist))
        };
        if !ak.is_empty() {
            if let Some(memo) = self.album_hit.get(&ak, now) {
                if !same_artist_album(
                    &memo.match_info.artist_name,
                    &memo.match_info.collection_name,
                    &artist,
                    &album,
                ) {
                    self.album_hit.delete(&ak);
                } else if self
                    .store
                    .size(&memo.file)
                    .is_some_and(|len| len > MIN_ASSET_BYTES)
                {
                    self.store.touch(&memo.file);
                    return MotionResult {
                        ok: true,
                        file: Some(memo.file),
                        collection_id: Some(memo.collection_id),
                        cached: true,
                        album_hit: true,
                        match_info: Some(memo.match_info),
                        reason: None,
                    };
                } else {
                    self.album_hit.delete(&ak);
                }
            }
            if self.album_miss.get(&ak, now).is_some() {
                return MotionResult::fail("no-motion");
            }
        }

        let result = self.resolve_inner(title, &artist, &album, max_cache_mb, now, transport);
        if epoch != self.cache_epoch {
            return MotionResult::fail("cancelled");
        }
        if !ak.is_empty() {
            if result.ok
                && let (Some(cid), Some(file), Some(match_info)) = (
                    result.collection_id.clone(),
                    result.file.clone(),
                    result.match_info.clone(),
                )
            {
                self.album_hit.set(
                    ak,
                    AlbumMemo {
                        collection_id: cid,
                        file,
                        match_info,
                    },
                    f64::INFINITY,
                );
            } else if result.reason == Some("no-motion") {
                self.album_miss.set(ak, true, now + MISSING_TTL_MS);
            }
        }
        result
    }

    fn resolve_inner(
        &mut self,
        title: &str,
        artist: &str,
        album: &str,
        max_cache_mb: u64,
        now: f64,
        transport: &dyn Transport,
    ) -> MotionResult {
        let term = if artist.is_empty() {
            title.to_string()
        } else {
            format!("{title} {artist}")
        };
        let latin_artist = LATIN_RE.is_match(artist) && !CJK_HANGUL_RE.is_match(artist);
        let countries = if latin_artist {
            ["us", "jp"]
        } else {
            ["jp", "us"]
        };

        let mut all_hits: Vec<(&'static str, SearchHit)> = Vec::new();
        let mut any_search_failed = false;
        let mut all_search_failed = true;
        for country in countries {
            let url = format!(
                "https://itunes.apple.com/search?term={}&entity=song&limit={SEARCH_LIMIT}&country={country}",
                net::encode_component(&term)
            );
            match net::get_json(transport, &url, &[MOTION_UA], T_SEARCH_MS)
                .and_then(|val| serde_json::from_value::<SearchResponse>(val).map_err(|e| crate::net::HttpError::Json(e.to_string())))
            {
                Ok(parsed) => {
                    all_search_failed = false;
                    all_hits.extend(parsed.results.into_iter().map(|hit| (country, hit)));
                }
                Err(_) => {
                    any_search_failed = true;
                }
            }
        }
        if all_search_failed {
            return MotionResult::fail("search-error");
        }
        if all_hits.is_empty() {
            return MotionResult::fail("no-match");
        }

        let mut scored: Vec<(i32, &'static str, SearchHit, String)> = all_hits
            .into_iter()
            .filter_map(|(country, hit)| {
                let cid = collection_id_string(hit.collection_id.as_ref())?;
                let cand_title = hit.track_name.as_deref().unwrap_or("");
                let cand_artist = hit.artist_name.as_deref().unwrap_or("");
                let cand_album = hit.collection_name.as_deref().unwrap_or("");
                let s = score_of(cand_title, cand_artist, cand_album, title, artist, album);
                if s >= MIN_SCORE
                    && title_key(cand_title) == title_key(title)
                    && same_artist_album(cand_artist, cand_album, artist, album)
                {
                    Some((s, country, hit, cid))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));

        let mut seen = HashSet::new();
        let mut picks = Vec::new();
        for (score, country, hit, cid) in scored {
            if seen.insert(cid.clone()) {
                picks.push((score, country, hit, cid));
                if picks.len() >= TRY_CANDIDATES {
                    break;
                }
            }
        }
        if picks.is_empty() {
            return MotionResult::fail("no-match");
        }

        let mut uncertain = any_search_failed;
        for (score, country, hit, cid) in picks {
            match self.asset_for(&cid, country, max_cache_mb, now, transport) {
                Ok(Some((file, cached))) => {
                    if cached {
                        self.store.touch(&file);
                    }
                    return MotionResult {
                        ok: true,
                        file: Some(file),
                        collection_id: Some(cid),
                        cached,
                        album_hit: false,
                        match_info: Some(MotionMatch {
                            collection_id: cid_clone(&hit),
                            score,
                            track_name: hit.track_name.unwrap_or_default(),
                            artist_name: hit.artist_name.unwrap_or_default(),
                            collection_name: hit.collection_name.unwrap_or_default(),
                        }),
                        reason: None,
                    };
                }
                Ok(None) => {}
                Err(_) => {
                    uncertain = true;
                }
            }
        }

        if uncertain {
            MotionResult::fail("error")
        } else {
            MotionResult::fail("no-motion")
        }
    }

    fn asset_for(
        &mut self,
        collection_id: &str,
        country: &str,
        max_cache_mb: u64,
        now: f64,
        transport: &dyn Transport,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        let file = self.cache_file(collection_id);
        if self.store.size(&file).is_some_and(|len| len > MIN_ASSET_BYTES) {
            return Ok(Some((file, true)));
        }
        let Some(mp4) = self.motion_mp4(collection_id, country, now, transport)? else {
            return Ok(None);
        };
        if let Err(err) = self.store.download(&mp4, &file, T_DL_MS) {
            self.url_memo.delete(collection_id);
            return Err(err);
        }
        self.prune(max_cache_mb);
        Ok(Some((file, false)))
    }

    fn motion_mp4(
        &mut self,
        collection_id: &str,
        country: &str,
        now: f64,
        transport: &dyn Transport,
    ) -> Result<Option<String>, String> {
        if let Some(memoized) = self.url_memo.get(collection_id, now) {
            return Ok(memoized);
        }
        let mut storefronts = Vec::with_capacity(4);
        for candidate in ["cn", country, "us", "jp"] {
            if !storefronts.contains(&candidate) {
                storefronts.push(candidate);
            }
        }
        let mut master = None;
        let mut any_page_ok = false;
        let mut last_err = None;
        for storefront in storefronts {
            let page_url = format!("https://music.apple.com/{storefront}/album/x/{collection_id}");
            match net::get_text(transport, &page_url, &[MOTION_UA], T_PAGE_MS) {
                Ok(html) => {
                    any_page_ok = true;
                    if let Some(found) = extract_master_m3u8(&html) {
                        master = Some(found);
                        break;
                    }
                }
                Err(err) => {
                    last_err = Some(err.to_string());
                }
            }
        }
        if !any_page_ok {
            return Err(last_err.unwrap_or_else(|| "apple music page fetch failed".into()));
        }
        let Some(master) = master else {
            self.url_memo
                .set(collection_id.to_string(), None, now + MISSING_TTL_MS);
            return Ok(None);
        };
        let master_text =
            net::get_text(transport, &master, &[MOTION_UA], T_M3U8_MS).map_err(|e| e.to_string())?;
        let Some(variant) = pick_variant_from_playlist(&master_text, &master) else {
            self.url_memo
                .set(collection_id.to_string(), None, now + MISSING_TTL_MS);
            return Ok(None);
        };
        let variant_text =
            net::get_text(transport, &variant, &[MOTION_UA], T_M3U8_MS).map_err(|e| e.to_string())?;
        let mp4 = variant_mp4_from_playlist(&variant_text, &variant);
        let ttl = if mp4.is_some() { URL_TTL_MS } else { MISSING_TTL_MS };
        self.url_memo
            .set(collection_id.to_string(), mp4.clone(), now + ttl);
        Ok(mp4)
    }
}

fn cid_clone(hit: &SearchHit) -> String {
    collection_id_string(hit.collection_id.as_ref()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{HttpError, HttpRequest, HttpResponse};
    use std::cell::RefCell;

    struct FakeTransport {
        responses: HashMap<String, String>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl Transport for FakeTransport {
        fn send(&self, req: &HttpRequest) -> Result<HttpResponse, HttpError> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(req.url.clone());
            match self.responses.get(&req.url) {
                Some(body) => Ok(HttpResponse {
                    status: 200,
                    content_type: "text/plain".into(),
                    body: body.as_bytes().to_vec(),
                }),
                None => Err(HttpError::Status(404)),
            }
        }
    }

    struct MemStore {
        files: RefCell<HashMap<PathBuf, (Vec<u8>, u128)>>,
        next_tick: RefCell<u128>,
    }

    impl MemStore {
        fn new() -> Self {
            Self {
                files: RefCell::new(HashMap::new()),
                next_tick: RefCell::new(100),
            }
        }
    }

    impl MotionStore for MemStore {
        fn ensure_dir(&self, _dir: &Path) -> Result<(), String> {
            Ok(())
        }
        fn size(&self, file: &Path) -> Option<u64> {
            self.files.borrow().get(file).map(|(b, _)| b.len() as u64)
        }
        fn touch(&self, file: &Path) {
            let mut t = self.next_tick.borrow_mut();
            *t += 1;
            if let Some(entry) = self.files.borrow_mut().get_mut(file) {
                entry.1 = *t;
            }
        }
        fn list_mp4(&self, dir: &Path) -> Vec<MotionCacheFile> {
            self.files
                .borrow()
                .iter()
                .filter(|(p, _)| p.parent() == Some(dir))
                .map(|(p, (b, t))| MotionCacheFile {
                    name: p.file_name().unwrap().to_string_lossy().into_owned(),
                    path: p.clone(),
                    size: b.len() as u64,
                    modified_ms: *t,
                })
                .collect()
        }
        fn remove(&self, file: &Path) -> Result<(), String> {
            self.files.borrow_mut().remove(file);
            Ok(())
        }
        fn clear_dir(&self, dir: &Path) -> CacheStats {
            let list = self.list_mp4(dir);
            let bytes: u64 = list.iter().map(|f| f.size).sum();
            for item in &list {
                self.files.borrow_mut().remove(&item.path);
            }
            CacheStats {
                count: list.len(),
                bytes,
            }
        }
        fn download(&self, _url: &str, dest: &Path, _timeout_ms: u64) -> Result<u64, String> {
            let len = (MIN_ASSET_BYTES + 4096) as usize;
            let mut t = self.next_tick.borrow_mut();
            *t += 1;
            self.files
                .borrow_mut()
                .insert(dest.to_path_buf(), (vec![b'x'; len], *t));
            Ok(len as u64)
        }
    }

    #[test]
    fn picks_highest_h264_variant_up_to_1080p_and_extracts_mp4() {
        let master = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=9000000,RESOLUTION=2160x2160,CODECS=\"hvc1.1.6.L150\"
2160.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1200000,RESOLUTION=540x540,CODECS=\"avc1.64001f\"
540.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=4500000,RESOLUTION=1080x1080,CODECS=\"avc1.640028\"
1080.m3u8
";
        let picked = pick_variant_from_playlist(
            master,
            "https://mvod.itunes.apple.com/assets/master.m3u8",
        )
        .unwrap();
        assert_eq!(picked, "https://mvod.itunes.apple.com/assets/1080.m3u8");

        let variant = "#EXTM3U\n#EXT-X-MAP:URI=\"video_1080.mp4\"\n#EXTINF:5.0,\n";
        let mp4 = variant_mp4_from_playlist(variant, &picked).unwrap();
        assert_eq!(
            mp4,
            "https://mvod.itunes.apple.com/assets/video_1080.mp4"
        );
    }

    #[test]
    fn resolves_downloads_short_circuits_same_album_and_invalidates_on_clear() {
        let mut responses = HashMap::new();
        let search_json = serde_json::json!({
            "results": [{
                "collectionId": 1440857781,
                "trackName": "Style",
                "artistName": "Taylor Swift",
                "collectionName": "1989 (Deluxe)"
            }]
        })
        .to_string();
        responses.insert(
            format!(
                "https://itunes.apple.com/search?term={}&entity=song&limit=10&country=us",
                net::encode_component("Style Taylor Swift")
            ),
            search_json,
        );
        responses.insert(
            "https://itunes.apple.com/search?term=Style%20Taylor%20Swift&entity=song&limit=10&country=jp"
                .into(),
            r#"{"results":[]}"#.into(),
        );
        responses.insert(
            "https://music.apple.com/cn/album/x/1440857781".into(),
            r#"{"motionDetailSquare":{"previewFrame":{},"video":"https:\/\/example.com\/assets\/master.m3u8"}}"#.into(),
        );
        responses.insert(
            "https://example.com/assets/master.m3u8".into(),
            "#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1080x1080,CODECS=\"avc1.640028\"\n1080.m3u8\n"
                .into(),
        );
        responses.insert(
            "https://example.com/assets/1080.m3u8".into(),
            "#EXT-X-MAP:URI=\"loop.mp4\"\n".into(),
        );

        let transport = FakeTransport {
            responses,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let mut resolver = MotionCoverResolver::new(PathBuf::from("/cache"), MemStore::new());
        let first = resolver.resolve(
            &MotionTrackInput {
                title: "Style".into(),
                artist: "Taylor Swift — 1989 (Deluxe)".into(),
                album: String::new(),
            },
            true,
            500,
            &transport,
        );
        assert!(first.ok);
        assert_eq!(first.collection_id.as_deref(), Some("1440857781"));
        assert!(!first.cached);

        let calls_after_first = transport
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        let second = resolver.resolve(
            &MotionTrackInput {
                title: "Blank Space".into(),
                artist: "Taylor Swift".into(),
                album: "1989 (Deluxe)".into(),
            },
            true,
            500,
            &transport,
        );
        assert!(second.ok);
        assert!(second.album_hit);
        assert_eq!(
            transport
                .calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            calls_after_first
        );

        let cleared = resolver.clear_cache();
        assert_eq!(cleared.count, 1);
        assert_eq!(resolver.stats().count, 0);
    }
}
