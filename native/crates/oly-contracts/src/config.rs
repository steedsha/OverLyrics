//! Translation settings and per-source translation preference.
//!
//! Ported 1:1 from `src/shared/translation-settings.ts` and
//! `src/shared/translation-preference.ts`. The curated provider table is a
//! contract: the settings UI lists exactly these endpoints and models, and a
//! stored API key is only ever forwarded to the official endpoint it belongs to.
//! A hand-edited `aiBaseUrl` pointing somewhere else must not receive it.

use serde_json::Value;

/// One selectable model inside a provider preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiModel {
    pub value: &'static str,
    pub label: &'static str,
}

/// One curated provider: endpoint, whitelisted models, extra request fields.
#[derive(Clone, Copy, Debug)]
pub struct AiPreset {
    /// Key in the config file (`lyrics.aiPreset`).
    pub provider: &'static str,
    /// Display name in the settings dropdown.
    pub name: &'static str,
    pub base_url: &'static str,
    pub models: &'static [AiModel],
    /// `models[0].value` — what an unknown or stale model falls back to.
    pub default_model: &'static str,
    /// Extra JSON merged into the chat-completions body. Kept as a JSON string
    /// so the table stays a plain `const` and the exact bytes are testable.
    pub request: &'static str,
}

/// Curated official endpoints and models, in dropdown order.
pub const AI_PRESETS: &[AiPreset] = &[
    AiPreset {
        provider: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        models: &[
            AiModel {
                value: "deepseek-flash",
                label: "DeepSeek Flash",
            },
            AiModel {
                value: "deepseek-v4-pro",
                label: "DeepSeek V4 Pro",
            },
        ],
        default_model: "deepseek-flash",
        request: r#"{"thinking":{"type":"disabled"}}"#,
    },
    AiPreset {
        provider: "zhipu",
        name: "智谱 GLM",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        models: &[AiModel {
            value: "glm-4.7-flash",
            label: "GLM-4.7-Flash",
        }],
        default_model: "glm-4.7-flash",
        request: r#"{"thinking":{"type":"disabled"}}"#,
    },
    AiPreset {
        provider: "siliconflow",
        name: "硅基流动",
        base_url: "https://api.siliconflow.cn/v1",
        models: &[
            AiModel {
                value: "deepseek-ai/DeepSeek-V3.2",
                label: "DeepSeek V3.2",
            },
            AiModel {
                value: "Qwen/Qwen3.6-27B",
                label: "Qwen3.6 27B",
            },
        ],
        default_model: "deepseek-ai/DeepSeek-V3.2",
        request: r#"{"enable_thinking":false}"#,
    },
    AiPreset {
        provider: "dashscope",
        name: "通义千问",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        models: &[
            AiModel {
                value: "qwen3.8-flash",
                label: "Qwen3.8 Flash",
            },
            AiModel {
                value: "qwen3.7-plus",
                label: "Qwen3.7 Plus",
            },
        ],
        default_model: "qwen3.8-flash",
        request: r#"{"enable_thinking":false}"#,
    },
    AiPreset {
        provider: "moonshot",
        name: "Kimi",
        base_url: "https://api.moonshot.cn/v1",
        models: &[
            AiModel {
                value: "kimi-k3.1-pro",
                label: "Kimi K3.1 Pro",
            },
            AiModel {
                value: "kimi-k3",
                label: "Kimi K3",
            },
        ],
        default_model: "kimi-k3.1-pro",
        request: r#"{"temperature":1}"#,
    },
    AiPreset {
        provider: "openrouter",
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        models: &[AiModel {
            value: "deepseek/deepseek-v4.1-flash",
            label: "DeepSeek V4.1 Flash",
        }],
        default_model: "deepseek/deepseek-v4.1-flash",
        request: r#"{"reasoning":{"effort":"none"}}"#,
    },
    AiPreset {
        provider: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        models: &[AiModel {
            value: "gpt-4o-mini",
            label: "GPT-4o mini",
        }],
        default_model: "gpt-4o-mini",
        request: "{}",
    },
];

/// Translation target languages, in dropdown order.
pub const TRANSLATION_LANGUAGES: &[(&str, &str)] = &[
    ("zh-CHS", "简体中文"),
    ("zh-CHT", "繁体中文"),
    ("en", "英语"),
    ("ja", "日语"),
    ("ko", "韩语"),
];

/// Look one preset up by its config key.
pub fn ai_preset(provider: &str) -> Option<&'static AiPreset> {
    AI_PRESETS.iter().find(|p| p.provider == provider)
}

/// The `lyrics` fields `translationSettings` reads.
#[derive(Clone, Debug, Default)]
pub struct TranslationConfig {
    pub ai_preset: Option<String>,
    pub ai_model: Option<String>,
    pub ai_key: Option<String>,
    pub translate_target: Option<String>,
    pub ai_base_url: Option<String>,
    pub libre_url: Option<String>,
}

/// The resolved request shape the engine hand to a provider client.
#[derive(Clone, Debug, PartialEq)]
pub struct AiRequest {
    pub base_url: &'static str,
    pub model: &'static str,
    pub api_key: String,
    /// Extra body fields, already parsed from the preset.
    pub request: Value,
}

/// What the settings page renders and the request path uses.
#[derive(Clone, Debug, PartialEq)]
pub struct TranslationSettings {
    pub provider: &'static str,
    pub target: &'static str,
    /// Deliberately blank in this build: the LibreTranslate path was retired.
    pub libre_url: &'static str,
    pub ai: AiRequest,
}

/// The config patch written when the user picks a provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSelection {
    pub ai_preset: &'static str,
    pub ai_base_url: &'static str,
    pub ai_model: &'static str,
    pub ai_key: String,
}

/// Normalise an endpoint for comparison: trim, then drop trailing slashes.
///
/// Only the comparison is normalised; the preset endpoint is what gets used.
fn canonical_endpoint(text: &str) -> &str {
    text.trim().trim_end_matches('/')
}

/// Resolve the curated provider, model, key and request extras.
///
/// A stored key survives on disk even when it is not forwarded: switching back
/// to the official endpoint must not require re-entering it.
pub fn translation_settings(config: &TranslationConfig) -> TranslationSettings {
    let known = config
        .ai_preset
        .as_deref()
        .and_then(ai_preset)
        .filter(|_| config.ai_preset.is_some());
    let preset = known.unwrap_or_else(|| ai_preset("deepseek").expect("deepseek preset"));

    // `!config.aiBaseUrl` in the reference: absent or empty means "no override".
    let endpoint_matches = match config.ai_base_url.as_deref() {
        None => true,
        Some(text) if text.is_empty() => true,
        Some(text) => canonical_endpoint(text) == preset.base_url,
    };
    let key_matches = known.is_some() && endpoint_matches;

    let target = config
        .translate_target
        .as_deref()
        .and_then(|want| {
            TRANSLATION_LANGUAGES
                .iter()
                .find(|(value, _)| *value == want)
                .map(|(value, _)| *value)
        })
        .unwrap_or("zh-CHS");

    // Map through the preset so the result borrows from the `'static` table
    // rather than from the caller's config document.
    let model: &'static str = config
        .ai_model
        .as_deref()
        .and_then(|want| {
            preset
                .models
                .iter()
                .find(|m| m.value == want)
                .map(|m| m.value)
        })
        .unwrap_or(preset.default_model);

    TranslationSettings {
        provider: preset.provider,
        target,
        libre_url: "",
        ai: AiRequest {
            base_url: preset.base_url,
            model,
            api_key: if key_matches {
                config.ai_key.clone().unwrap_or_default()
            } else {
                String::new()
            },
            request: serde_json::from_str(preset.request).unwrap_or(Value::Null),
        },
    }
}

/// The patch applied when the provider dropdown changes: key cleared, endpoint
/// and model set to the new preset's defaults.
pub fn select_ai_provider(provider: &str) -> Option<ProviderSelection> {
    let preset = ai_preset(provider)?;
    Some(ProviderSelection {
        ai_preset: preset.provider,
        ai_base_url: preset.base_url,
        ai_model: preset.default_model,
        ai_key: String::new(),
    })
}

/// The on-disk key for a per-song, per-source translation decision.
///
/// `JSON.stringify([track, source.trim().toLowerCase()])` — the exact byte
/// shape matters because the key is stored in `lyrics.translationOverrides`.
pub fn source_translation_key(track: &str, source: &str) -> String {
    let source = source.trim().to_lowercase();
    serde_json::to_string(&[track, source.as_str()])
        .unwrap_or_else(|_| String::from("[]"))
}

/// The on-disk key for a per-song, per-candidate translation decision.
///
/// Scoped cleanly to `(track, source, id)` when a candidate id is known so
/// toggling smart translation on one uploaded version only affects that exact
/// version of that song; falls back to `source_translation_key` if `id` is empty.
pub fn candidate_translation_key(track: &str, source: &str, id: &str) -> String {
    let source = source.trim().to_lowercase();
    let id = id.trim();
    if id.is_empty() {
        return source_translation_key(track, source.as_str());
    }
    serde_json::to_string(&[track, source.as_str(), id])
        .unwrap_or_else(|_| String::from("[]"))
}

/// Is translation on for this song+source?
///
/// An explicit boolean override wins; anything else falls back to the global
/// `autoTranslate` switch, which must be literally `true`.
pub fn is_translation_enabled(config: &Value, key: &str) -> bool {
    let lyrics = config.get("lyrics").unwrap_or(&Value::Null);
    if !key.is_empty() {
        if let Some(choice) = lyrics
            .get("translationOverrides")
            .and_then(|overrides| overrides.get(key))
            .and_then(Value::as_bool)
        {
            return choice;
        }
    }
    lyrics.get("autoTranslate").and_then(Value::as_bool) == Some(true)
}
