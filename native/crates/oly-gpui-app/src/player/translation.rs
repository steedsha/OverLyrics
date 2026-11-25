//! Bounded background smart translation. Results only patch missing translations;
//! provider text, word timing and the current lyric animation remain untouched.
use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::{Arc, Condvar, Mutex},
    thread,
};

use oly_contracts::{
    config::{TranslationConfig, is_translation_enabled, translation_settings},
    lyrics::LyricDoc,
};
use oly_engine::{
    net::{HttpError, HttpRequest, HttpResponse, Transport},
    translate::needs_translation,
    translate_net::{self, Engine, TranslateOptions},
};
use oly_platform_win::http::NativeHttp;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationPatch {
    pub line_index: usize,
    /// Guard against applying a result to a different version's row.
    pub original_text: String,
    pub translation: String,
}

#[derive(Clone, Debug)]
pub struct TranslationResult {
    pub generation: u64,
    pub identity: String,
    pub epoch: u64,
    pub patches: Vec<TranslationPatch>,
}

pub fn translation_enabled(config: &Value, preference_key: &str) -> bool {
    is_translation_enabled(config, preference_key)
}

/// Use the existing curated AI resolver: it validates model/endpoint and only
/// forwards the key to the provider to which it belongs.
pub fn translate_options(config: &Value, doc: &LyricDoc) -> TranslateOptions {
    let lyrics = config.get("lyrics").unwrap_or(&Value::Null);
    let string = |key: &str| lyrics.get(key).and_then(Value::as_str).map(str::to_owned);
    let settings = translation_settings(&TranslationConfig {
        ai_preset: string("aiPreset"),
        ai_model: string("aiModel"),
        ai_key: string("aiKey"),
        translate_target: string("translateTarget"),
        ai_base_url: string("aiBaseUrl"),
        libre_url: string("libreUrl"),
    });
    let engine = Engine::parse(lyrics.get("translateEngine").and_then(Value::as_str).unwrap_or("free"));
    let libre_url = string("libreUrl").filter(|url| translate_net::libre_url_ok(url));
    TranslateOptions {
        engine,
        target: settings.target.to_string(),
        budget_ms: if engine == Engine::Free { translate_net::FREE_BUDGET_MS } else { translate_net::AI_BUDGET_MS },
        libre_url,
        ai: (engine != Engine::Free).then_some(settings.ai),
        ai_fallback: lyrics.get("aiFallback").and_then(Value::as_bool) != Some(false),
        song: json!({"title": doc.title, "artist": doc.artist, "artists": doc.artists, "album": doc.album}),
    }
}

/// Only foreign rows with no provider-supplied translation are eligible.
pub fn missing_translation_lines(doc: &LyricDoc) -> Vec<(usize, String)> {
    if doc.instrumental { return Vec::new(); }
    doc.lines.iter().enumerate()
        .filter(|(_, line)| line.trans.trim().is_empty() && needs_translation(&line.text))
        .map(|(index, line)| (index, line.text.clone()))
        .collect()
}

/// Safe to apply directly to the installed document; no full document install
/// or clock/animation reset is needed. Returns the number of changed rows.
pub fn apply_patches(doc: &mut LyricDoc, patches: &[TranslationPatch]) -> usize {
    let mut changed = 0;
    for patch in patches {
        if let Some(line) = doc.lines.get_mut(patch.line_index) {
            if line.text == patch.original_text && line.trans.trim().is_empty() && !patch.translation.trim().is_empty() {
                line.trans = patch.translation.clone();
                changed += 1;
            }
        }
    }
    changed
}

struct Job {
    generation: u64,
    identity: String,
    epoch: u64,
    cache_key: u64,
    rows: Vec<(usize, String)>,
    options: TranslateOptions,
}

#[derive(Default)]
struct State {
    epoch: u64,
    /// Includes generation, document identity, text and configuration. This also
    /// suppresses duplicate requests while the same result is being displayed.
    desired: Option<u64>,
    pending: Option<Job>,
    result: Option<TranslationResult>,
    stopped: bool,
}

impl State {
    fn invalidate(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending = None;
        self.result = None;
    }
    fn accepts(&self, epoch: u64) -> bool { !self.stopped && self.epoch == epoch }
    fn publish(&mut self, result: TranslationResult) {
        if self.accepts(result.epoch) { self.result = Some(result); }
    }
}

type Shared = Arc<(Mutex<State>, Condvar)>;

/// One active request and one replaceable pending request. Calling `request`
/// every UI poll is safe: identical input does not restart network work.
pub struct TranslationWorker {
    shared: Shared,
}

impl Default for TranslationWorker {
    fn default() -> Self { Self::new() }
}

impl TranslationWorker {
    pub fn new() -> Self {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let worker = Arc::clone(&shared);
        thread::Builder::new().name("lyric-translation".into()).spawn(move || run(worker))
            .expect("translation worker thread");
        Self { shared }
    }

    /// `identity` must identify the installed source/version. `preference_key`
    /// is config::source_translation_key(track_key, source), which intentionally
    /// differs from the source/version identity. Calling this after any config
    /// edit also fences results that were started with the old settings.
    pub fn request(&self, generation: u64, identity: String, preference_key: String, doc: LyricDoc, config: Value) {
        let rows = missing_translation_lines(&doc);
        let options = translate_options(&config, &doc);
        let cache_key = translation_cache_key(&rows, &options);
        let mut hasher = DefaultHasher::new();
        generation.hash(&mut hasher);
        identity.hash(&mut hasher);
        preference_key.hash(&mut hasher);
        cache_key.hash(&mut hasher);
        // Include preference/config changes even when their resolved engine is
        // unchanged: disable + re-enable must never revive an older result.
        config.to_string().hash(&mut hasher);
        let desired = hasher.finish();
        let enabled = translation_enabled(&config, &preference_key);
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        if state.desired == Some(desired) { return; }
        state.invalidate();
        state.desired = Some(desired);
        if enabled && !rows.is_empty() {
            state.pending = Some(Job { generation, identity, epoch: state.epoch, cache_key, rows, options });
        }
        wake.notify_one();
    }

    /// Call immediately when translation is disabled or the track/version is
    /// replaced, before polling results. In-flight HTTP cannot be aborted by
    /// NativeHttp; the transport fences every subsequent request and result.
    pub fn cancel(&self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.invalidate();
        state.desired = None;
        wake.notify_one();
    }

    pub fn take(&self) -> Option<TranslationResult> {
        let mut state = self.shared.0.lock().unwrap();
        let result = state.result.take()?;
        state.accepts(result.epoch).then_some(result)
    }
}

impl Drop for TranslationWorker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.invalidate();
        state.stopped = true;
        wake.notify_one();
        // Do not join a blocking HTTP request from the UI thread.
    }
}

/// Cache only translation input and effective engine options, never identity or
/// preference toggles. Keys are opaque hashes; API keys are never logged.
fn translation_cache_key(rows: &[(usize, String)], options: &TranslateOptions) -> u64 {
    let mut hasher = DefaultHasher::new();
    rows.hash(&mut hasher);
    format!("{:?}", options.engine).hash(&mut hasher);
    options.target.hash(&mut hasher);
    options.libre_url.hash(&mut hasher);
    options.ai_fallback.hash(&mut hasher);
    options.song.to_string().hash(&mut hasher);
    if let Some(ai) = options.ai.as_ref() {
        ai.base_url.hash(&mut hasher);
        ai.model.hash(&mut hasher);
        ai.api_key.hash(&mut hasher);
        ai.request.to_string().hash(&mut hasher);
    }
    hasher.finish()
}

struct FencedTransport { shared: Shared, epoch: u64 }
impl Transport for FencedTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        if !self.shared.0.lock().unwrap().accepts(self.epoch) {
            return Err(HttpError::Transport("translation cancelled".into()));
        }
        let response = NativeHttp.send(request);
        if !self.shared.0.lock().unwrap().accepts(self.epoch) {
            return Err(HttpError::Transport("translation cancelled".into()));
        }
        response
    }
}

fn run(shared: Shared) {
    let mut cache: HashMap<u64, Vec<String>> = HashMap::new();
    loop {
        let job = {
            let (lock, wake) = &*shared;
            let mut state = lock.lock().unwrap();
            while !state.stopped && state.pending.is_none() { state = wake.wait(state).unwrap(); }
            if state.stopped { return; }
            state.pending.take().unwrap()
        };
        if !shared.0.lock().unwrap().accepts(job.epoch) { continue; }
        let translated = if let Some(cached) = cache.get(&job.cache_key) {
            cached.clone()
        } else {
            let texts = job.rows.iter().map(|(_, text)| text.clone()).collect::<Vec<_>>();
            let transport = FencedTransport { shared: Arc::clone(&shared), epoch: job.epoch };
            let outcome = translate_net::translate_lines(&transport, &texts, &job.options);
            if !shared.0.lock().unwrap().accepts(job.epoch) { continue; }
            // Cache only useful responses. Failed jobs remain deduplicated until
            // inputs change or cancel() explicitly requests a retry.
            if outcome.lines.iter().any(|line| !line.trim().is_empty()) {
                if cache.len() >= 32 { cache.clear(); }
                cache.insert(job.cache_key, outcome.lines.clone());
            }
            outcome.lines
        };
        let patches = job.rows.into_iter().zip(translated).filter_map(|((line_index, original_text), translation)| {
            (!translation.trim().is_empty()).then_some(TranslationPatch { line_index, original_text, translation })
        }).collect();
        shared.0.lock().unwrap().publish(TranslationResult {
            generation: job.generation, identity: job.identity, epoch: job.epoch, patches,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oly_contracts::{config::source_translation_key, lyrics::{LyricLine, Word}};

    fn document() -> LyricDoc {
        LyricDoc { title: "Song".into(), lines: vec![
            LyricLine { text: "Hello world".into(), trans: "已有译文".into(), ..Default::default() },
            LyricLine { text: "The night is young".into(), start: Some(42), words: vec![Word { text: "The".into(), start: Some(42), end: Some(99) }], ..Default::default() },
            LyricLine { text: "晴天".into(), ..Default::default() },
            LyricLine { text: "♪".into(), ..Default::default() },
        ], ..Default::default() }
    }

    #[test]
    fn missing_foreign_rows_preserve_provider_translation_and_word_timing() {
        let mut doc = document();
        assert_eq!(missing_translation_lines(&doc), vec![(1, "The night is young".into())]);
        let words = doc.lines[1].words.clone();
        let patches = vec![
            TranslationPatch { line_index: 0, original_text: doc.lines[0].text.clone(), translation: "replace".into() },
            TranslationPatch { line_index: 1, original_text: doc.lines[1].text.clone(), translation: "夜正年轻".into() },
        ];
        assert_eq!(apply_patches(&mut doc, &patches), 1);
        assert_eq!(doc.lines[0].trans, "已有译文");
        assert_eq!(doc.lines[1].start, Some(42));
        assert_eq!(doc.lines[1].words, words);
        doc.lines[1].trans.clear();
        doc.lines[1].text = "Different version".into();
        assert_eq!(apply_patches(&mut doc, &patches), 0);
    }

    #[test]
    fn preferences_use_original_song_source_key_and_literal_boolean() {
        let key = source_translation_key("track-key", " QQ ");
        let mut config = json!({"lyrics":{"autoTranslate":false,"translationOverrides":{}}});
        config["lyrics"]["translationOverrides"][&key] = json!(true);
        assert!(translation_enabled(&config, &key));
        config["lyrics"]["translationOverrides"][&key] = json!(false);
        config["lyrics"]["autoTranslate"] = json!(true);
        assert!(!translation_enabled(&config, &key));
        config["lyrics"]["translationOverrides"][&key] = json!("false");
        assert!(translation_enabled(&config, &key));
    }

    #[test]
    fn options_resolve_ai_target_libre_and_fallback() {
        let config = json!({"lyrics":{"translateEngine":"ai-first","translateTarget":"ja","aiPreset":"deepseek","aiKey":"secret","libreUrl":"https://libre.example","aiFallback":false}});
        let options = translate_options(&config, &document());
        assert_eq!(options.engine, Engine::AiFirst);
        assert_eq!(options.target, "ja");
        assert_eq!(options.ai.as_ref().unwrap().api_key, "secret");
        assert_eq!(options.libre_url.as_deref(), Some("https://libre.example"));
        assert!(!options.ai_fallback);
        let free = translate_options(&json!({"lyrics":{"libreUrl":"file:///tmp"}}), &document());
        assert_eq!(free.engine, Engine::Free);
        assert!(free.ai.is_none());
        assert!(free.libre_url.is_none());
    }

    #[test]
    fn config_and_document_changes_produce_different_cache_keys() {
        let doc = document();
        let rows = missing_translation_lines(&doc);
        let a = translate_options(&json!({}), &doc);
        let b = translate_options(&json!({"lyrics":{"translateTarget":"ja"}}), &doc);
        assert_ne!(translation_cache_key(&rows, &a), translation_cache_key(&rows, &b));
        assert_ne!(translation_cache_key(&rows, &a), translation_cache_key(&[(1, "Another line".into())], &a));
    }

    #[test]
    fn latest_epoch_rejects_old_generation_identity_and_disable_results() {
        let mut state = State::default();
        let stale = TranslationResult { generation: 1, identity: "qq:old".into(), epoch: state.epoch, patches: vec![] };
        state.invalidate(); // New generation, source/version, config edit, or disable.
        state.publish(stale);
        assert!(state.result.is_none());
        let latest = TranslationResult { generation: 2, identity: "netease:new".into(), epoch: state.epoch, patches: vec![] };
        state.publish(latest);
        assert_eq!(state.result.as_ref().unwrap().identity, "netease:new");
        state.invalidate();
        assert!(state.result.is_none());
        assert!(!state.accepts(state.epoch.wrapping_sub(1)));
        state.stopped = true;
        assert!(!state.accepts(state.epoch));
    }
}
