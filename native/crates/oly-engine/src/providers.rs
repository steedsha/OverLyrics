//! The five lyric providers, ported from `src/engine/sources/index.ts`.
//!
//! Every adapter takes a [`Transport`] instead of owning a client: the live app
//! hands it the WinHTTP stack from `oly-platform-win`, and the parity suite hands
//! it the recorded byte streams from `native/tests/fixtures/http/recorded.json`.
//! Request shapes — URL, method, headers, body, timeout — are therefore pinned
//! by fixtures: a changed header or query fails a test.
//!
//! Provider quirks that are load-bearing and easy to "fix" wrongly:
//!
//! * Kugou search results carry `hash`/`duration` (seconds) and the lyric search
//!   has to echo both back or it returns nothing;
//! * QQ search is IP rate limited: `code 2001` means cool down and let the other
//!   sources answer, never retry in place;
//! * QQ's `<content>` is encrypted hex while `<contentts>` is plain LRC, and
//!   `<contentroma>` is encrypted hex that must be inflated before the timeline
//!   sniff;
//! * LRCLIB intermittently 503s and is retried exactly once after two seconds;
//! * AMLLDB ignores the artist in the query (its server matches a title
//!   substring) and has no duration at all.

use std::fmt;
use std::io::Read;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::decrypt::{decrypt_krc, krc_romanizations, krc_translations, triple_des_decrypt};
use crate::net::{self, HttpError, Transport};
use crate::sources::{LrclibEntry, amlldb_title_related, lrclib_map_entries};
use crate::types::{Candidate, Track};

/// What one provider can deliver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Supports {
    pub word: bool,
    pub trans: bool,
    pub roma: bool,
}

/// A provider failure. The search service isolates these per source: one broken
/// provider never blocks the others.
#[derive(Debug)]
pub enum ProviderError {
    Http(HttpError),
    /// The provider answered but has nothing usable ("未提供此曲目的歌词").
    Message(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(err) => write!(f, "{err}"),
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl From<HttpError> for ProviderError {
    fn from(err: HttpError) -> Self {
        Self::Http(err)
    }
}

/// What a provider returns when a candidate's lyrics are fetched.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderResult {
    pub raw: String,
    pub format: String,
    /// Per-line translation rows that belong to `raw` by line index (KRC).
    pub translation_list: Option<Vec<String>>,
    /// Per-line romanization rows that belong to `raw` by line index (KRC).
    pub romanization_list: Option<Vec<String>>,
    /// A second, independent LRC/QRC document carrying translations.
    pub translation_lrc: Option<String>,
    pub roma_lrc: Option<String>,
    pub reference: String,
}

/// Per-search knobs. `keep_plain` is LRCLIB's rescue mode: keep untimed rows so
/// a `中文 - English` title can be split, then filter them again before delivery.
#[derive(Clone, Copy, Debug, Default)]
pub struct SearchOptions {
    pub keep_plain: bool,
}

/// One lyric provider.
pub trait LyricsSource: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn supports(&self) -> Supports;
    /// Per-source search budget. The service uses the widest one; a provider
    /// that is slow only drags itself (the fan-out is concurrent).
    fn search_budget_ms(&self) -> u64 {
        2500
    }
    /// Per-source lyric probe budget. Slow community sources (e.g. AMLL) can
    /// override this without delaying faster concurrent sources.
    fn probe_budget_ms(&self) -> u64 {
        3500
    }
    fn search(
        &self,
        transport: &dyn Transport,
        track: &Track,
        opts: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError>;
    fn lyrics(
        &self,
        transport: &dyn Transport,
        candidate: &Candidate,
    ) -> Result<ProviderResult, ProviderError>;
}

// ---------------------------------------------------------------- helpers --

fn keyword(track: &Track) -> String {
    let artist = track.artist.clone().unwrap_or_default();
    [track.title.as_str(), artist.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn extra_text(candidate: &Candidate, key: &str) -> String {
    candidate
        .extra
        .as_ref()
        .and_then(|extra| extra.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn extra_number(candidate: &Candidate, key: &str) -> f64 {
    candidate
        .extra
        .as_ref()
        .and_then(|extra| extra.get(key))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

/// `String(number)` in JS: whole values print without a fractional part.
fn js_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 9e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn has_timing(text: &str) -> bool {
    let lrc = regex::Regex::new(r"\[\d{1,3}:\d{1,2}").expect("lrc timing regex");
    let qrc = regex::Regex::new(r"\[\d+,\d+\]").expect("qrc timing regex");
    lrc.is_match(text) || qrc.is_match(text) || text.contains("LyricContent=")
}

fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    if text.len() < 32 || text.len() % 2 != 0 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)? as u8;
        let low = (pair[1] as char).to_digit(16)? as u8;
        out.push(high << 4 | low);
    }
    Some(out)
}

/// QQ's ciphertext is zlib-wrapped deflate; a few payloads are raw deflate.
fn inflate_any(bytes: &[u8]) -> Option<String> {
    for zlib in [true, false] {
        let mut out = String::new();
        let ok = if zlib {
            flate2::read::ZlibDecoder::new(bytes)
                .read_to_string(&mut out)
                .is_ok()
        } else {
            flate2::read::DeflateDecoder::new(bytes)
                .read_to_string(&mut out)
                .is_ok()
        };
        if ok {
            return Some(out);
        }
    }
    None
}

fn split_artists(value: &str) -> Vec<String> {
    value
        .split(['\u{3001}', ','])
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

// ------------------------------------------------------------------ Kugou --

/// Kugou: no login, word-level KRC plus its own Chinese translation.
pub struct KugouSource;

impl KugouSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for KugouSource {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsSource for KugouSource {
    fn id(&self) -> &'static str {
        "kugou"
    }

    fn name(&self) -> &'static str {
        "酷狗音乐"
    }

    fn supports(&self) -> Supports {
        Supports {
            word: true,
            trans: true,
            roma: true,
        }
    }

    fn search(
        &self,
        transport: &dyn Transport,
        track: &Track,
        _opts: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError> {
        let url = format!(
            "http://mobilecdn.kugou.com/api/v3/search/song?format=json&keyword={}&page=1&pagesize=12&showtype=1",
            net::encode_component(&keyword(track))
        );
        let reply = net::get_json(transport, &url, &[], 10_000)?;
        let info = reply
            .pointer("/data/info")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(info
            .iter()
            .filter(|row| !text(row, "hash").is_empty())
            .map(|row| {
                let hash = text(row, "hash");
                let duration = number(row, "duration");
                Candidate {
                    source: "kugou".to_string(),
                    id: hash.clone(),
                    title: text(row, "songname"),
                    artists: Some(split_artists(&text(row, "singername"))),
                    album: Some(text(row, "album_name")),
                    duration_ms: Some(duration * 1000.0),
                    extra: Some(json!({
                        "hash": hash,
                        "duration": duration,
                        "albumId": text(row, "album_id"),
                    })),
                    ..Default::default()
                }
            })
            .collect())
    }

    fn lyrics(
        &self,
        transport: &dyn Transport,
        candidate: &Candidate,
    ) -> Result<ProviderResult, ProviderError> {
        let first_artist = candidate
            .artists
            .as_ref()
            .and_then(|list| list.first())
            .cloned()
            .unwrap_or_default();
        let query = [candidate.title.as_str(), first_artist.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let duration = extra_number(candidate, "duration");
        let hash = extra_text(candidate, "hash");
        let search_url = format!(
            "https://lyrics.kugou.com/search?ver=1&man=yes&client=pc&keyword={}&duration={}&hash={}",
            net::encode_component(&query),
            js_number(duration),
            net::encode_component(&hash)
        );
        let reply = net::get_json(transport, &search_url, &[], 12_000)?;
        let list = reply
            .get("candidates")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if list.is_empty() {
            return Err(ProviderError::Message(
                "酷狗音乐未提供此曲目的歌词".to_string(),
            ));
        }
        // Prefer the candidate whose song name matches; fall back to the first.
        let pick = list
            .iter()
            .find(|row| {
                let song = text(row, "song");
                !song.is_empty() && song.to_lowercase() == candidate.title.to_lowercase()
            })
            .unwrap_or(&list[0]);
        let download_url = format!(
            "https://lyrics.kugou.com/download?ver=1&client=pc&id={}&accesskey={}&fmt=krc&charset=utf8",
            text(pick, "id"),
            net::encode_component(&text(pick, "accesskey"))
        );
        let download = net::get_json(transport, &download_url, &[], 12_000)?;
        let content = text(&download, "content");
        if content.is_empty() {
            return Err(ProviderError::Message(
                "酷狗音乐歌词下载失败".to_string(),
            ));
        }
        let raw = decrypt_krc(&content)
            .ok_or_else(|| ProviderError::Message("酷狗音乐歌词解密失败".to_string()))?;
        let translation_list = krc_translations(&raw);
        let romanization_list = krc_romanizations(&raw);
        Ok(ProviderResult {
            format: "krc".to_string(),
            translation_list,
            romanization_list,
            reference: format!("https://www.kugou.com/song/#hash={hash}"),
            raw,
            translation_lrc: None,
            roma_lrc: None,
        })
    }
}

// ---------------------------------------------------------------- NetEase --

/// NetEase: line-level LRC plus an official `tlyric` translation.
pub struct NeteaseSource;

impl NeteaseSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NeteaseSource {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsSource for NeteaseSource {
    fn id(&self) -> &'static str {
        "netease"
    }

    fn name(&self) -> &'static str {
        "网易云音乐"
    }

    fn supports(&self) -> Supports {
        Supports {
            word: false,
            trans: true,
            roma: true,
        }
    }

    fn search(
        &self,
        transport: &dyn Transport,
        track: &Track,
        _opts: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError> {
        let url = format!(
            "http://music.163.com/api/search/get/web?csrf_token=&s={}&type=1&offset=0&total=true&limit=12",
            net::encode_component(&keyword(track))
        );
        let reply = net::get_json(
            transport,
            &url,
            &[("Referer", "https://music.163.com/")],
            10_000,
        )?;
        let songs = reply
            .pointer("/result/songs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(songs
            .iter()
            .filter(|row| row.get("id").is_some())
            .map(|row| Candidate {
                source: "netease".to_string(),
                id: row
                    .get("id")
                    .map(|id| id.to_string().trim_matches('"').to_string())
                    .unwrap_or_default(),
                title: text(row, "name"),
                artists: Some(
                    row.get("artists")
                        .and_then(Value::as_array)
                        .map(|artists| artists.iter().map(|a| text(a, "name")).collect())
                        .unwrap_or_default(),
                ),
                album: row
                    .get("album")
                    .map(|album| text(album, "name"))
                    .unwrap_or_default()
                    .into(),
                duration_ms: Some(number(row, "duration")),
                extra: Some(json!({})),
                ..Default::default()
            })
            .collect())
    }

    fn lyrics(
        &self,
        transport: &dyn Transport,
        candidate: &Candidate,
    ) -> Result<ProviderResult, ProviderError> {
        let url = format!(
            "https://music.163.com/api/song/lyric?id={}&lv=-1&kv=-1&tv=-1&rv=-1",
            candidate.id
        );
        let reply = net::get_json(
            transport,
            &url,
            &[("Referer", "https://music.163.com/")],
            12_000,
        )?;
        let lrc = reply
            .pointer("/lrc/lyric")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !regex::Regex::new(r"\d{1,3}:\d{1,2}")
            .expect("timing regex")
            .is_match(lrc)
        {
            return Err(ProviderError::Message(
                "网易云音乐未提供此曲目的歌词".to_string(),
            ));
        }
        let translation_lrc = reply
            .pointer("/tlyric/lyric")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        let roma_lrc = reply
            .pointer("/romalrc/lyric")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        Ok(ProviderResult {
            raw: lrc.to_string(),
            format: "lrc".to_string(),
            translation_list: None,
            romanization_list: None,
            translation_lrc,
            roma_lrc,
            reference: format!("https://music.163.com/song?id={}", candidate.id),
        })
    }
}

// --------------------------------------------------------------------- QQ --

const QQ_COOLDOWN_MIN_MS: i64 = 60_000;
const QQ_COOLDOWN_MAX_MS: i64 = 1_800_000;

struct QqState {
    limited_until_ms: i64,
    cooldown_ms: i64,
}

/// QQ Music: word-level QRC, plain-LRC official translation, QRC romanisation.
///
/// Search is IP rate limited. A `2001` answer cools the source down instead of
/// retrying: hammering it both prolongs the block and starves the other sources.
pub struct QqSource {
    state: Mutex<QqState>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl QqSource {
    pub fn new() -> Self {
        Self::with_clock(Box::new(system_now_ms))
    }

    /// Deterministic clock for tests.
    pub fn with_clock(clock: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            state: Mutex::new(QqState {
                limited_until_ms: 0,
                cooldown_ms: QQ_COOLDOWN_MIN_MS,
            }),
            clock,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QqState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl Default for QqSource {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsSource for QqSource {
    fn id(&self) -> &'static str {
        "qq"
    }

    fn name(&self) -> &'static str {
        "QQ 音乐"
    }

    fn supports(&self) -> Supports {
        Supports {
            word: true,
            trans: true,
            roma: true,
        }
    }

    fn search_budget_ms(&self) -> u64 {
        5000
    }

    fn search(
        &self,
        transport: &dyn Transport,
        track: &Track,
        _opts: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError> {
        let now = (self.clock)();
        if now < self.lock().limited_until_ms {
            return Ok(Vec::new());
        }
        let soso_url = format!(
            "https://c.y.qq.com/soso/fcgi-bin/client_search_cp?w={}&format=json&n=20&p=1",
            net::encode_component(&keyword(track))
        );
        let reply = match net::get_json(
            transport,
            &soso_url,
            &[("Referer", "https://y.qq.com/")],
            5_000,
        ) {
            Ok(reply) => reply,
            Err(_) => {
                let body = json!({
                    "req_1": {
                        "method": "DoSearchForQQMusicDesktop",
                        "module": "music.search.SearchCgiService",
                        "param": {
                            "num_per_page": 20,
                            "page_num": 1,
                            "query": keyword(track),
                            "search_type": 0,
                        },
                    }
                });
                net::post_json(
                    transport,
                    "https://u.y.qq.com/cgi-bin/musicu.fcg",
                    &body.to_string(),
                    &[
                        ("Content-Type", "application/json"),
                        ("Referer", "https://c.y.qq.com/"),
                    ],
                    10_000,
                )?
            }
        };
        let list = reply
            .pointer("/data/song/list")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| {
                reply
                    .pointer("/req_1/data/body/song/list")
                    .and_then(Value::as_array)
                    .cloned()
            })
            .unwrap_or_default();
        if list.is_empty() {
            let code = reply
                .pointer("/req_1/code")
                .or_else(|| reply.get("code"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let message = if code == 0 {
                // A clean empty answer: the server really has no match.
                self.lock().cooldown_ms = QQ_COOLDOWN_MIN_MS;
                None
            } else {
                let mut state = self.lock();
                state.cooldown_ms = (state.cooldown_ms * 2).min(QQ_COOLDOWN_MAX_MS);
                state.limited_until_ms = now + state.cooldown_ms;
                Some(state.cooldown_ms)
            };
            if let Some(cooldown) = message {
                log::warn!(
                    "QQ 搜索被限流（code {code}），冷却 {} 秒，期间由其他来源接管",
                    cooldown / 1000
                );
            }
            return Ok(Vec::new());
        }
        self.lock().cooldown_ms = QQ_COOLDOWN_MIN_MS;
        Ok(list
            .iter()
            .filter(|row| {
                !text(row, "mid").is_empty() || !text(row, "songmid").is_empty()
            })
            .map(|row| {
                let mid = {
                    let mid = text(row, "mid");
                    if mid.is_empty() {
                        text(row, "songmid")
                    } else {
                        mid
                    }
                };
                let sid = {
                    let id = row.get("id").map(value_id).unwrap_or_default();
                    if id.is_empty() {
                        row.get("songid").map(value_id).unwrap_or_default()
                    } else {
                        id
                    }
                };
                let artists: Vec<String> = row
                    .get("singer")
                    .and_then(Value::as_array)
                    .map(|singers| {
                        singers
                            .iter()
                            .map(|s| {
                                let name = text(s, "name");
                                if name.is_empty() {
                                    text(s, "title")
                                } else {
                                    name
                                }
                            })
                            .filter(|name| !name.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let album = row
                    .get("album")
                    .map(|album| text(album, "name"))
                    .unwrap_or_default();
                let album = if album.is_empty() {
                    text(row, "albumname")
                } else {
                    album
                };
                let id = if sid.is_empty() {
                    mid.clone()
                } else {
                    sid.clone()
                };
                Candidate {
                    source: "qq".to_string(),
                    id,
                    title: {
                        let name = text(row, "name");
                        if name.is_empty() {
                            let title = text(row, "title");
                            if title.is_empty() {
                                text(row, "songname")
                            } else {
                                title
                            }
                        } else {
                            name
                        }
                    },
                    artists: Some(artists),
                    album: Some(album),
                    duration_ms: Some(number(row, "interval") * 1000.0),
                    extra: Some(json!({ "mid": mid, "id": sid })),
                    ..Default::default()
                }
            })
            .collect())
    }

    fn lyrics(
        &self,
        transport: &dyn Transport,
        candidate: &Candidate,
    ) -> Result<ProviderResult, ProviderError> {
        let mid = extra_text(candidate, "mid");
        let music_id = {
            let id = extra_text(candidate, "id");
            if id.is_empty() {
                candidate.id.clone()
            } else {
                id
            }
        };
        let mut original = String::new();
        let mut translation = String::new();
        let mut roma = String::new();
        if !music_id.is_empty() && music_id.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(xml) = self.xml_lyric(transport, &music_id) {
                original = xml.0;
                translation = xml.1;
                roma = xml.2;
            }
        }
        if original.is_empty() && !mid.is_empty() {
            if let Ok(reply) = net::post_json(
                transport,
                "https://u.y.qq.com/cgi-bin/musicu.fcg?format=json",
                &json!({
                    "comm": { "ct": 19, "cv": 1873, "uin": "" },
                    "req": {
                        "method": "GetPlayLyricInfo",
                        "module": "music.musichallSong.PlayLyricInfo",
                        "param": {
                            "songMID": mid,
                            "songID": music_id.parse::<i64>().unwrap_or(0),
                            "qrc": 1,
                            "trans": 1,
                            "roma": 1,
                            "crypt": 1,
                            "ct": 19,
                            "cv": 1873,
                        },
                    },
                })
                .to_string(),
                &[
                    ("Content-Type", "application/json"),
                    ("Referer", "https://y.qq.com/"),
                ],
                12_000,
            ) {
                original = reply
                    .pointer("/req/data/lyric")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                translation = reply
                    .pointer("/req/data/trans")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                roma = reply
                    .pointer("/req/data/roma")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
            }
        }
        if original.is_empty() {
            return Err(ProviderError::Message(
                "QQ 音乐未提供此曲目的歌词".to_string(),
            ));
        }
        if !roma.is_empty() && !has_timing(&roma) {
            if let Some(bytes) = hex_bytes(&roma) {
                roma = inflate_any(&triple_des_decrypt(&bytes)).unwrap_or_default();
            }
        }
        let raw = if let Some(bytes) = hex_bytes(&original) {
            inflate_any(&triple_des_decrypt(&bytes))
        } else if regex::Regex::new(r"\[\d{1,3}:\d{1,2}")
            .expect("timing regex")
            .is_match(&original)
        {
            Some(original.clone())
        } else {
            None
        };
        let raw = match raw {
            Some(raw) => raw,
            None => {
                let hex = hex_bytes(&original).is_some();
                return Err(ProviderError::Message(if hex {
                    "QQ 音乐歌词解密失败".to_string()
                } else {
                    "QQ 音乐该版本歌词缺少时间轴（纯文本），无法同步".to_string()
                }));
            }
        };
        let format = if regex::Regex::new(r"LyricContent=|\[\d+,\d+\]")
            .expect("qrc regex")
            .is_match(&raw)
        {
            "qrc"
        } else {
            "lrc"
        };
        let has_roma = has_timing(&roma)
            || roma.contains("LyricContent=")
            || regex::Regex::new(r"\[\d+,\d+\]").expect("qrc roma regex").is_match(&roma);
        Ok(ProviderResult {
            format: format.to_string(),
            translation_lrc: has_timing(&translation).then_some(translation),
            roma_lrc: has_roma.then_some(roma),
            reference: if mid.is_empty() {
                String::new()
            } else {
                format!("https://y.qq.com/n/ryqq/songDetail/{mid}")
            },
            raw,
            translation_list: None,
            romanization_list: None,
        })
    }
}

impl QqSource {
    /// `lyric_download.fcg` hands over content, translation and romanisation in
    /// a single request — one round trip instead of three.
    fn xml_lyric(
        &self,
        transport: &dyn Transport,
        music_id: &str,
    ) -> Result<(String, String, String), ProviderError> {
        let body = format!(
            "version=15&miniversion=82&lrctype=4&musicid={}",
            net::encode_component(music_id)
        );
        let xml = net::post_form(
            transport,
            "https://c.y.qq.com/qqmusic/fcgi-bin/lyric_download.fcg",
            &body,
            &[
                ("Content-Type", "application/x-www-form-urlencoded"),
                ("Referer", "https://c.y.qq.com/"),
            ],
            12_000,
        )?;
        let cleaned = xml.replace("<!--", "").replace("-->", "");
        let cdata = |tag: &str| -> String {
            let pattern = format!(
                r"<{tag}\b[^>]*>\s*<!\[CDATA\[([\s\S]*?)\]\]>"
            );
            regex::Regex::new(&pattern)
                .ok()
                .and_then(|re| re.captures(&cleaned))
                .map(|caps| caps[1].trim().to_string())
                .unwrap_or_default()
        };
        Ok((cdata("content"), cdata("contentts"), cdata("contentroma")))
    }
}

fn value_id(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

// ----------------------------------------------------------------- LRCLIB --

/// LRCLIB: open, free, no key. Line-level only, so it sorts below every
/// word-level source; it exists to cover catalogues the Chinese services miss.
pub struct LrclibSource;

impl LrclibSource {
    pub fn new() -> Self {
        Self
    }

    fn search_once(
        &self,
        transport: &dyn Transport,
        track: &Track,
        opts: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError> {
        let url = format!(
            "https://lrclib.net/api/search?q={}",
            net::encode_component(&keyword(track))
        );
        let reply = net::get_json(transport, &url, &[], 14_000)?;
        let rows = reply.as_array().cloned().unwrap_or_default();
        let entries: Vec<LrclibEntry> = rows
            .iter()
            .map(|row| LrclibEntry {
                id: row.get("id").and_then(Value::as_i64).unwrap_or(0),
                track_name: text(row, "trackName"),
                artist_name: text(row, "artistName"),
                album_name: text(row, "albumName"),
                duration_sec: number(row, "duration"),
                synced_lyrics: row
                    .get("syncedLyrics")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                plain_lyrics: row
                    .get("plainLyrics")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect();
        Ok(lrclib_map_entries(&entries, opts.keep_plain))
    }
}

impl Default for LrclibSource {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsSource for LrclibSource {
    fn id(&self) -> &'static str {
        "lrclib"
    }

    fn name(&self) -> &'static str {
        "LRCLIB"
    }

    fn supports(&self) -> Supports {
        Supports {
            word: false,
            trans: false,
            roma: false,
        }
    }

    /// The host lives outside China; a cold TLS handshake is ~2s, so the default
    /// 2.5s budget cut it off and the source looked permanently empty.
    fn search_budget_ms(&self) -> u64 {
        15_000
    }

    fn search(
        &self,
        transport: &dyn Transport,
        track: &Track,
        opts: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError> {
        match self.search_once(transport, track, opts) {
            Err(ProviderError::Http(err)) if err.is_server_error() => {
                // Free public service, intermittently overloaded: one retry.
                std::thread::sleep(std::time::Duration::from_secs(2));
                self.search_once(transport, track, opts)
            }
            other => other,
        }
    }

    fn lyrics(
        &self,
        _transport: &dyn Transport,
        candidate: &Candidate,
    ) -> Result<ProviderResult, ProviderError> {
        let lrc = candidate
            .extra
            .as_ref()
            .and_then(|extra| extra.get("syncedLyrics"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                ProviderError::Message("LRCLIB 该候选未提供同步歌词".to_string())
            })?;
        Ok(ProviderResult {
            raw: lrc.to_string(),
            format: "lrc".to_string(),
            translation_list: None,
            romanization_list: None,
            translation_lrc: None,
            roma_lrc: None,
            reference: "https://lrclib.net".to_string(),
        })
    }
}

// ----------------------------------------------------------------- AMLLDB --

/// AMLL community library: hand-made word-level TTML, the only source with
/// duet agents and harmony blocks. Search matches a title substring, so the
/// query carries the title alone and a local word-level filter removes noise.
pub struct AmlldbSource;

impl AmlldbSource {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AmlldbSource {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsSource for AmlldbSource {
    fn id(&self) -> &'static str {
        "amlldb"
    }

    fn name(&self) -> &'static str {
        "AMLL 歌词库"
    }

    fn supports(&self) -> Supports {
        Supports {
            word: true,
            trans: true,
            roma: false,
        }
    }

    fn search_budget_ms(&self) -> u64 {
        9000
    }

    fn probe_budget_ms(&self) -> u64 {
        18_000
    }

    fn search(
        &self,
        transport: &dyn Transport,
        track: &Track,
        _opts: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError> {
        let query = track.title.trim().to_string();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let reply = net::post_json(
            transport,
            "https://amlldb.bikonoo.com/api/search-lyrics",
            &json!({ "query": query, "type": "all" }).to_string(),
            &[("Content-Type", "application/json")],
            8000,
        )?;
        let rows = reply.as_array().cloned().unwrap_or_default();
        Ok(rows
            .iter()
            .filter(|row| !text(row, "file").is_empty())
            .filter(|row| amlldb_title_related(&query, &text(row, "title")))
            .map(|row| {
                let artists = row
                    .get("artists")
                    .and_then(Value::as_array)
                    .map(|artists| {
                        artists
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .filter(|artists: &Vec<String>| !artists.is_empty())
                    .unwrap_or_else(|| {
                        text(row, "artist")
                            .split('/')
                            .map(|part| part.trim().to_string())
                            .filter(|part| !part.is_empty())
                            .collect()
                    });
                let album = row
                    .get("album")
                    .and_then(Value::as_array)
                    .and_then(|albums| albums.first())
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let file = text(row, "file");
                Candidate {
                    source: "amlldb".to_string(),
                    id: file.clone(),
                    title: text(row, "title"),
                    artists: Some(artists),
                    album: Some(album),
                    duration_ms: Some(0.0),
                    extra: Some(json!({
                        "file": file,
                        "amIds": row.get("amIds").cloned().unwrap_or(Value::Null),
                        "ncmIds": row.get("ncmIds").cloned().unwrap_or(Value::Null),
                        "qqIds": row.get("qqIds").cloned().unwrap_or(Value::Null),
                    })),
                    ..Default::default()
                }
            })
            .collect())
    }

    fn lyrics(
        &self,
        transport: &dyn Transport,
        candidate: &Candidate,
    ) -> Result<ProviderResult, ProviderError> {
        let file = {
            let file = extra_text(candidate, "file");
            if file.is_empty() {
                candidate.id.clone()
            } else {
                file
            }
        };
        if file.is_empty() {
            return Err(ProviderError::Message(
                "AMLL 歌词库候选缺少文件名".to_string(),
            ));
        }
        let encoded = net::encode_component(&file);
        let mirrors: [(String, u64); 3] = [
            (
                format!("https://raw.githubusercontent.com/amll-dev/amll-ttml-db/main/raw-lyrics/{encoded}"),
                5_000,
            ),
            (
                format!("https://testingcf.jsdelivr.net/gh/amll-dev/amll-ttml-db@main/raw-lyrics/{encoded}"),
                7_000,
            ),
            (
                format!("https://amlldb.bikonoo.com/raw-lyrics/{encoded}"),
                6_000,
            ),
        ];
        let mut raw = None;
        let mut last_err = None;
        for (url, timeout_ms) in &mirrors {
            match net::get_text(transport, url, &[], *timeout_ms) {
                Ok(text) => {
                    raw = Some(text);
                    break;
                }
                Err(err) => {
                    last_err = Some(ProviderError::from(err));
                }
            }
        }
        let raw = match raw {
            Some(text) => text,
            None => {
                return Err(last_err
                    .unwrap_or_else(|| ProviderError::Message("AMLL 歌词下载失败".to_string())))
            }
        };
        if !regex::Regex::new(r#"<p\b[^>]*\bbegin\s*=\s*""#)
            .expect("ttml gate")
            .is_match(&raw)
        {
            // The probe classifies "格式错误" as definitively unusable; wording
            // matters, so keep it.
            return Err(ProviderError::Message(
                "AMLL 歌词库返回的内容格式错误（不是 TTML 歌词）".to_string(),
            ));
        }
        Ok(ProviderResult {
            raw,
            format: "ttml".to_string(),
            translation_list: None,
            romanization_list: None,
            translation_lrc: None,
            roma_lrc: None,
            reference: "https://amlldb.bikonoo.com/".to_string(),
        })
    }
}

/// The registry, in the reference's order (Kugou and NetEase first, QQ third,
/// the two overseas catalogues last).
pub fn create_sources() -> Vec<Box<dyn LyricsSource>> {
    vec![
        Box::new(KugouSource::new()),
        Box::new(NeteaseSource::new()),
        Box::new(QqSource::new()),
        Box::new(LrclibSource::new()),
        Box::new(AmlldbSource::new()),
    ]
}
