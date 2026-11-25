//! Cross-language rescue helpers.
//!
//! Ported 1:1 from `tests/oracle/crosslang.js`. The rescue chain exists because
//! a Chinese track whose SMTC metadata is English will never match a mainland
//! source on the string layer: the title is a different language and the artist
//! is pinyin. Consensus over the first few candidates plus a duration window is
//! the only identity evidence left.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::data::pinyin::PY_TABLE;
use crate::types::Candidate;
use crate::zhconv::to_simplified;

/// Any CJK ideograph.
pub static CJK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\u{3400}-\u{4dbf}\u{4e00}-\u{9fff}]").expect("cjk"));

/// Version markers (Live / Remix / Cover) sorted to the bottom unless the playing track itself is versioned.
pub static VERSION_MARK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)live|remix|演唱|concert|cover|翻唱").expect("version_mark"));

/// Two candidates within this window are considered the same recording.
pub const DURATION_IDENTITY_WINDOW_MS: f64 = 3500.0;

/// Lowercase, punctuation → space, traditional → simplified, collapse.
pub fn norm_txt(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
        if c.is_alphanumeric() || c.is_whitespace() {
            out.push(c);
        } else {
            out.push(' ');
        }
    }
    let simplified = to_simplified(&out);
    simplified.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Yanzi Sun` → `["Yanzi Sun", "Sun Yanzi"]`; a single word stays alone.
///
/// The swap only applies when the *whole* trimmed string is printable ASCII
/// (`/^[\x20-\x7e]+$/`), so `"Hello，世界！"` keeps its single unsplit form even
/// though it contains spaces.
pub fn artist_name_variants(name: &str) -> Vec<String> {
    let raw = name.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let printable_ascii = raw.chars().all(|c| ('\u{20}'..='\u{7e}').contains(&c));
    if tokens.len() == 2 && printable_ascii {
        let swapped = format!("{} {}", tokens[1], tokens[0]);
        if swapped.to_lowercase() != raw.to_lowercase() {
            return vec![raw.to_string(), swapped];
        }
    }
    vec![raw.to_string()]
}

/// Generate romanized (pinyin) search variants for a Chinese artist name so LRCLIB
/// can be queried when SMTC provided a Chinese artist with an English song title.
pub fn chinese_artist_pinyin_variants(zh_artist: &str) -> Vec<String> {
    let simplified = to_simplified(zh_artist.trim());
    let syllables: Vec<&str> = simplified
        .chars()
        .filter(|c| CJK_RE.is_match(&c.to_string()))
        .filter_map(|c| {
            PY_TABLE
                .binary_search_by_key(&c, |(k, _)| *k)
                .ok()
                .and_then(|i| PY_TABLE[i].1.split(',').next())
        })
        .collect();
    let mut out = Vec::new();
    match syllables.len() {
        0 => {}
        1 => out.push(syllables[0].to_string()),
        2 => {
            out.push(format!("{} {}", syllables[0], syllables[1]));
            out.push(format!("{} {}", syllables[1], syllables[0]));
        }
        3 => {
            out.push(format!("{} {}{}", syllables[0], syllables[1], syllables[2]));
            out.push(format!("{}{} {}", syllables[1], syllables[2], syllables[0]));
        }
        _ => out.push(syllables.join(" ")),
    }
    out
}

/// Strip a parenthetical remark (both widths) from the tail.
fn strip_parens(s: &str) -> String {
    let mut out = s.to_string();
    for (open, close) in [('(', ')'), ('\u{FF08}', '\u{FF09}')] {
        if let (Some(i), Some(j)) = (out.find(open), out.rfind(close)) {
            if j > i {
                out = format!("{}{}", &out[..i], &out[j + close.len_utf8()..]);
            }
        }
    }
    out.trim().to_string()
}

/// `綠光 - Green Light` → `綠光` when the right half is the wanted title.
///
/// `zh_artist` short-circuits the check: when the prefix is the known Chinese
/// artist name, the other half is the title and is returned instead.
pub fn parse_chinese_title(track_name: &str, want_title: &str, zh_artist: Option<&str>) -> Option<String> {
    let idx = track_name.find(" - ")?;
    let pre = track_name[..idx].trim();
    let post = track_name[idx + 3..].trim();
    if pre.is_empty() || post.is_empty() || !CJK_RE.is_match(pre) {
        return None;
    }
    if let Some(zh) = zh_artist {
        if !zh.trim().is_empty() && norm_txt(pre) == norm_txt(zh) {
            return Some(post.to_string());
        }
    }
    let stripped = strip_parens(post);
    let target = norm_txt(want_title);
    if !target.is_empty() && norm_txt(&stripped).contains(&target) {
        return Some(pre.to_string());
    }
    None
}

/// The most common Chinese artist among the first five entries of each list.
pub fn consensus_chinese_artist(lists: &[Vec<String>]) -> Option<String> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for list in lists {
        for name in list.iter().take(5) {
            if CJK_RE.is_match(name) {
                *counts.entry(name.trim().to_string()).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= 3)
        .max_by_key(|(_, n)| *n)
        .map(|(name, _)| name)
}

fn cand_duration_ms(c: &Candidate) -> f64 {
    c.duration_ms
        .filter(|d| d.is_finite() && *d > 0.0)
        .or_else(|| {
            c.duration_sec
                .filter(|d| d.is_finite() && *d > 0.0)
                .map(|d| d * 1000.0)
        })
        .unwrap_or(0.0)
}

/// Variant-aware bilingual title parser: checks any of `want_titles` against the right half.
pub fn parse_chinese_title_with_variants(
    track_name: &str,
    want_titles: &[String],
    zh_artist: Option<&str>,
) -> Option<String> {
    let idx = track_name.find(" - ")?;
    let pre = track_name[..idx].trim();
    let post = track_name[idx + 3..].trim();
    if pre.is_empty() || post.is_empty() || !CJK_RE.is_match(pre) {
        return None;
    }
    let pre_clean = strip_parens(pre);
    if !pre_clean.is_empty() && CJK_RE.is_match(&pre_clean) {
        let post_norm = norm_txt(&strip_parens(post));
        if want_titles.iter().any(|w| {
            let nw = norm_txt(w);
            !nw.is_empty() && post_norm.contains(&nw)
        }) {
            return Some(pre_clean);
        }
    }
    if let Some(zh) = zh_artist {
        let post_clean = strip_parens(post);
        if !zh.trim().is_empty()
            && !pre_clean.is_empty()
            && norm_txt(&pre_clean) == norm_txt(zh)
            && !post_clean.is_empty()
            && CJK_RE.is_match(&post_clean)
        {
            return Some(post_clean);
        }
    }
    None
}

/// Extract up to 3 distinct Chinese title candidates from LRCLIB entries, ordered by
/// duration proximity to `duration_ms`. When `require_strict_duration` is true (e.g.
/// title-only LRCLIB lookup when SMTC artist is already Chinese), entries without a
/// matching duration within `±DURATION_IDENTITY_WINDOW_MS` are rejected.
pub fn extract_chinese_titles(
    entries: &[Candidate],
    want_titles: &[String],
    zh_artist: Option<&str>,
    duration_ms: Option<f64>,
    require_strict_duration: bool,
) -> Vec<String> {
    let want_dur = duration_ms.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(0.0);
    if require_strict_duration && want_dur <= 0.0 {
        return Vec::new();
    }
    let mut ranked: Vec<(&Candidate, i32, f64)> = entries
        .iter()
        .filter_map(|e| {
            let d = cand_duration_ms(e);
            if want_dur > 0.0 && d > 0.0 {
                let diff = (d - want_dur).abs();
                if diff > DURATION_IDENTITY_WINDOW_MS {
                    return None;
                }
                Some((e, 0, diff))
            } else {
                Some((e, 1, 0.0))
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (e, _, _) in ranked {
        if let Some(zh) = parse_chinese_title_with_variants(&e.title, want_titles, zh_artist) {
            let key = norm_txt(&zh);
            if !key.is_empty() && seen.insert(key) {
                out.push(zh);
                if out.len() >= 3 {
                    break;
                }
            }
        }
    }
    out
}

/// Three-gate verification for rescued Chinese-source candidates:
/// 1. Artist matches `zh_artist` after `norm_txt` (handles Simplified/Traditional differences);
/// 2. Title contains `zh_title` after `norm_txt`;
/// 3. Duration falls within `±3.5 s` when SMTC duration is known (or excludes Live/Remix when unknown).
pub fn pick_l1_candidate(
    rows: &[Candidate],
    zh_artist: &str,
    zh_title: &str,
    duration_ms: Option<f64>,
    want_versioned: bool,
) -> Option<Candidate> {
    let zh_artist_n = norm_txt(zh_artist);
    let zh_title_n = norm_txt(zh_title);
    if zh_artist_n.is_empty() || zh_title_n.is_empty() {
        return None;
    }
    let mut ok: Vec<&Candidate> = rows
        .iter()
        .filter(|c| {
            let artists = c.artist_list().unwrap_or_default();
            let artist_match = artists.iter().any(|a| {
                let na = norm_txt(a);
                !na.is_empty() && (na == zh_artist_n || na.contains(&zh_artist_n))
            });
            artist_match && norm_txt(&c.title).contains(&zh_title_n)
        })
        .collect();
    if ok.is_empty() {
        return None;
    }
    ok.sort_by_key(|c| {
        let v = VERSION_MARK_RE.is_match(&c.title);
        if want_versioned { !v } else { v }
    });

    let want_dur = duration_ms.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(0.0);
    if want_dur > 0.0 {
        let mut near: Vec<(&Candidate, f64)> = ok
            .into_iter()
            .filter_map(|c| {
                let d = cand_duration_ms(c);
                if d > 0.0 {
                    let diff = (d - want_dur).abs();
                    if diff <= DURATION_IDENTITY_WINDOW_MS {
                        return Some((c, diff));
                    }
                }
                None
            })
            .collect();
        near.sort_by(|a, b| {
            let av = VERSION_MARK_RE.is_match(&a.0.title);
            let bv = VERSION_MARK_RE.is_match(&b.0.title);
            let vo = if want_versioned {
                (!av).cmp(&(!bv))
            } else {
                av.cmp(&bv)
            };
            vo.then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        near.first().map(|(c, _)| (*c).clone())
    } else {
        if !want_versioned {
            ok.retain(|c| !VERSION_MARK_RE.is_match(&c.title));
        }
        ok.first().map(|c| (*c).clone())
    }
}

/// Fallback LRCLIB entry picker (line-level synced lyrics). Rejects explicit duration conflicts.
pub fn pick_lrclib_entry(
    entries: &[Candidate],
    title_variants: &[String],
    artist_variants: &[String],
    duration_ms: Option<f64>,
) -> Option<Candidate> {
    let titles: Vec<String> = title_variants
        .iter()
        .map(|t| norm_txt(t))
        .filter(|t| !t.is_empty())
        .collect();
    let artists: Vec<String> = artist_variants
        .iter()
        .map(|a| norm_txt(a))
        .filter(|a| !a.is_empty())
        .collect();
    if titles.is_empty() || artists.is_empty() {
        return None;
    }
    let want_dur = duration_ms.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(0.0);
    let mut rel: Vec<(&Candidate, f64)> = entries
        .iter()
        .filter_map(|e| {
            let t = norm_txt(&e.title);
            let a = norm_txt(&e.artist_list().unwrap_or_default().join(" "));
            if t.is_empty() || a.is_empty() {
                return None;
            }
            if !titles.iter().any(|x| t.contains(x)) {
                return None;
            }
            if !artists.iter().any(|x| a.contains(x) || x.contains(&a)) {
                return None;
            }
            let d = cand_duration_ms(e);
            if want_dur > 0.0 && d > 0.0 && (d - want_dur).abs() > DURATION_IDENTITY_WINDOW_MS {
                return None;
            }
            let score = if want_dur > 0.0 && d > 0.0 {
                1e6 - (d - want_dur).abs()
            } else {
                0.0
            };
            Some((e, score))
        })
        .collect();
    rel.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    rel.first().map(|(e, _)| (*e).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norms_and_variants() {
        assert_eq!(
            norm_txt("  Hello\u{ff0c}\u{4e16}\u{754c}\u{ff01}  "),
            "hello 世界"
        );
        assert_eq!(norm_txt("我懷念的"), "我怀念的");
        assert_eq!(artist_name_variants("Yanzi Sun"), vec!["Yanzi Sun", "Sun Yanzi"]);
        assert_eq!(artist_name_variants("Adele"), vec!["Adele"]);
    }

    #[test]
    fn chinese_prefix_is_recovered() {
        assert_eq!(
            parse_chinese_title("綠光 - Green Light", "Green Light", Some("Yanzi Sun")),
            Some("綠光".to_string())
        );
        assert_eq!(parse_chinese_title("Green Light", "Green Light", None), None);
    }
}
