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

/// Visibility groups, best first:
/// `逐字+翻译+和声 > 逐字+翻译 > 逐行+翻译 > 逐字 > 逐行 > 原文`.
pub fn group_order(c: &Candidate) -> u8 {
    match kind_of(c) {
        "both_bg" => 0,
        "both" => 1,
        "trans" => 2,
        "word" => 3,
        "line" => 4,
        _ => 5,
    }
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

fn src_privilege(c: &Candidate) -> u8 {
    if c.fused == Some(true) {
        return 0;
    }
    match c.source.as_str() {
        // Source priority within the same content tier: AMLL > QQ > Kugou > Netease > LRCLIB.
        "amlldb" | "amllb" | "amll" => 5,
        "qq" => 4,
        "kugou" => 3,
        "netease" => 2,
        "lrclib" => 1,
        _ => 0,
    }
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

/// The one ordering function. Used by the picker panel and by automatic
/// selection alike.
pub fn cand_sort(a: &Candidate, b: &Candidate) -> Ordering {
    let ga = group_order(a);
    let gb = group_order(b);
    if ga != gb {
        return ga.cmp(&gb);
    }
    let sa = src_privilege(a);
    let sb = src_privilege(b);
    if sa != sb {
        return sb.cmp(&sa);
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
