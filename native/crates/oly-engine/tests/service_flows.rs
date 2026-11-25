//! The lyrics service end to end: fan-out, probe gating, epoch fencing and
//! source memory.
//!
//! Every provider here is scripted, so the assertions are about the service's
//! own decisions — which rows survive a probe, which answers are believed, and
//! which writer is allowed to land — rather than about any particular website.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde_json::{Value, json};

use oly_engine::net::{HttpError, HttpRequest, HttpResponse, Transport};
use oly_engine::providers::{
    LyricsSource, ProviderError, ProviderResult, SearchOptions, Supports,
};
use oly_engine::service::{LyricsService, SearchFlags};
use oly_engine::strings::track_key;
use oly_engine::types::{Candidate, Track};

// ------------------------------------------------------------- harness ------

/// Three timed lines: the document builder rejects anything shorter, so every
/// scripted lyric body has to clear that bar.
const LRC: &str = "[00:00.00]first line\n[00:02.00]second line\n[00:04.00]third line\n";

struct NoTransport;

impl Transport for NoTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(HttpError::Transport(format!(
            "scripted source must not hit the network: {}",
            request.url
        )))
    }
}

#[derive(Default)]
struct Counters {
    searches: AtomicUsize,
    fetches: AtomicUsize,
}

struct FakeSource {
    id: &'static str,
    name: &'static str,
    rows: Vec<Candidate>,
    search_error: Option<&'static str>,
    lyric_error: Option<&'static str>,
    counters: Arc<Counters>,
    before_search: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl FakeSource {
    fn new(id: &'static str, rows: Vec<Candidate>, counters: &Arc<Counters>) -> Self {
        Self {
            id,
            name: id,
            rows,
            search_error: None,
            lyric_error: None,
            counters: counters.clone(),
            before_search: None,
        }
    }

    fn search_error(mut self, message: &'static str) -> Self {
        self.search_error = Some(message);
        self
    }

    fn lyric_error(mut self, message: &'static str) -> Self {
        self.lyric_error = Some(message);
        self
    }

    fn before_search(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.before_search = Some(hook);
        self
    }
}

impl LyricsSource for FakeSource {
    fn id(&self) -> &'static str {
        self.id
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn supports(&self) -> Supports {
        Supports { word: true, trans: true, roma: false }
    }

    fn search(
        &self,
        _transport: &dyn Transport,
        _track: &Track,
        _opts: &SearchOptions,
    ) -> Result<Vec<Candidate>, ProviderError> {
        self.counters.searches.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = self.before_search.as_ref() {
            hook();
        }
        if let Some(message) = self.search_error {
            return Err(ProviderError::Message(message.to_string()));
        }
        Ok(self.rows.clone())
    }

    fn lyrics(
        &self,
        _transport: &dyn Transport,
        _candidate: &Candidate,
    ) -> Result<ProviderResult, ProviderError> {
        self.counters.fetches.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = self.lyric_error {
            return Err(ProviderError::Message(message.to_string()));
        }
        Ok(ProviderResult {
            raw: LRC.to_string(),
            format: "lrc".to_string(),
            reference: "scripted".to_string(),
            ..Default::default()
        })
    }
}

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "oly-service-{}-{}-{name}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

fn config(sources: &[(&str, bool)]) -> Value {
    let mut enabled = serde_json::Map::new();
    for (id, on) in sources {
        enabled.insert((*id).to_string(), Value::Bool(*on));
    }
    json!({"lyrics": {"matchThreshold": 70, "enabledSources": enabled}})
}

fn service(dir: &Path, sources: Vec<Box<dyn LyricsSource>>, config: Value) -> Arc<LyricsService> {
    let config: Arc<dyn Fn() -> Value + Send + Sync> = Arc::new(move || config.clone());
    let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_: &str| {});
    Arc::new(LyricsService::with_sources(dir, config, log, sources))
}

fn track() -> Track {
    Track {
        title: "稻香".to_string(),
        artist: Some("周杰伦".to_string()),
        artists: None,
        album: Some("魔杰座".to_string()),
        album_artists: None,
        duration_ms: Some(223_000.0),
    }
}

fn candidate(id: &str, title: &str, artist: &str, seconds: f64) -> Candidate {
    Candidate {
        id: id.to_string(),
        title: title.to_string(),
        artist: Some(artist.to_string()),
        duration_sec: Some(seconds),
        ..Default::default()
    }
}

// --------------------------------------------------------------- fan-out ----

#[test]
fn every_enabled_source_contributes_and_rows_are_deduped() {
    let dir = temp_dir("fan-out");
    let counters = Arc::new(Counters::default());
    let alpha = FakeSource::new(
        "alpha",
        vec![candidate("a1", "稻香", "周杰伦", 223.0), candidate("a1", "稻香", "周杰伦", 223.0)],
        &counters,
    );
    let beta = FakeSource::new("beta", vec![candidate("b1", "稻香", "周杰伦", 223.5)], &counters);
    let service = service(
        &dir,
        vec![Box::new(alpha), Box::new(beta)],
        config(&[("alpha", true), ("beta", true)]),
    );

    let result = service.search_all(&NoTransport, &track(), SearchFlags::default());

    assert!(!result.cancelled);
    let sources: Vec<&str> = result.list.iter().map(|row| row.source.as_str()).collect();
    assert!(sources.contains(&"alpha"), "alpha answered: {sources:?}");
    assert!(sources.contains(&"beta"), "beta answered: {sources:?}");
    // The same `{source}\u{1}{id}` twice is one row.
    assert_eq!(result.list.iter().filter(|row| row.source == "alpha").count(), 1);
    assert!(result.list.iter().all(|row| row.match_info.is_some()));
    let state = service.search_state(&track_key(&track()));
    assert_eq!(state.phase, "ready");
    assert_eq!(state.completed.len(), 2, "both sources reported done");
}

#[test]
fn disabled_sources_are_skipped() {
    let dir = temp_dir("disabled");
    let counters = Arc::new(Counters::default());
    let alpha = FakeSource::new("alpha", vec![candidate("a1", "稻香", "周杰伦", 223.0)], &counters);
    let beta = FakeSource::new("beta", vec![candidate("b1", "稻香", "周杰伦", 223.0)], &counters);
    let service = service(
        &dir,
        vec![Box::new(alpha), Box::new(beta)],
        config(&[("alpha", true), ("beta", false)]),
    );

    let result = service.search_all(&NoTransport, &track(), SearchFlags::default());

    assert!(result.list.iter().all(|row| row.source != "beta"));
    let state = service.search_state(&track_key(&track()));
    assert_eq!(state.completed, vec!["alpha".to_string()]);
}

#[test]
fn a_broken_source_does_not_block_the_others() {
    let dir = temp_dir("one-broken");
    let counters = Arc::new(Counters::default());
    let broken = FakeSource::new("alpha", Vec::new(), &counters).search_error("connection reset by peer");
    let healthy = FakeSource::new("beta", vec![candidate("b1", "稻香", "周杰伦", 223.0)], &counters);
    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    let config_value = config(&[("alpha", true), ("beta", true)]);
    let config_fn: Arc<dyn Fn() -> Value + Send + Sync> = Arc::new(move || config_value.clone());
    let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
        sink.lock().unwrap().push(line.to_string());
    });
    let service = LyricsService::with_sources(
        &dir,
        config_fn,
        log,
        vec![Box::new(broken), Box::new(healthy)],
    );

    let result = service.search_all(&NoTransport, &track(), SearchFlags::default());

    assert!(result.list.iter().any(|row| row.source == "beta"));
    let logs = logs.lock().unwrap();
    assert!(
        logs.iter().any(|line| line.contains("alpha") && line.contains("搜索失败")),
        "the failure must leave a line: {logs:?}"
    );
}

// ---------------------------------------------------------------- probing ---

#[test]
fn a_definitive_failure_drops_the_row() {
    let dir = temp_dir("definitive");
    let counters = Arc::new(Counters::default());
    let dead = FakeSource::new("alpha", vec![candidate("a1", "稻香", "周杰伦", 223.0)], &counters)
        .lyric_error("未找到这首歌的歌词");
    let service = service(&dir, vec![Box::new(dead)], config(&[("alpha", true)]));

    let result = service.search_all(&NoTransport, &track(), SearchFlags::default());

    let row = &result.list[0];
    assert_eq!(row.probe_failed, Some(true), "a definitive failure removes the row");
    assert!(row.verified.is_none());
}

#[test]
fn a_soft_failure_keeps_the_row_unverified() {
    let dir = temp_dir("soft");
    let counters = Arc::new(Counters::default());
    let shaky = FakeSource::new("alpha", vec![candidate("a1", "稻香", "周杰伦", 223.0)], &counters)
        .lyric_error("connection reset by peer");
    let service = service(&dir, vec![Box::new(shaky)], config(&[("alpha", true)]));

    let result = service.search_all(&NoTransport, &track(), SearchFlags::default());

    let row = &result.list[0];
    assert_eq!(row.probe_failed, None, "a transport wobble is not a verdict");
    assert!(row.verified.is_none());
    assert!(!result.probed, "a soft failure means the round is not authoritative");
}

#[test]
fn a_timeout_marks_the_row_without_removing_it() {
    let dir = temp_dir("timeout");
    let counters = Arc::new(Counters::default());
    let slow = FakeSource::new("alpha", vec![candidate("a1", "稻香", "周杰伦", 223.0)], &counters)
        .lyric_error("探测超时");
    let service = service(&dir, vec![Box::new(slow)], config(&[("alpha", true)]));

    let result = service.search_all(&NoTransport, &track(), SearchFlags::default());

    let row = &result.list[0];
    assert_eq!(row.probe_timed_out, Some(true));
    assert_eq!(row.probe_failed, None);
    assert!(row.verified.is_none());
}

#[test]
fn a_verified_row_carries_the_probe_facts() {
    let dir = temp_dir("verified");
    let counters = Arc::new(Counters::default());
    let good = FakeSource::new("alpha", vec![candidate("a1", "稻香", "周杰伦", 223.0)], &counters);
    let service = service(&dir, vec![Box::new(good)], config(&[("alpha", true)]));

    let result = service.search_all(&NoTransport, &track(), SearchFlags::default());

    assert!(result.probed);
    let verified = result.list[0].verified.as_ref().expect("verified");
    assert_eq!(verified.timeline, Some(true));
    assert_eq!(verified.lines, Some(3));
    assert_eq!(verified.word, Some(false), "plain LRC is line timing");
}

// ---------------------------------------------------------- epoch fencing ---

#[test]
fn an_epoch_change_discards_the_in_flight_answer() {
    let dir = temp_dir("epoch");
    let counters = Arc::new(Counters::default());
    let slot: Arc<Mutex<Option<Weak<LyricsService>>>> = Arc::new(Mutex::new(None));
    let hook = {
        let slot = slot.clone();
        Arc::new(move || {
            if let Some(weak) = slot.lock().unwrap().as_ref() {
                if let Some(service) = weak.upgrade() {
                    // The song changed while this search was running.
                    service.invalidate();
                }
            }
        }) as Arc<dyn Fn() + Send + Sync>
    };
    let alpha = FakeSource::new("alpha", vec![candidate("a1", "稻香", "周杰伦", 223.0)], &counters)
        .before_search(hook);
    let service = service(&dir, vec![Box::new(alpha)], config(&[("alpha", true)]));
    *slot.lock().unwrap() = Some(Arc::downgrade(&service));

    let result = service.search_all(&NoTransport, &track(), SearchFlags::default());

    assert!(result.cancelled, "an answer for the previous generation must not land");
    assert!(result.list.is_empty());
}

// ------------------------------------------------------------------ cache ---

#[test]
fn a_cached_search_does_not_ask_the_sources_twice() {
    let dir = temp_dir("cache");
    let counters = Arc::new(Counters::default());
    let alpha = FakeSource::new("alpha", vec![candidate("a1", "稻香", "周杰伦", 223.0)], &counters);
    let service = service(&dir, vec![Box::new(alpha)], config(&[("alpha", true)]));

    let first = service.search_all(&NoTransport, &track(), SearchFlags::default());
    assert!(!first.cached);
    let calls = counters.searches.load(Ordering::SeqCst);

    let second = service.search_all(&NoTransport, &track(), SearchFlags::default());
    assert!(second.cached);
    assert_eq!(counters.searches.load(Ordering::SeqCst), calls, "no second fan-out");
    assert_eq!(second.list.len(), first.list.len());
}

#[test]
fn restart_re_asks_after_an_empty_answer() {
    let dir = temp_dir("restart");
    let counters = Arc::new(Counters::default());
    let empty = FakeSource::new("alpha", Vec::new(), &counters);
    let service = service(&dir, vec![Box::new(empty)], config(&[("alpha", true)]));

    service.search_all(&NoTransport, &track(), SearchFlags::default());
    let after_first = counters.searches.load(Ordering::SeqCst);

    // A refresh must not be swallowed by the 30 s empty-result cache.
    let refreshed = service.search_all(
        &NoTransport,
        &track(),
        SearchFlags { restart: true, ..Default::default() },
    );
    assert!(!refreshed.cached);
    assert!(counters.searches.load(Ordering::SeqCst) > after_first);

    let settled = counters.searches.load(Ordering::SeqCst);
    let again = service.search_all(&NoTransport, &track(), SearchFlags::default());
    assert!(again.cached);
    assert_eq!(counters.searches.load(Ordering::SeqCst), settled);
}

// ----------------------------------------------------------- source memory --

#[test]
fn a_manual_pick_is_written_and_reloaded() {
    let dir = temp_dir("memory-manual");
    let counters = Arc::new(Counters::default());
    let alpha = FakeSource::new("alpha", Vec::new(), &counters);
    let svc = service(&dir, vec![Box::new(alpha)], config(&[("alpha", true)]));
    let picked = candidate("k1", "稻香", "周杰伦", 223.0);

    let chosen = svc.use_candidate(&NoTransport, &track(), "alpha", &picked, svc.epoch(), false, true);
    assert!(chosen.ok, "{}", chosen.error);

    let reloaded = service(&dir, Vec::new(), config(&[]));
    assert_eq!(reloaded.init(), 1, "the manual pick is on disk");
    let (source, remembered) = reloaded.remembered_source(&track()).expect("remembered");
    assert_eq!(source, "alpha");
    assert_eq!(remembered.expect("candidate").id, "k1");
}

#[test]
fn an_automatic_pick_stays_in_this_session() {
    let dir = temp_dir("memory-auto");
    let counters = Arc::new(Counters::default());
    let alpha = FakeSource::new("alpha", Vec::new(), &counters);
    let svc = service(&dir, vec![Box::new(alpha)], config(&[("alpha", true)]));
    let picked = candidate("k1", "稻香", "周杰伦", 223.0);

    let chosen = svc.use_candidate(&NoTransport, &track(), "alpha", &picked, svc.epoch(), true, true);
    assert!(chosen.ok, "{}", chosen.error);
    assert!(svc.remembered_source(&track()).is_some(), "visible this session");

    let reloaded = service(&dir, Vec::new(), config(&[]));
    assert_eq!(reloaded.init(), 0, "an automatic pick must not stick across restarts");
    assert!(reloaded.remembered_source(&track()).is_none());

    assert!(svc.promote_source_memory(&track(), "alpha", "k1"));
    let reloaded_promoted = service(&dir, Vec::new(), config(&[]));
    assert_eq!(reloaded_promoted.init(), 1, "promoted pick must persist");
    let (source, cand) = reloaded_promoted
        .remembered_manual_source(&track())
        .expect("promoted pick restored");
    assert_eq!(source, "alpha");
    assert_eq!(cand.expect("candidate").id, "k1");
}

#[test]
fn a_stale_epoch_refuses_to_write() {
    let dir = temp_dir("memory-stale");
    let counters = Arc::new(Counters::default());
    let alpha = FakeSource::new("alpha", Vec::new(), &counters);
    let service = service(&dir, vec![Box::new(alpha)], config(&[("alpha", true)]));
    let picked = candidate("k1", "稻香", "周杰伦", 223.0);
    let epoch = service.epoch();

    service.invalidate();
    let chosen = service.use_candidate(&NoTransport, &track(), "alpha", &picked, epoch, false, true);

    assert!(chosen.stale, "a click from the previous song must not land");
    assert!(!chosen.ok);
    assert!(service.remembered_source(&track()).is_none());
}

#[test]
fn rescore_follows_the_latest_metadata() {
    let dir = temp_dir("rescore");
    let service = service(&dir, Vec::new(), config(&[]));
    let mut rows = vec![candidate("x", "稻香", "周杰伦", 223.0)];

    service.rescore(&track(), &mut rows);
    let matched = rows[0].percent.expect("percent");

    let mut other = track();
    other.title = "完全不同的另一首歌".to_string();
    other.artist = Some("Someone Else".to_string());
    other.album = None;
    other.duration_ms = Some(10_000.0);
    service.rescore(&other, &mut rows);
    let mismatched = rows[0].percent.expect("percent");

    assert!(matched >= 90.0, "a clean metadata match scores high: {matched}");
    assert!(mismatched < matched, "{mismatched} must fall below {matched}");
}

#[test]
fn source_searches_overlap_instead_of_waiting_for_previous_source() {
 let dir=temp_dir("concurrent-search");
 let counters=Arc::new(Counters::default());
 let active=Arc::new(AtomicUsize::new(0));
 let peak=Arc::new(AtomicUsize::new(0));
 let hook:Arc<dyn Fn()+Send+Sync>={let active=active.clone();let peak=peak.clone();Arc::new(move||{
  let n=active.fetch_add(1,Ordering::SeqCst)+1;peak.fetch_max(n,Ordering::SeqCst);
  std::thread::sleep(std::time::Duration::from_millis(80));active.fetch_sub(1,Ordering::SeqCst);
 })};
 let a=FakeSource::new("alpha",vec![candidate("a","稻香","周杰伦",223.)],&counters).before_search(hook.clone());
 let b=FakeSource::new("beta",vec![candidate("b","稻香","周杰伦",223.)],&counters).before_search(hook);
 let svc=service(&dir,vec![Box::new(a),Box::new(b)],config(&[("alpha",true),("beta",true)]));
 let result=svc.search_all(&NoTransport,&track(),SearchFlags::default());
 assert_eq!(result.list.len(),2); assert_eq!(peak.load(Ordering::SeqCst),2,"slow sources must not serialize all other searches");
}

#[test]
fn verified_fast_source_is_delivered_before_slow_search_finishes() {
 let dir=temp_dir("progress");let counters=Arc::new(Counters::default());
 let finished=Arc::new(std::sync::atomic::AtomicBool::new(false));
 let hook:Arc<dyn Fn()+Send+Sync>={let finished=finished.clone();Arc::new(move||{std::thread::sleep(std::time::Duration::from_millis(180));finished.store(true,Ordering::SeqCst);})};
 let fast=FakeSource::new("alpha",vec![candidate("a","稻香","周杰伦",223.)],&counters);
 let slow=FakeSource::new("beta",vec![candidate("b","稻香","周杰伦",223.)],&counters).before_search(hook);
 let svc=service(&dir,vec![Box::new(fast),Box::new(slow)],config(&[("alpha",true),("beta",true)]));
 let mut early=false;
 let result=svc.search_all_with_progress(&NoTransport,&track(),SearchFlags::default(),svc.epoch(),&mut |rows|{
  if rows.iter().any(|c|c.source=="alpha" && c.verified.is_some()) && !finished.load(Ordering::SeqCst){early=true;}
 });
 assert!(early,"usable lyrics should reach playback while slow sources are still working");assert_eq!(result.list.len(),2);
}

#[test]
fn each_source_probes_its_best_identity_match_before_provider_order() {
 let dir=temp_dir("probe-order");let counters=Arc::new(Counters::default());
 let mut ordinary=candidate("ordinary","稻香","周杰伦",223.);ordinary.album=Some("Other album".into());
 let mut exact=candidate("exact","稻香","周杰伦",223.);exact.album=Some("魔杰座".into());
 let t=track();assert!(oly_engine::matching::compare_track(&t,&exact).score>oly_engine::matching::compare_track(&t,&ordinary).score);
 let src=FakeSource::new("alpha",vec![ordinary,exact],&counters);
 let svc=service(&dir,vec![Box::new(src)],config(&[("alpha",true)]));
 let found=svc.search_all(&NoTransport,&t,SearchFlags::default());
 assert_eq!(found.list.iter().find(|c|c.verified.is_some()).unwrap().id,"exact");
}
