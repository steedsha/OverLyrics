//! The lyrics service: fan-out search, probing, source memory, lyric cache.
//!
//! Ported from `LyricsService` in `src/engine/index.ts`. The reference is async
//! because it runs on Node; this port is synchronous and takes a [`Transport`],
//! so a test drives it with recorded or synthetic bytes and every ordering is
//! explicit instead of being left to a scheduler.
//!
//! Three invariants are load-bearing and each has a test:
//!
//! * **A generation number fences every writer.** A search that started before
//!   the cache was invalidated must never publish into the new generation —
//!   otherwise a slow answer for the previous song lands in the panel of the
//!   current one.
//! * **Probing happens before the list is delivered.** Handing out unverified
//!   candidates and retracting them a moment later is what the reference calls
//!   "先放出来，闪一下才说没有源"; users see the list jump.
//! * **A soft probe failure never removes a row.** Only a provider saying
//!   "no lyrics / decrypt failed / not found" is definitive. A timeout or a
//!   reset connection leaves the row in place, unverified but selectable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::doc::{build_doc, compose, RawLyrics, ScoredCandidate};
use crate::identity::identity_gate;
use crate::matching::score_candidate;
use crate::net::{Transport, HttpRequest, HttpResponse, HttpError};
use crate::providers::{
    create_sources, LyricsSource, ProviderError, ProviderResult, SearchOptions as SourceSearchOptions,
};
use crate::strings::{primary_artist, title_variants, track_key};
use crate::translate::has_foreign_lyrics;
use crate::types::{Candidate, MatchInfo, Track, Verified};

/// Per-source search budget. The fan-out is concurrent in the reference; a slow
/// source must not hold the panel, which is what this ceiling encodes.
pub const SEARCH_BUDGET_MS: u64 = 2500;
/// One lyric fetch during a probe. Kugou's probe is search + download + decrypt
/// (≈2 s measured), so 1.2 s would misjudge a healthy source.
pub const PROBE_BUDGET_MS: u64 = 3500;
/// Whole-round probe ceiling. Past this the list is delivered with the probe
/// still running and `probed: false`.
pub const PROBE_TOTAL_BUDGET_MS: u64 = 9000;
pub const SEARCH_CACHE_MAX: usize = 240;
pub const SEARCH_CACHE_TTL_MS: i64 = 10 * 60 * 1000;
/// An empty result is only believed for 30 s: a cold song searched during a
/// network wobble would otherwise poison the panel for ten minutes.
pub const SEARCH_CACHE_EMPTY_TTL_MS: i64 = 30 * 1000;
pub const VARIANT_CANDIDATES_PER_SOURCE: usize = 6;
pub const MAX_CANDIDATES: usize = 60;
pub const LYRIC_RESPONSE_TTL_MS: i64 = 120_000;
pub const LYRIC_RESPONSE_MAX: usize = 96;
pub const LYRIC_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const LYRIC_RESPONSE_ITEM_BYTES: usize = 1024 * 1024;
pub const SOURCE_MEMORY_MAX: usize = 400;

/// Which probe failures are the provider *definitively* reporting that this
/// candidate cannot be used. Everything else is "we do not know yet".
pub const DEFINITIVE_PROBE_FAIL: &str = concat!(
    r"没有[^，。！？]{0,10}歌词|无歌词|沒有歌詞|未找到|找不到|没有找到|不存在|",
    r"解密失败|解析失败|无法解析|格式错误|no\s+lyric|not\s+found|",
    r"invalid\s+(response|data|format)|decrypt"
);

/// `withTimeout(promise, ms, '搜索超时')` / `'探测超时'`.
pub const SEARCH_TIMEOUT_MESSAGE: &str = "搜索超时";
pub const PROBE_TIMEOUT_MESSAGE: &str = "探测超时";

struct BudgetTransport<'a> {
    inner:&'a dyn Transport,
    deadline:std::time::Instant,
    epoch:u64,
    current:&'a AtomicU64,
}
impl<'a> BudgetTransport<'a> {
    fn new(inner:&'a dyn Transport,ms:u64,epoch:u64,current:&'a AtomicU64)->Self {
        Self{inner,deadline:std::time::Instant::now()+std::time::Duration::from_millis(ms),epoch,current}
    }
    fn expired(&self)->bool {self.current.load(Ordering::SeqCst)!=self.epoch || std::time::Instant::now()>=self.deadline}
}
impl Transport for BudgetTransport<'_> {
    fn send(&self,request:&HttpRequest)->Result<HttpResponse,HttpError> {
        if self.expired(){return Err(HttpError::Timeout);}
        let mut request=request.clone();
        let remaining=self.deadline.saturating_duration_since(std::time::Instant::now()).as_millis().max(1) as u64;
        request.timeout_ms=if request.timeout_ms==0{remaining}else{request.timeout_ms.min(remaining)};
        self.inner.send(&request)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Filters one search call. `force` ignores the cache; `restart` also drops a
/// cached *empty* answer, which is what the panel's refresh button asks for.
#[derive(Clone, Copy, Debug, Default)]
pub struct SearchFlags {
    pub force: bool,
    pub restart: bool,
    /// LRCLIB's rescue mode: keep untimed rows so a bilingual title can be split.
    pub keep_plain: bool,
}

/// One published snapshot of the search panel.
#[derive(Clone, Debug, Default)]
pub struct SearchState {
    pub key: String,
    pub revision: u64,
    /// `idle` | `searching` | `probing` | `ready` | `error`
    pub phase: String,
    pub list: Vec<Candidate>,
    pub completed: Vec<String>,
    pub error: String,
}

/// What `search_all` returns.
#[derive(Clone, Debug, Default)]
pub struct SearchResult {
    pub list: Vec<Candidate>,
    pub probed: bool,
    /// The generation moved on while this search was in flight.
    pub cancelled: bool,
    pub cached: bool,
}

/// Output of the cross-language rescue pass.
#[derive(Clone, Debug, Default)]
pub struct RescueResult {
    pub injected: Vec<Candidate>,
    pub zh_artist: Option<String>,
    pub zh_titles: Vec<String>,
    pub cancelled: bool,
}

/// A candidate the user (or the automatic picker) settled on.
#[derive(Clone, Debug, Default)]
pub struct ChosenDoc {
    pub ok: bool,
    pub doc: Option<Value>,
    pub error: String,
    pub stale: bool,
}

#[derive(Clone, Debug)]
struct CacheEntry {
    list: Vec<Candidate>,
    at_ms: i64,
    probed: bool,
}

#[derive(Clone, Debug)]
struct LyricHit {
    value: ProviderResult,
    at_ms: i64,
    bytes: usize,
}

#[derive(Clone, Debug)]
struct MemoryEntry {
    source: String,
    cand: Option<Candidate>,
    at_ms: i64,
    /// Automatic picks live in memory only: after a restart the app picks again
    /// so a mediocre first pick cannot stick across sessions.
    auto_pick: bool,
}

#[derive(Debug, Default)]
struct ProbeOutcome {
    source: String,
    id: Option<String>,
    verified: Option<Verified>,
    extra_verified: Vec<(String, Verified)>,
    bad_ids: Vec<String>,
    timed_out: bool,
    soft: bool,
    failed: bool,
    error: Option<String>,
    cancelled: bool,
}

impl ProbeOutcome {
    fn of(source: &str) -> Self {
        Self {
            source: source.to_string(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProbeSummary {
    pub ok_count: usize,
    pub soft_count: usize,
    pub dead_count: usize,
    /// No soft failure happened, so the round may be cached as authoritative.
    pub authoritative: bool,
}

type ConfigFn = Arc<dyn Fn() -> Value + Send + Sync>;
type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// The service. One instance per app; the provider list is fixed at
/// construction and every cache is internally synchronised.
pub struct LyricsService {
    data_dir: PathBuf,
    config: ConfigFn,
    log: LogFn,
    sources: Vec<Box<dyn LyricsSource>>,
    by_id: BTreeMap<&'static str, usize>,
    search_cache: Mutex<BTreeMap<String, CacheEntry>>,
    search_states: Mutex<BTreeMap<String, SearchState>>,
    search_revision: AtomicU64,
    cache_epoch: AtomicU64,
    lyric_responses: Mutex<BTreeMap<String, LyricHit>>,
    lyric_bytes: AtomicU64,
    source_memory: Mutex<BTreeMap<String, MemoryEntry>>,
    last_chosen_docs: Mutex<BTreeMap<String, (String, String, Value)>>,
}

impl LyricsService {
    pub fn new(data_dir: impl Into<PathBuf>, config: ConfigFn, log: LogFn) -> Self {
        Self::with_sources(data_dir, config, log, create_sources())
    }

    pub fn with_sources(
        data_dir: impl Into<PathBuf>,
        config: ConfigFn,
        log: LogFn,
        sources: Vec<Box<dyn LyricsSource>>,
    ) -> Self {
        let mut by_id = BTreeMap::new();
        for (index, source) in sources.iter().enumerate() {
            by_id.insert(source.id(), index);
        }
        Self {
            data_dir: data_dir.into(),
            config,
            log,
            sources,
            by_id,
            search_cache: Mutex::new(BTreeMap::new()),
            search_states: Mutex::new(BTreeMap::new()),
            search_revision: AtomicU64::new(0),
            cache_epoch: AtomicU64::new(0),
            lyric_responses: Mutex::new(BTreeMap::new()),
            lyric_bytes: AtomicU64::new(0),
            source_memory: Mutex::new(BTreeMap::new()),
            last_chosen_docs: Mutex::new(BTreeMap::new()),
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn source_ids(&self) -> Vec<&'static str> {
        self.sources.iter().map(|source| source.id()).collect()
    }

    /// One-time startup: read the remembered manual picks.
    pub fn init(&self) -> usize {
        self.load_source_memory()
    }

    // ------------------------------------------------------------- generation --

    pub fn epoch(&self) -> u64 {
        self.cache_epoch.load(Ordering::SeqCst)
    }

    /// Invalidate everything in flight. Called when the configuration changes
    /// enough to alter results (sources, threshold) and when the user clears the
    /// cache: a search that started under the old rules must not publish.
    pub fn invalidate(&self) -> u64 {
        self.cache_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn current(&self, epoch: u64) -> bool {
        epoch == self.epoch()
    }

    // --------------------------------------------------------------- state ----

    pub fn search_state(&self, key: &str) -> SearchState {
        if let Some(state) = Self::lock(&self.search_states).get(key) {
            return state.clone();
        }
        let hit = Self::lock(&self.search_cache).get(key).cloned();
        SearchState {
            key: key.to_string(),
            revision: 0,
            phase: if hit.is_some() { "ready" } else { "idle" }.to_string(),
            list: hit.map(|entry| entry.list).unwrap_or_default(),
            completed: Vec::new(),
            error: String::new(),
        }
    }

    fn publish(&self, key: &str, phase: &str, list: Option<Vec<Candidate>>, completed: Option<Vec<String>>, error: Option<String>) -> SearchState {
        let mut state = self.search_state(key);
        state.phase = phase.to_string();
        if let Some(list) = list {
            state.list = list;
        }
        if let Some(completed) = completed {
            state.completed = completed;
        }
        if let Some(error) = error {
            state.error = error;
        }
        state.revision = self.search_revision.fetch_add(1, Ordering::SeqCst) + 1;
        let mut states = Self::lock(&self.search_states);
        states.insert(key.to_string(), state.clone());
        if states.len() > SEARCH_CACHE_MAX {
            let oldest = states.keys().next().cloned();
            if let Some(oldest) = oldest {
                states.remove(&oldest);
            }
        }
        state
    }

    // --------------------------------------------------------------- search ---

    /// The configured match threshold. The panel filters with the same number
    /// the service ranks by, so this is part of the public surface.
    pub fn threshold(&self) -> f64 {
        (self.config)()
            .pointer("/lyrics/matchThreshold")
            .and_then(Value::as_f64)
            .unwrap_or(70.0)
    }

    fn enabled_ids(&self) -> Vec<&'static str> {
        let config = (self.config)();
        let enabled = config.pointer("/lyrics/enabledSources");
        self.sources
            .iter()
            .filter(|source| {
                enabled
                    .and_then(|map| map.get(source.id()))
                    .map(|flag| flag != &Value::Bool(false))
                    .unwrap_or(true)
            })
            .map(|source| source.id())
            .collect()
    }

    /// Metadata arrives in several packets; cached scores must follow it.
    pub fn rescore(&self, track: &Track, list: &mut [Candidate]) {
        for candidate in list.iter_mut() {
            let score = score_candidate(track, candidate);
            candidate.percent = Some(score.percent);
            candidate.match_info = Some(MatchInfo {
                score: score.score,
                percent: score.percent,
            });
        }
    }

    pub fn search_all(&self, transport: &dyn Transport, track: &Track, flags: SearchFlags) -> SearchResult {
        self.search_all_at(transport, track, flags, self.epoch())
    }

    /// A queued caller supplies its original epoch; never adopt a newer
    /// generation after a track switch between scheduling and execution.
    pub fn search_all_at(&self, transport: &dyn Transport, track: &Track, flags: SearchFlags, epoch: u64) -> SearchResult {
        self.search_all_with_progress(transport,track,flags,epoch,&mut |_|{})
    }

    /// Live playback may install a verified answer while other sources are still responding.
    pub fn search_all_with_progress(&self,transport:&dyn Transport,track:&Track,flags:SearchFlags,epoch:u64,progress:&mut dyn FnMut(&[Candidate]))->SearchResult {
        if !self.current(epoch) { return SearchResult { cancelled: true, ..Default::default() }; }
        let key = track_key(track);
        let cached = {
            let cache = Self::lock(&self.search_cache);
            cache.get(&key).cloned()
        };
        if !flags.force {
            if let Some(mut entry) = cached {
                let ttl = if entry.list.is_empty() {
                    SEARCH_CACHE_EMPTY_TTL_MS
                } else {
                    SEARCH_CACHE_TTL_MS
                };
                let fresh = now_ms() - entry.at_ms < ttl;
                // `restart` refuses an empty cached answer: that is exactly the
                // "every source says nothing" state a manual refresh overrides.
                let refused = flags.restart && entry.list.is_empty();
                if fresh && !refused {
                    self.rescore(track, &mut entry.list);
                    let mut probed = entry.probed;
                    if !probed {
                        probed = self.probe_round(transport, track, &mut entry.list, epoch);
                        if !self.current(epoch) {
                            return SearchResult { cancelled: true, ..Default::default() };
                        }
                    }
                    let list = entry.list.clone();
                    progress(&list);
                    self.store(&key, entry, probed);
                    let state = self.publish(&key, "ready", Some(list), None, None);
                    return SearchResult {
                        list: state.list,
                        probed,
                        cancelled: false,
                        cached: true,
                    };
                }
            }
        }

        self.publish(&key, "searching", Some(Vec::new()), Some(Vec::new()), None);
        let mut out: Vec<Candidate> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut completed: Vec<String> = Vec::new();
        let ids = self.enabled_ids();
        let variants = title_variants(&track.title);
        let mut all_probed=true;

        // Independent providers run together; only this collector publishes shared state.
        std::thread::scope(|scope| {
            let (sender, receiver)=std::sync::mpsc::channel();
            for id in &ids {
                let Some(index)=self.by_id.get(id).copied() else {continue;};
                let source=&self.sources[index];let variants=&variants;let sender=sender.clone();
                scope.spawn(move || {
                    let budget=BudgetTransport::new(transport,source.search_budget_ms(),epoch,&self.cache_epoch);
                    let mut rows=Vec::new();let mut seen=BTreeSet::new();
                    for variant in variants {
                        if !self.current(epoch) || budget.expired(){break;}
                        let mut query=track.clone();query.title=variant.clone();
                        match source.search(&budget,&query,&SourceSearchOptions{keep_plain:flags.keep_plain}) {
                            Ok(candidates)=>for mut row in candidates.into_iter().take(VARIANT_CANDIDATES_PER_SOURCE) {
                                if !seen.insert(row.id.clone()){continue;}
                                row.source=(*id).to_string();row.source_name=Some(source.name().to_string());
                                let score=score_candidate(track,&row);row.percent=Some(score.percent);
                                row.match_info=Some(MatchInfo{score:score.score,percent:score.percent});
                                row.cap_word=Some(source.supports().word);row.cap_trans=Some(source.supports().trans);rows.push(row);
                            },
                            Err(error)=>(self.log)(&format!("{} 搜索失败：{error}",source.name())),
                        }
                        if rows.iter().filter(|row| identity_gate(row, track)).count() >= VARIANT_CANDIDATES_PER_SOURCE {
                            break;
                        }
                    }
                    rows.retain(|row|identity_gate(row,track));
                    rows.sort_by(|a,b|b.match_info.map(|m|m.score).unwrap_or(0.).partial_cmp(&a.match_info.map(|m|m.score).unwrap_or(0.)).unwrap_or(std::cmp::Ordering::Equal));
                    let probe_total_ms = PROBE_TOTAL_BUDGET_MS.max(source.probe_budget_ms());
                    let probe_budget=BudgetTransport::new(transport,probe_total_ms,epoch,&self.cache_epoch);
                    let probed=self.probe_round(&probe_budget,track,&mut rows,epoch);
                    let _=sender.send(((*id).to_string(),rows,probed));
                });
            }
            drop(sender);
            for (id,rows,probed) in receiver {
                if !self.current(epoch){continue;}
                for row in rows {
                    if seen.insert(format!("{id}\u{1}{}",row.id)){out.push(row);}
                }
                completed.push(id);all_probed &= probed;
                progress(&out);
                self.publish(&key,"searching",Some(identity_sorted(track,&out)),Some(completed.clone()),None);
            }
        });
        if !self.current(epoch){return SearchResult{cancelled:true,..Default::default()};}

        out.sort_by(|a, b| {
            let left = b.match_info.map(|m| m.score).unwrap_or(0.0);
            let right = a.match_info.map(|m| m.score).unwrap_or(0.0);
            left.partial_cmp(&right).unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(MAX_CANDIDATES);
        let mut list: Vec<Candidate> = out.into_iter().filter(|row| identity_gate(row, track)).collect();
        self.publish(&key, "probing", Some(list.clone()), None, None);

        let probed = all_probed;
        if !self.current(epoch) {
            return SearchResult { cancelled: true, ..Default::default() };
        }
        list.sort_by(crate::picker::cand_sort);
        let entry = CacheEntry {
            list: list.clone(),
            at_ms: now_ms(),
            probed,
        };
        self.store(&key, entry, probed);
        self.publish(&key, "ready", Some(list.clone()), Some(completed), None);
        SearchResult { list, probed, cancelled: false, cached: false }
    }

    /// Cross-language rescue: when a track has an English title or romanized artist
    /// and phase-1 search did not yield a word-level match, try to recover the
    /// Chinese artist (via manual alias or consensus lookup) and Chinese title (via
    /// LRCLIB bilingual entries), then re-query Chinese providers with strict checks.
    pub fn cross_lang_rescue(
        &self,
        transport: &dyn Transport,
        track: &Track,
        raw_artist: Option<&str>,
        existing: &[Candidate],
        epoch: u64,
    ) -> RescueResult {
        use crate::crosslang::{
            CJK_RE, VERSION_MARK_RE, artist_name_variants, chinese_artist_pinyin_variants,
            consensus_chinese_artist, extract_chinese_titles, pick_l1_candidate, pick_lrclib_entry,
        };
        use crate::zhconv::to_simplified;

        if !self.current(epoch) {
            return RescueResult { cancelled: true, ..Default::default() };
        }
        let want_title = track.title.trim();
        if want_title.is_empty() {
            return RescueResult::default();
        }
        let mapped_artist = primary_artist(track);
        let title_has_cjk = CJK_RE.is_match(want_title);
        let mapped_artist_has_cjk = CJK_RE.is_match(&mapped_artist);
        if title_has_cjk && mapped_artist_has_cjk {
            return RescueResult::default();
        }

        let raw_latin_artist = raw_artist
            .map(str::trim)
            .filter(|s| !s.is_empty() && !CJK_RE.is_match(s))
            .map(str::to_string)
            .or_else(|| {
                (!mapped_artist.is_empty() && !mapped_artist_has_cjk).then(|| mapped_artist.clone())
            });
        let zh_artist_preset = mapped_artist_has_cjk.then(|| mapped_artist.clone());
        if zh_artist_preset.is_none() && raw_latin_artist.is_none() {
            return RescueResult::default();
        }

        let has_dur = track.duration_ms.is_some_and(|d| d.is_finite() && d > 0.0);
        let require_strict_lrclib_dur = raw_latin_artist.is_none();
        let need_lrclib = !title_has_cjk && (raw_latin_artist.is_some() || has_dur);
        let artist_vars = raw_latin_artist
            .as_deref()
            .map(artist_name_variants)
            .unwrap_or_else(|| {
                zh_artist_preset
                    .as_deref()
                    .map(chinese_artist_pinyin_variants)
                    .unwrap_or_default()
            });
        let title_vars = title_variants(want_title);
        let enabled = self.enabled_ids();

        let has_cjk_target = ["kugou", "netease", "qq"]
            .iter()
            .any(|id| enabled.contains(id) && self.by_id.contains_key(id));
        let has_rev_source = ["kugou", "netease"]
            .iter()
            .any(|id| enabled.contains(id) && self.by_id.contains_key(id));
        let can_resolve_l1 = has_cjk_target && (zh_artist_preset.is_some() || has_rev_source);
        let can_fallback_lrclib = !title_has_cjk
            && !existing.iter().any(|c| c.source == "lrclib")
            && enabled.contains(&"lrclib")
            && self.by_id.contains_key("lrclib");
        if !can_resolve_l1 && !can_fallback_lrclib {
            return RescueResult::default();
        }

        let mut rev_artist_lists: Vec<Vec<String>> = Vec::new();
        let mut lrc_entries: Vec<Candidate> = Vec::new();

        std::thread::scope(|scope| {
            let (tx_rev, rx_rev) = std::sync::mpsc::channel::<Vec<Vec<String>>>();
            let (tx_lrc, rx_lrc) = std::sync::mpsc::channel::<Vec<Candidate>>();

            if zh_artist_preset.is_none() && !artist_vars.is_empty() {
                for src_id in ["kugou", "netease"] {
                    if !enabled.contains(&src_id) {
                        continue;
                    }
                    let Some(idx) = self.by_id.get(src_id).copied() else { continue };
                    let source = &self.sources[idx];
                    let artist_vars = &artist_vars;
                    let tx = tx_rev.clone();
                    scope.spawn(move || {
                        let budget = BudgetTransport::new(transport, 4500, epoch, &self.cache_epoch);
                        let mut lists = Vec::new();
                        for av in artist_vars {
                            if !self.current(epoch) || budget.expired() {
                                break;
                            }
                            let q = Track {
                                title: String::new(),
                                artist: Some(av.clone()),
                                ..Default::default()
                            };
                            if let Ok(rows) = source.search(&budget, &q, &SourceSearchOptions::default()) {
                                let names: Vec<String> = rows
                                    .into_iter()
                                    .take(5)
                                    .flat_map(|r| r.artist_list().unwrap_or_default())
                                    .collect();
                                if !names.is_empty() {
                                    lists.push(names);
                                }
                            }
                        }
                        let _ = tx.send(lists);
                    });
                }
            }
            drop(tx_rev);

            if need_lrclib {
                if let Some(idx) = self.by_id.get("lrclib").copied() {
                    if enabled.contains(&"lrclib") {
                        let source = &self.sources[idx];
                        let artist_vars = &artist_vars;
                        let title_vars = &title_vars;
                        let tx = tx_lrc.clone();
                        scope.spawn(move || {
                            let budget = BudgetTransport::new(transport, 5000, epoch, &self.cache_epoch);
                            let mut out = Vec::new();
                            let mut seen = BTreeSet::new();
                            let avs: Vec<Option<String>> = if artist_vars.is_empty() {
                                vec![None]
                            } else {
                                artist_vars.iter().cloned().map(Some).collect()
                            };
                            for tv in title_vars.iter().take(2) {
                                for av in &avs {
                                    if !self.current(epoch) || budget.expired() {
                                        break;
                                    }
                                    let q = Track {
                                        title: tv.clone(),
                                        artist: av.clone(),
                                        duration_ms: track.duration_ms,
                                        ..Default::default()
                                    };
                                    if let Ok(rows) = source.search(&budget, &q, &SourceSearchOptions { keep_plain: true }) {
                                        for r in rows {
                                            if seen.insert(format!("{}\u{1}{}", r.id, r.title)) {
                                                out.push(r);
                                            }
                                        }
                                    }
                                }
                            }
                            let _ = tx.send(out);
                        });
                    }
                }
            }
            drop(tx_lrc);

            for lists in rx_rev {
                rev_artist_lists.extend(lists);
            }
            for entries in rx_lrc {
                lrc_entries.extend(entries);
            }
        });

        if !self.current(epoch) {
            return RescueResult { cancelled: true, ..Default::default() };
        }

        let zh_artist = zh_artist_preset.or_else(|| consensus_chinese_artist(&rev_artist_lists));
        let zh_titles = if title_has_cjk {
            vec![want_title.to_string()]
        } else {
            extract_chinese_titles(
                &lrc_entries,
                &title_vars,
                zh_artist.as_deref(),
                track.duration_ms,
                require_strict_lrclib_dur,
            )
        };

        (self.log)(&format!(
            "跨语言救援：[{}] {} -> 中文歌手「{}」，中文歌名候选 {:?}",
            want_title,
            mapped_artist,
            zh_artist.as_deref().unwrap_or("无"),
            zh_titles
        ));

        let mut rescued: Vec<Candidate> = Vec::new();
        if let Some(ref zh_a) = zh_artist {
            if !zh_titles.is_empty() {
                let mut simp_titles = Vec::new();
                for zt in &zh_titles {
                    let s = to_simplified(zt);
                    if !s.is_empty() && !simp_titles.contains(&s) {
                        simp_titles.push(s);
                    }
                }
                let want_versioned = VERSION_MARK_RE.is_match(want_title);
                std::thread::scope(|scope| {
                    let (tx, rx) = std::sync::mpsc::channel::<Candidate>();
                    for src_id in ["kugou", "netease", "qq"] {
                        if !enabled.contains(&src_id) {
                            continue;
                        }
                        let Some(idx) = self.by_id.get(src_id).copied() else { continue };
                        let source = &self.sources[idx];
                        let simp_titles = &simp_titles;
                        let tx = tx.clone();
                        scope.spawn(move || {
                            let budget = BudgetTransport::new(transport, 4500, epoch, &self.cache_epoch);
                            for zt in simp_titles {
                                if !self.current(epoch) || budget.expired() {
                                    break;
                                }
                                let q = Track {
                                    title: zt.clone(),
                                    artist: Some(zh_a.clone()),
                                    duration_ms: track.duration_ms,
                                    ..Default::default()
                                };
                                if let Ok(rows) = source.search(&budget, &q, &SourceSearchOptions::default()) {
                                    if let Some(mut cand) = pick_l1_candidate(&rows, zh_a, zt, track.duration_ms, want_versioned) {
                                        cand.source = src_id.to_string();
                                        cand.source_name = Some(source.name().to_string());
                                        cand.cap_word = Some(source.supports().word);
                                        cand.cap_trans = Some(source.supports().trans);
                                        cand.rescue = Some(true);
                                        let score = score_candidate(track, &cand);
                                        cand.percent = Some(score.percent);
                                        cand.match_info = Some(MatchInfo {
                                            score: score.score,
                                            percent: score.percent,
                                        });
                                        let _ = tx.send(cand);
                                        break;
                                    }
                                }
                            }
                        });
                    }
                    drop(tx);
                    for cand in rx {
                        if !existing.iter().any(|e| e.source == cand.source && e.id == cand.id)
                            && !rescued.iter().any(|e| e.source == cand.source && e.id == cand.id)
                        {
                            rescued.push(cand);
                        }
                    }
                });
            }
        }

        if rescued.is_empty() && !title_has_cjk && !existing.iter().any(|c| c.source == "lrclib") {
            let synced_only: Vec<Candidate> = lrc_entries
                .into_iter()
                .filter(|e| {
                    e.extra
                        .as_ref()
                        .and_then(|x| x.get("syncedLyrics"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.trim().is_empty())
                        && (has_dur || !VERSION_MARK_RE.is_match(&e.title))
                })
                .collect();
            if let Some(mut cand) = pick_lrclib_entry(&synced_only, &title_vars, &artist_vars, track.duration_ms) {
                if let Some(idx) = self.by_id.get("lrclib").copied() {
                    let source = &self.sources[idx];
                    cand.source = "lrclib".to_string();
                    cand.source_name = Some(source.name().to_string());
                    cand.cap_word = Some(source.supports().word);
                    cand.cap_trans = Some(source.supports().trans);
                    cand.rescue = Some(true);
                    let score = score_candidate(track, &cand);
                    cand.percent = Some(score.percent);
                    cand.match_info = Some(MatchInfo {
                        score: score.score,
                        percent: score.percent,
                    });
                    rescued.push(cand);
                }
            }
        }

        if !self.current(epoch) {
            return RescueResult { cancelled: true, ..Default::default() };
        }

        if !rescued.is_empty() {
            let probe_budget = BudgetTransport::new(transport, PROBE_TOTAL_BUDGET_MS, epoch, &self.cache_epoch);
            self.probe_round(&probe_budget, track, &mut rescued, epoch);
            rescued.retain(|r| {
                r.probe_failed != Some(true)
                    && r.verified.as_ref().is_some_and(|v| {
                        v.timeline != Some(false) && v.lines.is_none_or(|n| n > 0)
                    })
            });
        }

        if !self.current(epoch) {
            return RescueResult { cancelled: true, ..Default::default() };
        }

        if !rescued.is_empty() {
            let key = track_key(track);
            let mut cache = Self::lock(&self.search_cache);
            let entry = cache.entry(key).or_insert_with(|| CacheEntry {
                list: Vec::new(),
                at_ms: now_ms(),
                probed: true,
            });
            for row in &rescued {
                if !entry.list.iter().any(|c| c.source == row.source && c.id == row.id) {
                    entry.list.push(row.clone());
                }
            }
        }

        RescueResult {
            injected: rescued,
            zh_artist,
            zh_titles,
            cancelled: false,
        }
    }

    fn store(&self, key: &str, entry: CacheEntry, _probed: bool) {
        let mut cache = Self::lock(&self.search_cache);
        cache.insert(key.to_string(), entry);
        while cache.len() > SEARCH_CACHE_MAX {
            let oldest = cache.keys().next().cloned();
            match oldest {
                Some(oldest) => {
                    cache.remove(&oldest);
                }
                None => break,
            }
        }
    }

    // ---------------------------------------------------------------- probe ---

    /// Verify up to three candidates per source. Returns whether the round was
    /// authoritative (`probed`), i.e. no soft failure forced a retry next time.
    pub fn probe_round(&self, transport: &dyn Transport, _track: &Track, list: &mut Vec<Candidate>, epoch: u64) -> bool {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, row) in list.iter().enumerate() {
            groups.entry(row.source.clone()).or_default().push(index);
        }
        if groups.len()>1 {
            let mut batches=std::thread::scope(|scope| {
                let jobs:Vec<_>=groups.values().map(|indices| {
                    let mut rows:Vec<_>=indices.iter().map(|i|list[*i].clone()).collect();
                    scope.spawn(move || {
                        let per_probe_ms = rows
                            .first()
                            .and_then(|r| self.by_id.get(r.source.as_str()))
                            .map(|&idx| self.sources[idx].probe_budget_ms())
                            .unwrap_or(PROBE_BUDGET_MS);
                        let budget=BudgetTransport::new(transport,PROBE_TOTAL_BUDGET_MS.max(per_probe_ms),epoch,&self.cache_epoch);
                        let ok=self.probe_round(&budget,_track,&mut rows,epoch);(rows,ok)
                    })
                }).collect();
                jobs.into_iter().map(|job|job.join().expect("lyric probe worker")).collect::<Vec<_>>()
            });
            if !self.current(epoch){return false;}
            let ok=batches.iter().all(|(_,ok)|*ok);
            let by_key:BTreeMap<_,_>=batches.drain(..).flat_map(|(rows,_)|rows).map(|c|((c.source.clone(),c.id.clone()),c)).collect();
            *list=list.iter().filter_map(|c|by_key.get(&(c.source.clone(),c.id.clone())).cloned()).collect();
            return ok;
        }
        let mut outcomes: Vec<ProbeOutcome> = Vec::new();
        for (source, indices) in &groups {
            if !self.current(epoch) {
                return false;
            }
            if !self.by_id.contains_key(source.as_str()) {
                continue;
            }
            let mut outcome = ProbeOutcome::of(source);
            let try_indices: Vec<usize> = indices.iter().copied().take(3).collect();
            if let Some(first) = try_indices.first() {
                list[*first].best_of_source = Some(true);
            }
            let per_probe_ms = self
                .by_id
                .get(source.as_str())
                .map(|&idx| self.sources[idx].probe_budget_ms())
                .unwrap_or(PROBE_BUDGET_MS);
            let mut verified_rows: Vec<(String, Verified)> = Vec::new();
            for index in &try_indices {
                let row = list[*index].clone();
                let budget=BudgetTransport::new(transport,per_probe_ms,epoch,&self.cache_epoch);
                match self.fetch_lyrics(&budget, source, &row) {
                    Ok(result) => {
                        if !self.current(epoch) {
                            outcome.cancelled = true;
                            break;
                        }
                        if !has_timeline(&result.raw) {
                            outcome.bad_ids.push(row.id.clone());
                            continue;
                        }
                        let mut best = row.clone();
                        best.match_info = Some(MatchInfo { score: 0.0, percent: row.percent.unwrap_or(0.0) });
                        let doc = build_doc(
                            &ScoredCandidate { id: source.clone(), best },
                            &raw_lyrics(&result),
                        );
                        let Some(doc) = doc else {
                            outcome.bad_ids.push(row.id.clone());
                            continue;
                        };
                        if doc.instrumental {
                            let verified = Verified {
                                word: Some(false),
                                trans: Some(false),
                                bg: Some(false),
                                lines: Some(1),
                                timeline: Some(true),
                                first_ms: Some(0.0),
                                foreign: Some(false),
                            };
                            verified_rows.push((row.id.clone(), verified));
                            break;
                        }
                        if doc.lines.len() < 3 {
                            outcome.bad_ids.push(row.id.clone());
                            continue;
                        }
                        let first_ms = doc.lines.first().map(|line| line.start as f64).unwrap_or(0.0);
                        let has_bg = doc.has_background
                            || doc.lines.iter().any(|l| {
                                l.bg.is_some()
                                    || l.bg_words.as_ref().is_some_and(|w| !w.is_empty())
                            });
                        let verified = Verified {
                            word: Some(doc.has_word_level),
                            trans: Some(doc.has_translation),
                            bg: Some(has_bg),
                            lines: Some(doc.lines.len() as i64),
                            timeline: Some(true),
                            first_ms: Some(first_ms),
                            foreign: Some(has_foreign_lyrics(&doc.lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>())),
                        };
                        verified_rows.push((row.id.clone(), verified));
                        if row.duration_sec.unwrap_or(0.0) > 0.0 {
                            break;
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if is_timeout(&message) {
                            outcome.timed_out = true;
                            break;
                        }
                        if is_definitive(&message) {
                            outcome.bad_ids.push(row.id.clone());
                            continue;
                        }
                        outcome.soft = true;
                        if outcome.error.is_none() {
                            outcome.error = Some(message);
                        }
                        continue;
                    }
                }
            }
            if let Some((id, verified)) = verified_rows.first().cloned() {
                outcome.id = Some(id);
                outcome.verified = Some(verified);
                outcome.extra_verified = verified_rows.into_iter().skip(1).collect();
            } else if outcome.soft || outcome.timed_out {
                outcome.id = indices.first().map(|index| list[*index].id.clone());
            } else {
                outcome.failed = true;
                outcome.id = indices.first().map(|index| list[*index].id.clone());
            }
            outcomes.push(outcome);
        }

        let mut ok = 0;
        let mut soft = 0;
        let mut dead = 0;
        for outcome in &outcomes {
            if outcome.cancelled {
                return false;
            }
            for bad in &outcome.bad_ids {
                if let Some(row) = list.iter_mut().find(|row| row.source == outcome.source && &row.id == bad) {
                    row.probe_failed = Some(true);
                }
            }
            for (id, verified) in &outcome.extra_verified {
                if let Some(row) = list.iter_mut().find(|row| row.source == outcome.source && &row.id == id) {
                    row.verified = Some(verified.clone());
                }
            }
            let Some(id) = outcome.id.as_ref() else {
                continue;
            };
            if let Some(row) = list.iter_mut().find(|row| row.source == outcome.source && &row.id == id) {
                if outcome.failed {
                    row.probe_failed = Some(true);
                } else if let Some(verified) = outcome.verified.clone() {
                    row.verified = Some(verified);
                } else if outcome.timed_out {
                    row.probe_timed_out = Some(true);
                    row.probe_error = outcome.error.clone();
                }
            }
            if outcome.verified.is_some() {
                ok += 1;
            } else if outcome.soft {
                soft += 1;
            } else if outcome.failed {
                dead += 1;
            }
        }
        if soft > 0 || ok == 0 {
            let sample = outcomes.iter().find(|outcome| outcome.soft).and_then(|outcome| outcome.error.clone());
            (self.log)(&format!(
                "探测：可用源 {ok} / 软失败源 {soft} / 确认不可用源 {dead}（候选 {} 条）{}",
                list.len(),
                sample.map(|text| format!(" 例：{text}")).unwrap_or_default()
            ));
        }
        soft == 0
    }

    /// Same candidate, same download: the panel and the picker share one fetch,
    /// and a short TTL keeps a re-open instant without pinning stale lyrics.
    fn fetch_lyrics(&self, transport: &dyn Transport, source_id: &str, candidate: &Candidate) -> Result<ProviderResult, ProviderError> {
        let key = lyric_cache_key(source_id, candidate);
        if let Some(key) = key.as_ref() {
            let mut cache = Self::lock(&self.lyric_responses);
            if let Some(hit) = cache.get(key).cloned() {
                if now_ms() - hit.at_ms < LYRIC_RESPONSE_TTL_MS {
                    return Ok(hit.value);
                }
                cache.remove(key);
                self.lyric_bytes.fetch_sub(hit.bytes as u64, Ordering::SeqCst);
            }
        }
        let Some(index) = self.by_id.get(source_id).copied() else {
            return Err(ProviderError::Message("未知歌词来源".to_string()));
        };
        let value = self.sources[index].lyrics(transport, candidate)?;
        if let Some(key) = key {
            let bytes = value.raw.len() + value.translation_lrc.as_ref().map(String::len).unwrap_or(0);
            let within_budget = bytes <= LYRIC_RESPONSE_ITEM_BYTES
                && self.lyric_bytes.load(Ordering::SeqCst) as usize + bytes <= LYRIC_RESPONSE_BYTES;
            if within_budget {
                let mut cache = Self::lock(&self.lyric_responses);
                cache.insert(key, LyricHit { value: value.clone(), at_ms: now_ms(), bytes });
                self.lyric_bytes.fetch_add(bytes as u64, Ordering::SeqCst);
                while cache.len() > LYRIC_RESPONSE_MAX {
                    let oldest = cache.keys().next().cloned();
                    match oldest {
                        Some(oldest) => {
                            if let Some(removed) = cache.remove(&oldest) {
                                self.lyric_bytes.fetch_sub(removed.bytes as u64, Ordering::SeqCst);
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        Ok(value)
    }

    // -------------------------------------------------------------- choosing --

    /// The user picked a row. The clicked candidate goes first; up to three more
    /// from the same source follow, because one upload can be undecodable while
    /// the next has full word timing.
    ///
    /// `epoch` is the generation the caller observed when the row was clicked.
    /// If the cache was invalidated meanwhile the answer is `stale` and nothing
    /// is written — a slow download must never replace the current song's doc.
    pub fn use_candidate(&self, transport: &dyn Transport, track: &Track, source_id: &str, cand: &Candidate, epoch: u64, auto_pick: bool, persist: bool) -> ChosenDoc {
        if !self.current(epoch) {
            return ChosenDoc { ok: false, stale: true, ..Default::default() };
        }
        if !self.by_id.contains_key(source_id) {
            return ChosenDoc { ok: false, error: "未知歌词来源".to_string(), ..Default::default() };
        }
        let mut attempts: Vec<Candidate> = vec![cand.clone()];
        let key = track_key(track);
        let min_percent = cand
            .match_info
            .map(|m| m.percent)
            .or(cand.percent)
            .filter(|&p| p >= self.threshold())
            .map(|_| self.threshold());
        if let Some(entry) = Self::lock(&self.search_cache).get(&key).cloned() {
            for row in entry.list {
                let row_percent = row
                    .match_info
                    .map(|m| m.percent)
                    .or(row.percent)
                    .unwrap_or(0.0);
                if row.source == source_id
                    && row.id != cand.id
                    && row.probe_failed != Some(true)
                    && min_percent.is_none_or(|t| row_percent >= t)
                    && same_song(track, &row)
                {
                    attempts.push(row);
                }
            }
        }
        let mut first_error: Option<String> = None;
        for attempt in attempts.iter().take(4) {
            if !self.current(epoch) {
                return ChosenDoc { ok: false, stale: true, ..Default::default() };
            }
            let lyrics = match self.fetch_lyrics(transport, source_id, attempt) {
                Ok(lyrics) => lyrics,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error.to_string());
                    }
                    continue;
                }
            };
            if !self.current(epoch) {
                return ChosenDoc { ok: false, stale: true, ..Default::default() };
            }
            let mut best = attempt.clone();
            best.match_info = Some(MatchInfo { score: 0.0, percent: attempt.percent.unwrap_or(0.0) });
            let Some(doc) = build_doc(&ScoredCandidate { id: source_id.to_string(), best }, &raw_lyrics(&lyrics)) else {
                if first_error.is_none() {
                    first_error = Some("歌词解析失败".to_string());
                }
                continue;
            };
            let mut final_doc = compose(&[doc], track, Vec::new(), None);
            let ok = final_doc.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if !ok {
                return ChosenDoc { ok: false, doc: Some(final_doc), error: String::new(), stale: false };
            }
            if !self.current(epoch) {
                return ChosenDoc { ok: false, stale: true, ..Default::default() };
            }
            if let Some(object) = final_doc.as_object_mut() {
                object.insert("manual".to_string(), Value::Bool(!auto_pick));
                object.insert("auto".to_string(), Value::Bool(auto_pick));
            }
            // Remember *before* any document cache write: the fingerprint of the
            // remembered pick is what tells a later load which cache entry is
            // still the user's choice.
            {
                let mut memory = Self::lock(&self.source_memory);
                memory.insert(
                    key.clone(),
                    MemoryEntry { source: source_id.to_string(), cand: Some(attempt.clone()), at_ms: now_ms(), auto_pick },
                );
                evict_memory(&mut memory);
            }
            if persist {
                self.save_source_memory();
            }
            {
                let mut docs = Self::lock(&self.last_chosen_docs);
                docs.insert(
                    key.clone(),
                    (source_id.to_string(), chosen_id(&final_doc, &attempt.id), final_doc.clone()),
                );
                while docs.len() > 16 {
                    if let Some(oldest) = docs.keys().next().cloned() {
                        docs.remove(&oldest);
                    } else {
                        break;
                    }
                }
            }
            (self.log)(&format!(
                "已记住：[{}] {source_id} key={}{}",
                track.title,
                key,
                if auto_pick { "（自动，仅内存）" } else { "（手选，落盘）" }
            ));
            return ChosenDoc { ok: true, doc: Some(final_doc), error: String::new(), stale: false };
        }
        ChosenDoc {
            ok: false,
            doc: None,
            error: format!(
                "{}已尝试 {} 份候选均未通过校验（{}）",
                source_id,
                attempts.len().min(4),
                first_error.unwrap_or_else(|| "原因未知".to_string())
            ),
            stale: false,
        }
    }

    // -------------------------------------------------------- source memory ---

    fn memory_path(&self) -> PathBuf {
        self.data_dir.join("source-memory.json")
    }

    pub fn load_source_memory(&self) -> usize {
        let Ok(text) = std::fs::read_to_string(self.memory_path()) else {
            return 0;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return 0;
        };
        let Some(object) = value.as_object() else {
            return 0;
        };
        let mut memory = Self::lock(&self.source_memory);
        for (key, entry) in object {
            let Some(source) = entry.get("source").and_then(Value::as_str) else {
                continue;
            };
            let cand = entry.get("cand").and_then(|cand| serde_json::from_value::<Candidate>(cand.clone()).ok());
            memory.insert(
                key.clone(),
                MemoryEntry {
                    source: source.to_string(),
                    cand,
                    at_ms: entry.get("at").and_then(Value::as_i64).unwrap_or(0),
                    auto_pick: entry.get("auto").and_then(Value::as_bool).unwrap_or(false),
                },
            );
        }
        memory.len()
    }

    /// Only manual picks are written: an automatic pick is a session-local
    /// decision, and persisting it would make a mediocre first answer stick.
    pub fn save_source_memory(&self) {
        let memory = Self::lock(&self.source_memory);
        let manual: BTreeMap<&String, Value> = memory
            .iter()
            .filter(|(_, entry)| !entry.auto_pick)
            .map(|(key, entry)| {
                (
                    key,
                    serde_json::json!({
                        "source": entry.source,
                        "cand": entry.cand,
                        "at": entry.at_ms,
                    }),
                )
            })
            .collect();
        let Ok(text) = serde_json::to_string(&manual) else {
            return;
        };
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(self.memory_path(), text);
    }

    pub fn remembered_source(&self, track: &Track) -> Option<(String, Option<Candidate>)> {
        let memory = Self::lock(&self.source_memory);
        memory
            .get(&track_key(track))
            .map(|entry| (entry.source.clone(), entry.cand.clone()))
    }

    pub fn remembered_manual_source(&self,track:&Track)->Option<(String,Option<Candidate>)> {
        Self::lock(&self.source_memory).get(&track_key(track)).filter(|entry|!entry.auto_pick).map(|entry|(entry.source.clone(),entry.cand.clone()))
    }

    /// Promote a session-local automatic pick to a persisted preference once the
    /// listener has played normally past half the song, and persist its lyric
    /// document into the bounded on-disk cache.
    pub fn promote_source_memory(&self, track: &Track, source_id: &str, id: &str) -> bool {
        let key = track_key(track);
        let doc_saved = self.save_cached_doc_for(&key, source_id, id);
        let promoted = {
            let mut memory = Self::lock(&self.source_memory);
            let Some(entry) = memory.get_mut(&key) else {
                return doc_saved;
            };
            if entry.source != source_id || entry.cand.as_ref().is_some_and(|c| c.id != id) {
                return doc_saved;
            }
            if !entry.auto_pick {
                return doc_saved;
            }
            entry.auto_pick = false;
            entry.at_ms = now_ms();
            true
        };
        if promoted {
            self.save_source_memory();
            (self.log)(&format!("自动选择已转正落盘：[{}] {source_id} key={key}", track.title));
        }
        promoted || doc_saved
    }

    fn doc_cache_dir(&self) -> PathBuf {
        self.data_dir.join("lyrics-cache")
    }

    fn doc_cache_limit_bytes(&self) -> u64 {
        let cfg = (self.config)();
        let mb = cfg
            .get("lyrics")
            .and_then(|l| l.get("lyricsCacheMB"))
            .and_then(Value::as_f64)
            .unwrap_or(50.0)
            .clamp(10.0, 500.0);
        (mb * 1024.0 * 1024.0).round() as u64
    }

    fn doc_cache_path(&self, key: &str, source_id: &str, id: &str) -> PathBuf {
        let h1 = fnv1a64(&[key.as_bytes(), b"\x01", source_id.as_bytes(), b"\x01", id.as_bytes()]);
        let h2 = fnv1a64(&[id.as_bytes(), b"\x02", source_id.as_bytes(), b"\x02", key.as_bytes()]);
        self.doc_cache_dir().join(format!("{h1:016x}{h2:016x}.json"))
    }

    fn save_cached_doc_for(&self, key: &str, source_id: &str, id: &str) -> bool {
        let doc = {
            let docs = Self::lock(&self.last_chosen_docs);
            docs.get(key)
                .filter(|(s, doc_id, _)| s == source_id && doc_id == id)
                .map(|(_, _, doc)| doc.clone())
        };
        let Some(doc) = doc else {
            return false;
        };
        let dir = self.doc_cache_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        let payload = serde_json::json!({
            "trackKey": key,
            "source": source_id,
            "id": id,
            "doc": doc,
        });
        let Ok(bytes) = serde_json::to_vec(&payload) else {
            return false;
        };
        let path = self.doc_cache_path(key, source_id, id);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_err() {
            return false;
        }
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        enforce_doc_cache_budget(&dir, self.doc_cache_limit_bytes());
        true
    }

    pub fn load_cached_doc(&self, track: &Track, source_id: &str, id: &str) -> Option<Value> {
        let key = track_key(track);
        let path = self.doc_cache_path(&key, source_id, id);
        let text = std::fs::read_to_string(&path).ok()?;
        let mut parsed: Value = serde_json::from_str(&text).ok()?;
        if parsed.get("trackKey").and_then(Value::as_str) != Some(key.as_str())
            || parsed.get("source").and_then(Value::as_str) != Some(source_id)
            || parsed.get("id").and_then(Value::as_str) != Some(id)
        {
            return None;
        }
        let doc = parsed.get_mut("doc").map(Value::take)?;
        // Refresh mtime only when older than 10 minutes to avoid redundant SSD writes on every play.
        if let Ok(meta) = std::fs::metadata(&path) {
            let needs_touch = meta
                .modified()
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_none_or(|age| age.as_secs() >= 600);
            if needs_touch {
                if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
                    let _ = file.set_modified(SystemTime::now());
                }
            }
        }
        Some(doc)
    }

    pub fn clear_source_memory(&self) {
        Self::lock(&self.source_memory).clear();
        let _ = std::fs::write(self.memory_path(), "{}");
    }

    pub fn clear_search_cache(&self) {
        Self::lock(&self.search_cache).clear();
        Self::lock(&self.search_states).clear();
        Self::lock(&self.lyric_responses).clear();
        self.lyric_bytes.store(0, Ordering::SeqCst);
        self.invalidate();
    }
}

fn chosen_id(doc: &Value, fallback: &str) -> String {
    doc.get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for part in parts {
        for &b in *part {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

fn enforce_doc_cache_budget(dir: &Path, limit_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext == Some("tmp") {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if ext != Some("json") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let size = meta.len();
        let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
        total = total.saturating_add(size);
        files.push((mtime, size, path));
    }
    if total <= limit_bytes {
        return;
    }
    files.sort_by_key(|(mtime, _, _)| *mtime);
    for (_, size, path) in files {
        if total <= limit_bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn evict_memory(memory: &mut BTreeMap<String, MemoryEntry>) {
    while memory.len() > SOURCE_MEMORY_MAX {
        let oldest = memory.keys().next().cloned();
        match oldest {
            Some(oldest) => {
                memory.remove(&oldest);
            }
            None => break,
        }
    }
}

/// `identityGate` applied to the whole pool, sorted by score.
fn identity_sorted(track: &Track, out: &[Candidate]) -> Vec<Candidate> {
    let mut list: Vec<Candidate> = out.iter().filter(|row| identity_gate(row, track)).cloned().collect();
    list.sort_by(|a, b| {
        let left = b.match_info.map(|m| m.score).unwrap_or(0.0);
        let right = a.match_info.map(|m| m.score).unwrap_or(0.0);
        left.partial_cmp(&right).unwrap_or(std::cmp::Ordering::Equal)
    });
    list
}

/// The same song by title + artist, and by duration when both sides know it.
/// A namesake cover technically yields lyrics; inserting it would be a different
/// song's text under this title.
fn same_song(track: &Track, candidate: &Candidate) -> bool {
    use crate::identity::{artist_ok, title_ok};
    if candidate.title.is_empty() || !title_ok(&candidate.title, &track.title, None) {
        return false;
    }
    let artists = candidate.artist_list().unwrap_or_default();
    if !artist_ok(&artists, &primary_artist(track)) {
        return false;
    }
    let duration = track.duration_ms.unwrap_or(0.0);
    let candidate_duration = candidate.duration_sec.unwrap_or(0.0) * 1000.0;
    if duration > 0.0 && candidate_duration > 0.0 {
        return (candidate_duration - duration).abs() <= 8000.0;
    }
    candidate.percent.unwrap_or(0.0) >= 95.0
}

fn lyric_cache_key(source_id: &str, candidate: &Candidate) -> Option<String> {
    if candidate.id.is_empty() {
        return None;
    }
    let extra = |key: &str| {
        candidate
            .extra
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    serde_json::to_string(&[source_id, candidate.id.as_str(), extra("file").as_str(), extra("mid").as_str(), extra("id").as_str()]).ok()
}

fn raw_lyrics(result: &ProviderResult) -> RawLyrics {
    RawLyrics {
        raw: result.raw.clone(),
        translation_list: result.translation_list.clone(),
        romanization_list: result.romanization_list.clone(),
        translation_lrc: result.translation_lrc.clone(),
        roma_lrc: result.roma_lrc.clone(),
        reference: Some(result.reference.clone()),
    }
}

/// Any of the three timeline shapes the four providers emit: LRC, QRC, TTML.
fn has_timeline(raw: &str) -> bool {
    static LRC: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\[\d{1,3}:\d{1,2}").expect("lrc timing"));
    static QRC: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\[\d+,\d+\]").expect("qrc timing"));
    static TTML: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"<p\b[^>]*\bbegin\s*=\s*"[\d:]"#).expect("ttml timing"));
    LRC.is_match(raw) || QRC.is_match(raw) || TTML.is_match(raw)
}

fn is_timeout(message: &str) -> bool {
    message.contains("超时") || message.to_lowercase().contains("timeout")
}

fn is_definitive(message: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(&format!("(?i){DEFINITIVE_PROBE_FAIL}")).expect("probe fail regex"));
    RE.is_match(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_gate_accepts_all_three_shapes() {
        assert!(has_timeline("[00:12.34]hello"));
        assert!(has_timeline("[1234,567]hello(0,100)"));
        assert!(has_timeline("<p begin=\"1.5s\" end=\"3s\">hi</p>"));
        assert!(!has_timeline("just some plain text"));
    }

    #[test]
    fn only_definitive_failures_remove_a_candidate() {
        // The reference list is narrow: 没有…歌词 / 无歌词 / 未找到 / 找不到 /
        // 不存在 / 解密失败 / …. A provider saying "未提供此曲目的歌词" does
        // *not* match it, so in the shipped app that answer leaves the row in
        // the panel as unverified instead of deleting it. The port keeps that
        // quirk: tightening the regex here would drop candidates the reference
        // still shows, and the two stores would diverge.
        assert!(!is_definitive("酷狗音乐未提供此曲目的歌词"));
        assert!(is_definitive("未找到这首歌的歌词"));
        assert!(is_definitive("QQ 音乐歌词解密失败"));
        assert!(!is_definitive("请求超时"));
        assert!(!is_definitive("connection reset by peer"));
    }

    #[test]
    fn timeout_is_recognised_in_both_spellings() {
        assert!(is_timeout("探测超时"));
        assert!(is_timeout("operation timeout"));
        assert!(!is_timeout("HTTP 503"));
    }
}

#[cfg(test)] mod request_budget_tests {
 use super::*;
 struct Capture(Mutex<Vec<u64>>);
 impl Transport for Capture {fn send(&self,r:&HttpRequest)->Result<HttpResponse,HttpError>{self.0.lock().unwrap().push(r.timeout_ms);Ok(HttpResponse::default())}}
 #[test] fn provider_requests_share_deadline_and_stop_on_epoch_change() {
  let inner=Capture(Mutex::new(Vec::new()));let epoch=AtomicU64::new(3);
  let limited=BudgetTransport::new(&inner,100,3,&epoch);
  limited.send(&HttpRequest::get("https://example.invalid",12000)).unwrap();
  assert!(inner.0.lock().unwrap()[0]<=100);
  epoch.store(4,Ordering::SeqCst);assert!(matches!(limited.send(&HttpRequest::default()),Err(HttpError::Timeout)));
  assert_eq!(inner.0.lock().unwrap().len(),1);
 }
}
