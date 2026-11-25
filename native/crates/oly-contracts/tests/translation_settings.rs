//! Task 11 — translation settings contract.
//!
//! `src/shared/translation-settings.ts` is the contract: the curated provider
//! list, the model whitelist per provider, and the rule that a stored API key is
//! only forwarded to the official endpoint it belongs to. Getting this wrong
//! leaks a credential to whatever host a hand-edited `aiBaseUrl` names.

use oly_contracts::config::{
    AI_PRESETS, TranslationConfig, TRANSLATION_LANGUAGES, ai_preset, select_ai_provider,
    translation_settings,
};
use serde_json::json;

fn config(pairs: &[(&str, &str)]) -> TranslationConfig {
    let mut c = TranslationConfig::default();
    for (key, value) in pairs {
        match *key {
            "aiPreset" => c.ai_preset = Some((*value).into()),
            "aiModel" => c.ai_model = Some((*value).into()),
            "aiKey" => c.ai_key = Some((*value).into()),
            "translateTarget" => c.translate_target = Some((*value).into()),
            "aiBaseUrl" => c.ai_base_url = Some((*value).into()),
            "libreUrl" => c.libre_url = Some((*value).into()),
            other => panic!("unknown key {other}"),
        }
    }
    c
}

#[test]
fn every_shipped_provider_is_present_with_its_endpoint() {
    let keys: Vec<&str> = AI_PRESETS.iter().map(|p| p.provider).collect();
    for expected in [
        "deepseek",
        "zhipu",
        "siliconflow",
        "dashscope",
        "moonshot",
        "openrouter",
        "openai",
    ] {
        assert!(keys.contains(&expected), "missing provider {expected}");
    }
    let deepseek = ai_preset("deepseek").expect("deepseek preset");
    assert_eq!(deepseek.base_url, "https://api.deepseek.com/v1");
    assert_eq!(deepseek.default_model, "deepseek-flash");
    assert_eq!(deepseek.models.len(), 2);
    assert_eq!(serde_json::from_str::<serde_json::Value>(deepseek.request).unwrap(), json!({"thinking": {"type": "disabled"}}));

    let languages: Vec<&str> = TRANSLATION_LANGUAGES.iter().map(|(value, _)| *value).collect();
    assert_eq!(languages, ["zh-CHS", "zh-CHT", "en", "ja", "ko"]);
}

#[test]
fn defaults_land_on_deepseek_and_simplified_chinese() {
    let settings = translation_settings(&TranslationConfig::default());
    assert_eq!(settings.provider, "deepseek");
    assert_eq!(settings.target, "zh-CHS");
    assert_eq!(settings.libre_url, "");
    assert_eq!(settings.ai.base_url, "https://api.deepseek.com/v1");
    assert_eq!(settings.ai.model, "deepseek-flash");
    assert_eq!(settings.ai.api_key, "");
    assert_eq!(settings.ai.request, json!({"thinking": {"type": "disabled"}}));
}

#[test]
fn a_key_is_only_forwarded_to_its_own_official_endpoint() {
    // Same provider, default endpoint: the stored key is forwarded.
    let kept = translation_settings(&config(&[
        ("aiPreset", "zhipu"),
        ("aiKey", "sk-live-123"),
    ]));
    assert_eq!(kept.provider, "zhipu");
    assert_eq!(kept.ai.api_key, "sk-live-123");

    // Trailing slashes and surrounding whitespace are normalised before the
    // comparison, so a cosmetic edit does not silently drop the key.
    let normalized = translation_settings(&config(&[
        ("aiPreset", "zhipu"),
        ("aiKey", "sk-live-123"),
        ("aiBaseUrl", "  https://open.bigmodel.cn/api/paas/v4///  "),
    ]));
    assert_eq!(normalized.ai.api_key, "sk-live-123");

    // A custom endpoint keeps the credential on disk but never forwards it.
    let custom = translation_settings(&config(&[
        ("aiPreset", "zhipu"),
        ("aiKey", "sk-live-123"),
        ("aiBaseUrl", "https://proxy.example.com/v1"),
    ]));
    assert_eq!(custom.provider, "zhipu");
    assert_eq!(custom.ai.base_url, "https://open.bigmodel.cn/api/paas/v4");
    assert_eq!(custom.ai.api_key, "");

    // An unknown preset is not proof that the key belongs to deepseek either.
    let unknown = translation_settings(&config(&[
        ("aiPreset", "some-local-llm"),
        ("aiKey", "sk-live-123"),
        ("aiBaseUrl", "https://api.deepseek.com/v1"),
    ]));
    assert_eq!(unknown.provider, "deepseek");
    assert_eq!(unknown.ai.api_key, "");
}

#[test]
fn a_whitespace_only_base_url_does_not_count_as_matching() {
    // `'   '.trim()` is '' which is not the provider endpoint, so the key is
    // dropped; a falsy/absent value is the only thing that means "no override".
    let blank = translation_settings(&config(&[
        ("aiPreset", "deepseek"),
        ("aiKey", "sk-live-123"),
        ("aiBaseUrl", "   "),
    ]));
    assert_eq!(blank.ai.api_key, "");

    // An absent override means "no override" — but only a known preset can
    // forward a key at all, which is the same `known` gate as above.
    let absent = translation_settings(&config(&[
        ("aiPreset", "deepseek"),
        ("aiKey", "sk-live-123"),
    ]));
    assert_eq!(absent.ai.api_key, "sk-live-123");

    // Without a preset the engine cannot know whose key this is.
    let orphan = translation_settings(&config(&[("aiKey", "sk-live-123")]));
    assert_eq!(orphan.ai.api_key, "");
}

#[test]
fn only_whitelisted_models_and_targets_survive() {
    let settings = translation_settings(&config(&[
        ("aiPreset", "moonshot"),
        ("aiModel", "kimi-k3"),
        ("translateTarget", "ja"),
    ]));
    assert_eq!(settings.ai.model, "kimi-k3");
    assert_eq!(settings.target, "ja");

    // A model that the provider list no longer ships falls back to its default.
    let stale = translation_settings(&config(&[
        ("aiPreset", "moonshot"),
        ("aiModel", "kimi-k2-retired"),
    ]));
    assert_eq!(stale.ai.model, "kimi-k3.1-pro");

    // An unknown target falls back to simplified Chinese.
    let bogus = translation_settings(&config(&[("translateTarget", "klingon")]));
    assert_eq!(bogus.target, "zh-CHS");

    // libreUrl is deliberately blank in this build.
    let libre = translation_settings(&config(&[("libreUrl", "https://libre.example.com")]));
    assert_eq!(libre.libre_url, "");
}

#[test]
fn selecting_a_provider_resets_the_stored_key() {
    let selection = select_ai_provider("moonshot").expect("known provider");
    assert_eq!(selection.ai_preset, "moonshot");
    assert_eq!(selection.ai_base_url, "https://api.moonshot.cn/v1");
    assert_eq!(selection.ai_model, "kimi-k3.1-pro");
    assert_eq!(selection.ai_key, "");
    assert!(select_ai_provider("nope").is_none());
}

#[test]
fn per_source_translation_overrides_beat_the_global_switch() {
    use oly_contracts::config::{is_translation_enabled, source_translation_key};

    let key = source_translation_key("Midnights\u{1}Taylor Swift", "Kugou");
    assert_eq!(key, "[\"Midnights\\u0001Taylor Swift\",\"kugou\"]");

    // `json!` treats a bare expression as a literal key, so the dynamic key has
    // to go through `json!({})` + insert. The function takes the whole config
    // document, not the `lyrics` section on its own.
    let mut on = serde_json::Map::new();
    on.insert(key.clone(), serde_json::Value::Bool(true));
    let lyrics = json!({ "lyrics": { "autoTranslate": false, "translationOverrides": on } });
    assert!(is_translation_enabled(&lyrics, &key));

    let mut off_map = serde_json::Map::new();
    off_map.insert(key.clone(), serde_json::Value::Bool(false));
    let off = json!({ "lyrics": { "autoTranslate": true, "translationOverrides": off_map } });
    assert!(!is_translation_enabled(&off, &key));

    // A non-boolean override is not a decision and must not shadow the default.
    let mut odd = serde_json::Map::new();
    odd.insert(key.clone(), serde_json::Value::String("yes".into()));
    let ignored = json!({ "lyrics": { "autoTranslate": true, "translationOverrides": odd } });
    assert!(is_translation_enabled(&ignored, &key));

    // No override: the global switch decides.
    let global = json!({ "lyrics": { "autoTranslate": true, "translationOverrides": {} } });
    assert!(is_translation_enabled(&global, &key));
    assert!(!is_translation_enabled(&json!({ "lyrics": { "autoTranslate": false } }), &key));
    // A missing config section means "off", never a panic.
    assert!(!is_translation_enabled(&json!({}), &key));
}

#[test]
fn per_candidate_translation_key_isolates_versions_for_same_song_and_source() {
    use oly_contracts::config::{candidate_translation_key, is_translation_enabled};

    let k1 = candidate_translation_key("Midnights\u{1}Taylor Swift", "NetEase", "1001");
    let k2 = candidate_translation_key("Midnights\u{1}Taylor Swift", "NetEase", "1002");
    assert_ne!(k1, k2);

    let mut overrides = serde_json::Map::new();
    overrides.insert(k1.clone(), serde_json::Value::Bool(true));
    let cfg = json!({ "lyrics": { "autoTranslate": false, "translationOverrides": overrides } });
    assert!(is_translation_enabled(&cfg, &k1));
    assert!(!is_translation_enabled(&cfg, &k2));
}
