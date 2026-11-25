//! High-resolution static cover fallback resolver.
//!
//! When SMTC provides no thumbnail or only a low-resolution thumbnail, this
//! resolver fetches an 800x800+ cover from QQ Music, NetEase Cloud Music, or
//! iTunes Search, with strict title/artist validation and an LRU disk cache.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::crosslang::norm_txt;
use crate::identity::{artist_ok, title_ok};
use crate::motion_cover::split_smtc_artist_album;
use crate::net::{HttpRequest, Transport};

const REQUEST_TIMEOUT_MS: u64 = 6_000;
const IMAGE_TIMEOUT_MS: u64 = 8_000;
const HIT_TTL_MS: u64 = 30 * 60 * 1000;
const MISS_TTL_MS: u64 = 5 * 60 * 1000;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_CACHE_MB: u64 = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverTrackInput {
    pub title: String,
    pub artist: String,
    pub album: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HdCoverResult {
    pub ok: bool,
    pub bytes: Option<Vec<u8>>,
    pub source: Option<&'static str>,
    pub cached: bool,
    pub reason: Option<&'static str>,
}

impl HdCoverResult {
    fn fail(reason: &'static str) -> Self {
        Self {
            ok: false,
            bytes: None,
            source: None,
            cached: false,
            reason: Some(reason),
        }
    }
}

#[derive(Clone)]
struct MemoEntry<T> {
    value: T,
    expires_at: u64,
}

pub struct HdCoverResolver {
    cache_dir: PathBuf,
    hits: HashMap<String, MemoEntry<(PathBuf, &'static str)>>,
    misses: HashMap<String, MemoEntry<()>>,
}

impl HdCoverResolver {
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            hits: HashMap::new(),
            misses: HashMap::new(),
        }
    }

    pub fn resolve(
        &mut self,
        input: &CoverTrackInput,
        now_ms: u64,
        transport: &dyn Transport,
    ) -> HdCoverResult {
        let title = input.title.trim();
        let (artist, album) = split_smtc_artist_album(&input.artist, &input.album);
        if title.is_empty() || artist.is_empty() {
            return HdCoverResult::fail("missing_metadata");
        }

        let key = cache_key(title, &artist, &album);
        if let Some(entry) = self.hits.get(&key) {
            if entry.expires_at > now_ms {
                if let Ok(bytes) = fs::read(&entry.value.0) {
                    if is_valid_image(&bytes) {
                        return HdCoverResult {
                            ok: true,
                            bytes: Some(bytes),
                            source: Some(entry.value.1),
                            cached: true,
                            reason: None,
                        };
                    }
                }
            }
        }
        if let Some(entry) = self.misses.get(&key) {
            if entry.expires_at > now_ms {
                return HdCoverResult::fail("cached_miss");
            }
        }

        let disk_path = self.cache_dir.join(format!("{key}.img"));
        if let Ok(bytes) = fs::read(&disk_path) {
            if is_valid_image(&bytes) {
                self.hits.insert(
                    key,
                    MemoEntry {
                        value: (disk_path, "cache"),
                        expires_at: now_ms + HIT_TTL_MS,
                    },
                );
                return HdCoverResult {
                    ok: true,
                    bytes: Some(bytes),
                    source: Some("cache"),
                    cached: true,
                    reason: None,
                };
            }
        }

        let resolved = fetch_qq(title, &artist, &album, transport)
            .or_else(|| fetch_netease(title, &artist, &album, transport))
            .or_else(|| fetch_itunes(title, &artist, &album, "us", transport))
            .or_else(|| fetch_itunes(title, &artist, &album, "cn", transport));

        match resolved {
            Some((source, bytes)) => {
                self.write_cache(&disk_path, &bytes);
                self.hits.insert(
                    key,
                    MemoEntry {
                        value: (disk_path, source),
                        expires_at: now_ms + HIT_TTL_MS,
                    },
                );
                HdCoverResult {
                    ok: true,
                    bytes: Some(bytes),
                    source: Some(source),
                    cached: false,
                    reason: None,
                }
            }
            None => {
                self.misses.insert(
                    key,
                    MemoEntry {
                        value: (),
                        expires_at: now_ms + MISS_TTL_MS,
                    },
                );
                HdCoverResult::fail("not_found")
            }
        }
    }

    fn write_cache(&self, path: &Path, bytes: &[u8]) {
        if fs::create_dir_all(&self.cache_dir).is_err() {
            return;
        }
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, bytes).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
        prune_cache(&self.cache_dir, DEFAULT_MAX_CACHE_MB * 1024 * 1024);
    }
}

fn cache_key(title: &str, artist: &str, album: &str) -> String {
    let raw = if album.is_empty() {
        format!("{}|{}", norm_txt(title), norm_txt(artist))
    } else {
        format!("{}|{}", norm_txt(album), norm_txt(artist))
    };
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in raw.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn prune_cache(dir: &Path, max_bytes: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext == Some("tmp") {
            let _ = fs::remove_file(&path);
            continue;
        }
        if ext != Some("img") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let size = meta.len();
            let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            total = total.saturating_add(size);
            files.push((path, size, modified));
        }
    }
    if total <= max_bytes {
        return;
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, size, _) in files {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn is_valid_image(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return false;
    }
    bytes.starts_with(&[0x89, b'P', b'N', b'G'])
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || (bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

fn score_candidate(
    cand_title: &str,
    cand_artist: &str,
    cand_album: &str,
    title: &str,
    artist: &str,
    album: &str,
) -> Option<i32> {
    if !title_ok(cand_title, title, Some(artist))
        || !artist_ok(&[cand_artist.to_string()], artist)
    {
        return None;
    }
    let mut score = 100;
    let nt = norm_txt(title);
    let nct = norm_txt(cand_title);
    if nct == nt {
        score += 40;
    }
    if !album.is_empty() && !cand_album.is_empty() {
        let na = norm_txt(album);
        let nca = norm_txt(cand_album);
        if nca == na {
            score += 35;
        } else if nca.contains(&na) || na.contains(&nca) {
            score += 15;
        }
    }
    let low_query = title.to_lowercase();
    let low_cand = cand_title.to_lowercase();
    for tag in ["翻唱", "cover", "live", "伴奏", "instrumental", "dj", "remix"] {
        if low_cand.contains(tag) && !low_query.contains(tag) {
            score -= 45;
        }
    }
    Some(score)
}

fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push_str("%20"),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

fn fetch_image(url: &str, referer: Option<&str>, transport: &dyn Transport) -> Option<Vec<u8>> {
    let mut req = HttpRequest::get(url, IMAGE_TIMEOUT_MS);
    if let Some(r) = referer {
        req = req.header("Referer", r);
    }
    let resp = transport.send(&req).ok()?;
    if resp.status < 200 || resp.status >= 300 || !is_valid_image(&resp.body) {
        return None;
    }
    Some(resp.body)
}

#[derive(Deserialize)]
struct QqSearchResp {
    data: Option<QqData>,
}
#[derive(Deserialize)]
struct QqData {
    song: Option<QqSongList>,
}
#[derive(Deserialize)]
struct QqSongList {
    #[serde(default)]
    list: Vec<QqSong>,
}
#[derive(Deserialize)]
struct QqSong {
    #[serde(default)]
    songname: String,
    #[serde(default)]
    albumname: String,
    #[serde(default)]
    albummid: String,
    #[serde(default)]
    singer: Vec<QqSinger>,
}
#[derive(Deserialize)]
struct QqSinger {
    #[serde(default)]
    name: String,
}

fn fetch_qq(
    title: &str,
    artist: &str,
    album: &str,
    transport: &dyn Transport,
) -> Option<(&'static str, Vec<u8>)> {
    let term = url_encode(&format!("{artist} {title}"));
    let url = format!(
        "https://c.y.qq.com/soso/fcgi-bin/client_search_cp?p=1&n=5&w={term}&format=json"
    );
    let req = HttpRequest::get(url, REQUEST_TIMEOUT_MS).header("Referer", "https://y.qq.com/");
    let resp = transport.send(&req).ok()?;
    if resp.status < 200 || resp.status >= 300 {
        return None;
    }
    let parsed: QqSearchResp = serde_json::from_slice(&resp.body).ok()?;
    let list = parsed.data?.song?.list;
    let mut best: Option<(i32, String)> = None;
    for song in list {
        if song.albummid.trim().is_empty() {
            continue;
        }
        let singers = song
            .singer
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        if let Some(score) =
            score_candidate(&song.songname, &singers, &song.albumname, title, artist, album)
        {
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, song.albummid));
            }
        }
    }
    let (_, albummid) = best?;
    let pic_url = format!("https://y.gtimg.cn/music/photo_new/T002R800x800M000{albummid}.jpg");
    let bytes = fetch_image(&pic_url, Some("https://y.qq.com/"), transport)?;
    Some(("qq", bytes))
}

#[derive(Deserialize)]
struct NetEaseSearchResp {
    result: Option<NetEaseSearchResult>,
}
#[derive(Deserialize)]
struct NetEaseSearchResult {
    #[serde(default)]
    songs: Vec<NetEaseSearchSong>,
}
#[derive(Deserialize)]
struct NetEaseSearchSong {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<NetEaseArtist>,
    album: Option<NetEaseAlbum>,
}
#[derive(Deserialize)]
struct NetEaseArtist {
    #[serde(default)]
    name: String,
}
#[derive(Deserialize)]
struct NetEaseAlbum {
    #[serde(default)]
    name: String,
    #[serde(rename = "picUrl", default)]
    pic_url: String,
}
#[derive(Deserialize)]
struct NetEaseDetailResp {
    #[serde(default)]
    songs: Vec<NetEaseSearchSong>,
}

fn fetch_netease(
    title: &str,
    artist: &str,
    album: &str,
    transport: &dyn Transport,
) -> Option<(&'static str, Vec<u8>)> {
    let term = url_encode(&format!("{artist} {title}"));
    let url = format!("https://music.163.com/api/search/get/web?s={term}&type=1&limit=5");
    let req = HttpRequest::get(url, REQUEST_TIMEOUT_MS).header("Referer", "https://music.163.com/");
    let resp = transport.send(&req).ok()?;
    if resp.status < 200 || resp.status >= 300 {
        return None;
    }
    let parsed: NetEaseSearchResp = serde_json::from_slice(&resp.body).ok()?;
    let songs = parsed.result?.songs;
    let mut best: Option<(i32, u64)> = None;
    for song in songs {
        let singers = song
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let cand_album = song.album.as_ref().map_or("", |a| a.name.as_str());
        if let Some(score) =
            score_candidate(&song.name, &singers, cand_album, title, artist, album)
        {
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, song.id));
            }
        }
    }
    let (_, song_id) = best?;
    let detail_url = format!("https://music.163.com/api/song/detail/?ids=[{song_id}]");
    let detail_req =
        HttpRequest::get(detail_url, REQUEST_TIMEOUT_MS).header("Referer", "https://music.163.com/");
    let detail_resp = transport.send(&detail_req).ok()?;
    if detail_resp.status < 200 || detail_resp.status >= 300 {
        return None;
    }
    let detail: NetEaseDetailResp = serde_json::from_slice(&detail_resp.body).ok()?;
    let pic_url = detail.songs.first()?.album.as_ref()?.pic_url.trim();
    if pic_url.is_empty() {
        return None;
    }
    let hd_url = if pic_url.contains('?') {
        pic_url.to_string()
    } else {
        format!("{pic_url}?param=1000y1000")
    };
    let bytes = fetch_image(&hd_url, Some("https://music.163.com/"), transport)?;
    Some(("netease", bytes))
}

#[derive(Deserialize)]
struct ItunesResp {
    #[serde(default)]
    results: Vec<ItunesHit>,
}
#[derive(Deserialize)]
struct ItunesHit {
    #[serde(rename = "trackName", default)]
    track_name: String,
    #[serde(rename = "artistName", default)]
    artist_name: String,
    #[serde(rename = "collectionName", default)]
    collection_name: String,
    #[serde(rename = "artworkUrl100", default)]
    artwork_url_100: String,
}

fn fetch_itunes(
    title: &str,
    artist: &str,
    album: &str,
    country: &str,
    transport: &dyn Transport,
) -> Option<(&'static str, Vec<u8>)> {
    let term = url_encode(&format!("{artist} {title}"));
    let url = format!(
        "https://itunes.apple.com/search?term={term}&entity=song&limit=5&country={country}"
    );
    let req = HttpRequest::get(url, REQUEST_TIMEOUT_MS);
    let resp = transport.send(&req).ok()?;
    if resp.status < 200 || resp.status >= 300 {
        return None;
    }
    let parsed: ItunesResp = serde_json::from_slice(&resp.body).ok()?;
    let mut best: Option<(i32, String)> = None;
    for hit in parsed.results {
        if hit.artwork_url_100.trim().is_empty() {
            continue;
        }
        if let Some(score) = score_candidate(
            &hit.track_name,
            &hit.artist_name,
            &hit.collection_name,
            title,
            artist,
            album,
        ) {
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, hit.artwork_url_100));
            }
        }
    }
    let (_, art100) = best?;
    let hd_url = art100.replace("100x100bb", "1000x1000bb");
    let bytes = fetch_image(&hd_url, None, transport)?;
    Some(("itunes", bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::HttpResponse;
    use std::sync::Mutex;

    struct StubTransport {
        responses: Mutex<Vec<(String, HttpResponse)>>,
    }

    impl Transport for StubTransport {
        fn send(&self, req: &HttpRequest) -> Result<HttpResponse, crate::net::HttpError> {
            let mut list = self.responses.lock().unwrap();
            if let Some(pos) = list.iter().position(|(pat, _)| req.url.contains(pat)) {
                Ok(list.remove(pos).1)
            } else {
                Ok(HttpResponse {
                    status: 404,
                    content_type: String::new(),
                    body: Vec::new(),
                })
            }
        }
    }

    fn fake_jpeg() -> Vec<u8> {
        vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]
    }

    #[test]
    fn resolves_qq_cover_and_uses_disk_cache_on_repeat() {
        let dir = std::env::temp_dir().join(format!("oly-hd-cover-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let transport = StubTransport {
            responses: Mutex::new(vec![
                (
                    "client_search_cp".into(),
                    HttpResponse {
                        status: 200,
                        content_type: "application/json".into(),
                        body: r#"{"data":{"song":{"list":[{"songname":"晴天","albumname":"叶惠美","albummid":"000MkMni19ClKG","singer":[{"name":"周杰伦"}]}]}}}"#.as_bytes().to_vec(),
                    },
                ),
                (
                    "T002R800x800M000000MkMni19ClKG.jpg".into(),
                    HttpResponse {
                        status: 200,
                        content_type: "image/jpeg".into(),
                        body: fake_jpeg(),
                    },
                ),
            ]),
        };

        let input = CoverTrackInput {
            title: "晴天".into(),
            artist: "周杰伦".into(),
            album: "叶惠美".into(),
        };

        let mut resolver = HdCoverResolver::new(&dir);
        let first = resolver.resolve(&input, 1_000, &transport);
        assert!(first.ok);
        assert_eq!(first.source, Some("qq"));
        assert!(!first.cached);
        assert_eq!(first.bytes.as_deref(), Some(fake_jpeg().as_slice()));

        // Fresh resolver with empty transport hits disk cache.
        let empty_transport = StubTransport {
            responses: Mutex::new(Vec::new()),
        };
        let mut second_resolver = HdCoverResolver::new(&dir);
        let second = second_resolver.resolve(&input, 2_000, &empty_transport);
        assert!(second.ok);
        assert!(second.cached);
        assert_eq!(second.source, Some("cache"));
        assert_eq!(second.bytes.as_deref(), Some(fake_jpeg().as_slice()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_mismatched_cover_artist() {
        assert!(score_candidate(
            "晴天 (原唱 周杰伦)",
            "某翻唱歌手",
            "翻唱合集",
            "晴天",
            "周杰伦",
            "叶惠美"
        )
        .is_none());
    }
}
