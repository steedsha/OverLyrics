//! Wire shapes shared by the parsers, the scorer and the document builder.
//!
//! Every field name and every "is this key present at all" distinction is
//! carried over from `src/engine/types.ts`, because the caches written by the
//! legacy build are read back by this crate and the QA goldens compare the
//! serialized form byte for byte. `Option<T>` plus
//! `skip_serializing_if = "Option::is_none"` reproduces JavaScript's
//! `undefined`-drops-the-key behaviour; `Option<T>` without the skip
//! reproduces an explicit `null`.

use serde::{Deserialize, Serialize};

use oly_contracts::LyricDoc;

/// `skip_serializing_if` helper: a zero shift is the default and stays out of
/// the JSON so build goldens keep their original shape.
fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// One timed syllable inside a line.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub text: String,
}

impl Word {
    pub fn new(start: i64, text: impl Into<String>) -> Self {
        Self {
            start: Some(start),
            end: None,
            text: text.into(),
        }
    }
}

/// A row as the parsers hand it over: no translation, no harmony, just timing.
///
/// The translation/harmony slots are empty at parse time and are filled by the
/// document builder; every one of them is skipped when absent so the parse
/// goldens stay byte-identical to the reference's `{start,end,text,words}`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedLine {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub text: String,
    pub words: Vec<Word>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trans: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roma: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub singer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "bgWords")]
    pub bg_words: Option<Vec<Word>>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "bgTrans")]
    pub bg_trans: Option<String>,
}

/// The parser output. `plainOnly` documents carry no `timing`, `lang` or
/// `meta` keys, and the legacy `buildDoc` distinguishes "missing" from "empty"
/// when it copies these onto the document, so the three are optional here.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedLyrics {
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    pub lines: Vec<ParsedLine>,
    pub has_word_level: bool,
    pub has_translation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plain_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_romanization: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duet: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<Vec<String>>,
}

impl ParsedLyrics {
    pub fn timing_word(&self) -> bool {
        self.timing.as_deref() == Some("Word")
    }
}

/// One row of a delivered lyric document. The builder emits only the keys the
/// source actually had — a line with a single word carries no `words` array at
/// all, so the renderer falls back to whole-line highlighting.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocLine {
    pub start: i64,
    pub end: Option<i64>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trans: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roma: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub singer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "bgWords")]
    pub bg_words: Option<Vec<Word>>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "bgTrans")]
    pub bg_trans: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<Word>>,
}

/// A candidate as the sources hand it to the picker.
///
/// The legacy layer is untyped JavaScript and reads missing fields through
/// `||`, `Number.isFinite` and `??`; every field here is therefore optional and
/// the picker applies the same coercions rather than inventing defaults at the
/// deserialization boundary.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Candidate {
    pub source: String,
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artists: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_artists: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_sec: Option<f64>,
    /// The same duration in milliseconds. The search-result fixtures carry
    /// `durationMs` while the picker fixtures carry `durationSec`; both shapes
    /// are read verbatim rather than converted at the boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// The scored form `{score, percent}` the document builder reads. The
    /// search layer fills it in; the picker fixtures carry `percent` instead,
    /// so both slots exist and `build_doc` prefers this one.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "match")]
    pub match_info: Option<MatchInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<Verified>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_failed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_word: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_trans: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
    /// The provider's display name, filled in by the search service so a panel
    /// row can render without a second lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
    /// True on the row the probe chose as this source's representative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_of_source: Option<bool>,
    /// The probe could not finish (timeout / connection trouble). The row stays
    /// visible and selectable: a soft failure is not proof the lyrics are bad.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_timed_out: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_error: Option<String>,
    /// True when injected by the cross-language rescue chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rescue: Option<bool>,
}

/// `cand.match` — the score pair produced by `compare_track`.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MatchInfo {
    pub score: f64,
    pub percent: f64,
}

/// What the probe learned about a candidate.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Verified {
    pub word: Option<bool>,
    pub trans: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<bool>,
    pub lines: Option<i64>,
    pub timeline: Option<bool>,
    pub first_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign: Option<bool>,
}

impl Candidate {
    /// `toList(cand.artists ?? cand.artist)` — the matcher's view of the artists.
    pub fn artist_list(&self) -> Option<Vec<String>> {
        if let Some(list) = self.artists.as_ref() {
            return Some(split_artist_values(list));
        }
        self.artist.as_deref().map(|a| split_artist_values(&[a.to_string()]))
    }

    pub fn artist_text(&self) -> String {
        if let Some(artist) = self.artist.as_deref() {
            return artist.to_string();
        }
        self.artists.as_ref().map(|a| a.join(" / ")).unwrap_or_default()
    }
}

/// The currently playing track, as the matcher consumes it.
///
/// `artist` and `artists` are both accepted because the SMTC packet and the
/// sources disagree on shape; `albumArtists` only exists on some providers.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Track {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artists: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_artists: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
}

impl Track {
    /// `toList(track.artists ?? track.artist)`, with the legacy second-level
    /// split so `"Taylor Swift / Bon Iver"` inside one array element splits too.
    pub fn artist_list(&self) -> Option<Vec<String>> {
        if let Some(list) = self.artists.as_ref() {
            return Some(split_artist_values(list));
        }
        self.artist.as_deref().map(|a| split_artist_values(&[a.to_string()]))
    }
}

/// Split on `, & 、 /` and on ` feat. ` / ` ft. `, trimming and dropping
/// blanks. One regex over the whole string keeps the split UTF-8 safe and
/// matches the reference's `toList` exactly (it also splits inside array
/// elements, because some AMLL submissions join the artists with `/`).
pub fn split_artist_values(values: &[String]) -> Vec<String> {
    static SPLIT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\s*[,&、/]\s*|\s+(?:feat\.?|ft\.?)\s+").expect("artist split")
    });
    let mut out = Vec::new();
    for value in values {
        for part in SPLIT.split(value) {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
    }
    out
}

/// Timing-correction statistics from donor alignment (`alignStats`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlignStats {
    pub matched: usize,
    pub med: i64,
    pub mad: i64,
}

/// A delivered document, mirroring the legacy `buildDoc` return shape.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BuiltDoc {
    pub ok: bool,
    pub provider: String,
    pub provider_name: String,
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: i64,
    pub match_score: f64,
    pub match_percent: f64,
    pub reference: String,
    pub format: String,
    /// `undefined` for plain-text documents: the legacy builder copies the
    /// parser's value straight through, so the key is absent rather than empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    pub has_word_level: bool,
    pub has_translation: bool,
    pub has_romanization: bool,
    pub has_background: bool,
    pub duet: bool,
    pub instrumental: bool,
    pub lines: Vec<DocLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<Vec<String>>,
    /// Which provider supplied the translation, when it came from a donor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_source: Option<String>,
    /// Whole-document timing shift in ms (negative is earlier).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub timing_shift_ms: i64,
    /// How many rows were nudged individually when the offset was not constant.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub timing_nudged: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_shift_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align_stats: Option<AlignStats>,
}

impl BuiltDoc {
    /// Project onto the render-facing contract.
    pub fn to_lyric_doc(&self) -> LyricDoc {
        LyricDoc {
            title: self.title.clone(),
            artist: self.artists.join(" / "),
            artists: self.artists.clone(),
            album: self.album.clone(),
            duration_sec: self.duration_ms as f64 / 1000.0,
            lines: self
                .lines
                .iter()
                .map(|l| oly_contracts::LyricLine {
                    start: Some(l.start),
                    end: l.end,
                    text: l.text.clone(),
                    trans: l.trans.clone().unwrap_or_default(),
                    roma: l.roma.clone().unwrap_or_default(),
                    words: l
                        .words
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|w| oly_contracts::Word {
                            start: w.start,
                            end: w.end,
                            text: w.text,
                        })
                        .collect(),
                    bg: l.bg.as_ref().and_then(|bg| match bg {
                        serde_json::Value::Bool(flag) => Some(oly_contracts::BgField::Flag(*flag)),
                        serde_json::Value::String(text) => Some(oly_contracts::BgField::Text(text.clone())),
                        _ => None,
                    }),
                    bg_words: l.bg_words.as_deref().unwrap_or_default().iter()
                        .map(|w| oly_contracts::Word { start: w.start, end: w.end, text: w.text.clone() })
                        .collect(),
                    bg_trans: l.bg_trans.clone(),
                    singer: l.singer.clone(),
                })
                .collect(),
            credits: self.credits.clone().unwrap_or_default(),
            duet: self.duet,
            instrumental: self.instrumental,
        }
    }
}
