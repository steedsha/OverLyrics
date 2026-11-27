//! Candidate ordering and the fused word+translation pairs.
//!
//! Ported 1:1 from `tests/oracle/picker-order.js`. This module is the *only*
//! place that decides which candidate is "first visible": the picker panel and
//! the automatic source selection both read the same order, so the one the
//! engine picks is always the one the user sees at the top of the list.
//!
//! The rules encode three real incidents: AMLL's word+translation entries take
//! precedence over QQ's, a short "residual" submission of the same song must
//! sink below the complete one (first-line timestamp decides among identical
//! submissions), and a plain (no words, no translation) entry is demoted to 30 %
//! so the cross-language rescue chain still gets a chance to run.

use std::cmp::Ordering;

use crate::types::Candidate;

/// Default source priority order (index 0 = highest): AMLL > QQ > Kugou > Netease > LRCLIB.
pub const DEFAULT_SOURCE_ORDER: [&str; 5] = ["amlldb", "qq", "kugou", "netease", "lrclib"];

/// Preference when comparing single-badge tiers (`逐行+译文` vs `仅逐字`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TierPreference {
    #[default]
    TransFirst,
    WordFirst,
}

impl TierPreference {
    pub fn from_str_opt(value: Option<&str>) -> Self {
        match value {
            Some("word") => Self::WordFirst,
            _ => Self::TransFirst,
        }
    }
}

/// Complete ordering policy shared by the picker list and automatic selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortPolicy {
    pub source_order: Vec<String>,
    pub tier_preference: TierPreference,
}

impl Default for SortPolicy {
    fn default() -> Self {
        Self {
            source_order: DEFAULT_SOURCE_ORDER.iter().map(|s| (*s).to_string()).collect(),
            tier_preference: TierPreference::TransFirst,
        }
    }
}

impl SortPolicy {
    /// Normalize a configured source order so every known source appears once.
    pub fn normalize_source_order(raw: &[String]) -> Vec<String> {
        let mut out = Vec::with_capacity(DEFAULT_SOURCE_ORDER.len());
        for id in raw {
            let norm = canonical_source_id(id);
            if DEFAULT_SOURCE_ORDER.contains(&norm) && !out.iter().any(|s: &String| s == norm) {
                out.push(norm.to_string());
            }
        }
        for id in DEFAULT_SOURCE_ORDER {
            if !out.iter().any(|s| s == id) {
                out.push(id.to_string());
            }
        }
        out
    }

    pub fn from_config(config: &serde_json::Value) -> Self {
        let raw_order: Vec<String> = config
            .pointer("/lyrics/sourceOrder")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let source_order = Self::normalize_source_order(&raw_order);
        let tier_preference = TierPreference::from_str_opt(
            config.pointer("/lyrics/tierPreference").and_then(|v| v.as_str()),
        );
        Self {
            source_order,
            tier_preference,
        }
    }
}

fn canonical_source_id(id: &str) -> &str {
    match id {
        "amlldb" | "amllb" | "amll" => "amlldb",
        "qq" => "qq",
        "kugou" => "kugou",
        "netease" => "netease",
        "lrclib" => "lrclib",
        other => other,
    }
}

/// True when the candidate has been verified to carry a usable timeline with at
/// least one line (not merely static source capability hints).
pub fn is_verified_usable(c: &Candidate) -> bool {
    c.probe_failed != Some(true)
        && c.verified
            .as_ref()
            .is_some_and(|v| v.timeline == Some(true) && v.lines.unwrap_or(0) > 0)
}

/// Visibility groups, best first, parameterized by `TierPreference`.
pub fn group_order_with(c: &Candidate, pref: TierPreference) -> u8 {
    match kind_of(c) {
        "both_bg" => 0,
        "both" => 1,
        "trans" => match pref {
            TierPreference::TransFirst => 2,
            TierPreference::WordFirst => 3,
        },
        "word" => match pref {
            TierPreference::TransFirst => 3,
            TierPreference::WordFirst => 2,
        },
        "line" => 4,
        _ => 5,
    }
}

/// Visibility groups, best first:
/// `逐字+翻译+和声 > 逐字+翻译 > 逐行+翻译 > 逐字 > 逐行 > 原文`.
pub fn group_order(c: &Candidate) -> u8 {
    group_order_with(c, TierPreference::TransFirst)
}

/// Human label for a candidate's group.
pub fn group_label(c: &Candidate) -> &'static str {
    match kind_of(c) {
        "both_bg" => "逐字 + 译文 + 和声",
        "both" => "逐字 + 译文",
        "trans" => "逐行 + 译文",
        "word" => "仅逐字",
        "line" => "逐行",
        _ => "原文",
    }
}

/// What the candidate actually carries. A verified probe wins; before the
/// probe lands only the source's static capability is known.
pub fn kind_of(c: &Candidate) -> &'static str {
    if c.fused == Some(true) {
        return if c.verified.as_ref().and_then(|v| v.bg) == Some(true) {
            "both_bg"
        } else {
            "both"
        };
    }
    let word = match &c.verified {
        Some(v) => v.word == Some(true),
        None => c.cap_word == Some(true),
    };
    let trans = match &c.verified {
        Some(v) => v.trans == Some(true),
        None => c.cap_trans == Some(true),
    };
    let bg = c.verified.as_ref().and_then(|v| v.bg) == Some(true);
    let timeline = c.verified.as_ref().and_then(|v| v.timeline);
    match (word, trans, bg) {
        (true, true, true) => "both_bg",
        (true, true, false) => "both",
        (false, true, _) => "trans",
        (true, false, _) => "word",
        (false, false, _) => {
            if timeline == Some(true) {
                "line"
            } else {
                "plain"
            }
        }
    }
}

fn source_rank_with(c: &Candidate, order: &[String]) -> usize {
    if c.fused == Some(true) {
        return usize::MAX;
    }
    let canonical = canonical_source_id(c.source.as_str());
    order
        .iter()
        .position(|s| canonical_source_id(s.as_str()) == canonical)
        .unwrap_or(usize::MAX - 1)
}

/// Same-submission signature: identical title and identical artist set.
fn submission_sig(c: &Candidate) -> String {
    let mut artists: Vec<String> = c
        .artist
        .clone()
        .unwrap_or_default()
        .split('/')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    artists.sort();
    format!(
        "{}|{}",
        c.title.trim().to_lowercase(),
        artists.join("/").to_lowercase()
    )
}

fn first_ms(c: &Candidate) -> f64 {
    match &c.verified {
        Some(v) => v.first_ms.filter(|x| x.is_finite()).unwrap_or(f64::INFINITY),
        None => f64::INFINITY,
    }
}

fn cmp_f64(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

fn availability_tier(c: &Candidate) -> u8 {
    if is_verified_usable(c) {
        0
    } else if c.probe_failed == Some(true)
        || c.verified.as_ref().is_some_and(|v| v.lines == Some(0))
    {
        2
    } else {
        1
    }
}

/// Policy-aware ordering function shared by the picker panel and automatic
/// source selection: verified availability > badge tier > source order > score.
pub fn cand_sort_with(a: &Candidate, b: &Candidate, policy: &SortPolicy) -> Ordering {
    let aa = availability_tier(a);
    let ab = availability_tier(b);
    if aa != ab {
        return aa.cmp(&ab);
    }
    let ga = group_order_with(a, policy.tier_preference);
    let gb = group_order_with(b, policy.tier_preference);
    if ga != gb {
        return ga.cmp(&gb);
    }
    let sa = source_rank_with(a, &policy.source_order);
    let sb = source_rank_with(b, &policy.source_order);
    if sa != sb {
        return sa.cmp(&sb);
    }
    // Two submissions of the same song: percent is metadata noise (a different
    // album suffix alone moves it ten points), so the earlier first line wins —
    // a residual copy that opens late sinks.
    if a.source == b.source && submission_sig(a) == submission_sig(b) {
        return cmp_f64(first_ms(a), first_ms(b));
    }
    let p = cmp_f64(b.percent.unwrap_or(0.0), a.percent.unwrap_or(0.0));
    if p != Ordering::Equal {
        return p;
    }
    cmp_f64(first_ms(a), first_ms(b))
}

/// The one ordering function. Used by the picker panel and by automatic
/// selection alike.
pub fn cand_sort(a: &Candidate, b: &Candidate) -> Ordering {
    cand_sort_with(a, b, &SortPolicy::default())
}

/// Whether a candidate may appear in the list at all: a failed probe, or a
/// probe that found no timeline, disqualifies it.
pub fn usable(c: &Candidate) -> bool {
    if c.probe_failed == Some(true) {
        return false;
    }
    if let Some(v) = &c.verified {
        if v.timeline != Some(true) || v.lines.unwrap_or(0) == 0 {
            return false;
        }
    }
    true
}

/// The options `buildCands` takes.
#[derive(Clone, Debug, Default)]
pub struct PickerOptions {
    pub threshold: Option<f64>,
    pub duration_ms: Option<f64>,
    pub demote: Option<bool>,
    pub exclude_instrumental: Option<bool>,
}

/// The picker result: one entry per source plus the fused pairs.
#[derive(Clone, Debug, Default)]
pub struct PickerResult {
    pub cands: Vec<Candidate>,
    pub fused: Vec<Candidate>,
    pub source_count: usize,
}

fn same_song(c: &Candidate, duration_ms: f64) -> bool {
    let cd = c.duration_sec.unwrap_or(0.0) * 1000.0;
    if duration_ms > 0.0 && cd > 0.0 {
        return (cd - duration_ms).abs() <= 8000.0;
    }
    c.percent.unwrap_or(0.0) >= 95.0
}

/// Filter, pick the best per source, and build the fused word × translation
/// pairs.
pub fn build_cands(raw: &[Candidate], opts: &PickerOptions) -> PickerResult {
    let threshold = opts.threshold.unwrap_or(70.0);
    let duration_ms = opts.duration_ms.unwrap_or(0.0);
    let demote = opts.demote != Some(false);

    // Demote plain entries *before* the threshold pass so they cannot be
    // resurrected by the per-source benchmark exemption.
    let mut work: Vec<Candidate> = raw.to_vec();
    if demote {
        for c in &mut work {
            if kind_of(c) == "plain" && c.probe_failed != Some(true) {
                // Instrumental charts legitimately carry no words; skip those.
                let instrumental = c
                    .extra
                    .as_ref()
                    .and_then(|e| e.get("instrumental"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !instrumental {
                    c.percent = Some(30.0);
                }
            }
        }
    }

    // The best score per source acts as a benchmark: an entry scoring below the
    // threshold still passes when it *is* that source's best submission.
    let mut best_sig: std::collections::HashMap<String, (f64, String)> =
        std::collections::HashMap::new();
    for c in &work {
        let sig = submission_sig(c);
        let percent = c.percent.unwrap_or(0.0);
        best_sig
            .entry(c.source.clone())
            .and_modify(|e| {
                if percent > e.0 {
                    *e = (percent, sig.clone());
                }
            })
            .or_insert((percent, sig));
    }

    let mut passed: Vec<Candidate> = work
        .into_iter()
        .filter(|c| {
            if !usable(c) {
                return false;
            }
            let percent = c.percent.unwrap_or(0.0);
            if percent >= threshold {
                return true;
            }
            if kind_of(c) == "plain" {
                return false;
            }
            best_sig
                .get(&c.source)
                .is_some_and(|(_, sig)| *sig == submission_sig(c))
        })
        .collect();

    if passed.iter().any(|c| same_song(c, duration_ms)) {
        passed.retain(|c| same_song(c, duration_ms));
    }

    // Group by source, preserving first appearance.
    let mut order: Vec<String> = Vec::new();
    let mut by_source: std::collections::HashMap<String, Vec<Candidate>> =
        std::collections::HashMap::new();
    for c in passed {
        if !by_source.contains_key(&c.source) {
            order.push(c.source.clone());
        }
        by_source.entry(c.source.clone()).or_default().push(c);
    }

    let mut cands: Vec<Candidate> = Vec::new();
    for source in &order {
        let arr = by_source.get_mut(source).expect("source present");
        arr.sort_by(cand_sort);
        let best = arr[0].clone();
        cands.push(best.clone());

        // Alt entries: with a known duration only a version more than five
        // seconds apart qualifies; without one, up to two more submissions are
        // shown so the panel can offer a manual choice.
        if best.duration_sec.unwrap_or(0.0) > 0.0 {
            if let Some(alt) = arr.iter().skip(1).find(|x| {
                x.percent.unwrap_or(0.0) >= 90.0
                    && (x.duration_sec.unwrap_or(0.0) - best.duration_sec.unwrap_or(0.0)).abs()
                        > 5.0
            }) {
                cands.push(alt.clone());
            }
        } else {
            for x in arr.iter().skip(1).take(2) {
                let sig_ok = best_sig
                    .get(&x.source)
                    .is_some_and(|(_, sig)| *sig == submission_sig(x));
                if x.percent.unwrap_or(0.0) >= 90.0 || sig_ok {
                    cands.push(x.clone());
                }
            }
        }
    }

    // Fused pairs: verified word-only × verified trans-only across sources.
    let words: Vec<&Candidate> = cands
        .iter()
        .filter(|c| kind_of(c) == "word" && c.verified.is_some())
        .collect();
    let transs: Vec<&Candidate> = cands
        .iter()
        .filter(|c| kind_of(c) == "trans" && c.verified.is_some())
        .collect();
    let mut fused: Vec<Candidate> = Vec::new();
    for w in &words {
        for t in &transs {
            if w.source == t.source {
                continue;
            }
            let lines = match (&w.verified, &t.verified) {
                (Some(wv), Some(tv)) => wv.lines.or(tv.lines).unwrap_or(0),
                _ => 0,
            };
            let foreign = [w.verified.as_ref(), t.verified.as_ref()]
                .into_iter()
                .flatten()
                .any(|v| v.foreign == Some(true));
            fused.push(Candidate {
                source: "fused".to_string(),
                id: format!("{}+{}", w.source, t.source),
                title: w.title.clone(),
                artist: w.artist.clone(),
                artists: w.artists.clone(),
                album: w.album.clone(),
                duration_sec: w.duration_sec,
                percent: Some(
                    w.percent
                        .unwrap_or(0.0)
                        .min(t.percent.unwrap_or(0.0)),
                ),
                verified: Some(crate::types::Verified {
                    word: Some(true),
                    trans: Some(true),
                    bg: None,
                    lines: Some(lines),
                    timeline: Some(true),
                    first_ms: None,
                    foreign: Some(foreign),
                }),
                fused: Some(true),
                ..Candidate::default()
            });
        }
    }
    fused.sort_by(|a, b| {
        cmp_f64(b.percent.unwrap_or(0.0), a.percent.unwrap_or(0.0))
    });
    let mut seen = std::collections::HashSet::new();
    fused.retain(|f| {
        let key = format!(
            "{}\u{1}{}",
            f.id.split('+').next().unwrap_or(""),
            f.id.split('+').nth(1).unwrap_or("")
        );
        seen.insert(key)
    });
    fused.truncate(4);

    PickerResult {
        source_count: by_source.len(),
        cands,
        fused,
    }
}
