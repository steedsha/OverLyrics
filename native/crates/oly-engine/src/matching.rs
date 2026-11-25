//! Track/candidate scoring: `compareTrack` and its three sub-comparators.
//!
//! Ported 1:1 from `tests/oracle/match.js`. The cascade order matters — an
//! exact match after simplification is `perfect`, a title-bridge hit is
//! `perfect` even though the strings share no characters, a `(deluxe)` tail
//! bump is `veryHigh`, and only then does Dice similarity get a say. Pinyin
//! equivalence exists because SMTC hands us `Yanzi Sun` while the Chinese
//! sources store `孙燕姿`; it only runs when one side is Latin and the other is
//! Han, and it never invents a match the string layer would have made.
//!
//! Goldens: `native/tests/fixtures/oracle/engine/match.json` (62 cases).

use std::sync::LazyLock;

use regex::Regex;

use crate::data::pinyin::PY_TABLE;
use crate::data::t2s::T2S_PAIRS;
use crate::title_bridge;
use crate::types::{Track, Candidate};

/// Part levels, ordered by how much of the 30-point budget they earn.
pub const PERFECT: &str = "perfect";
pub const VERY_HIGH: &str = "veryHigh";
pub const HIGH: &str = "high";
pub const MEDIUM: &str = "medium";
pub const LOW: &str = "low";
pub const NO_MATCH: &str = "noMatch";

fn score_of(level: Option<&str>) -> f64 {
    match level {
        Some(PERFECT) => 7.0,
        Some(VERY_HIGH) => 6.0,
        Some(HIGH) => 5.0,
        Some(MEDIUM) => 4.0,
        Some(LOW) => 2.0,
        Some(NO_MATCH) => 0.0,
        _ => 0.0,
    }
}

fn percent_of(level: &str) -> f64 {
    match level {
        PERFECT => 100.0,
        VERY_HIGH => 99.0,
        HIGH => 95.0,
        MEDIUM => 90.0,
        LOW => 30.0,
        _ => -1.0,
    }
}

/// Traditional -> simplified, per character, matching layer only.
pub fn to_simplified(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            T2S_PAIRS
                .binary_search_by_key(&c, |(t, _)| *t)
                .map(|i| T2S_PAIRS[i].1)
                .unwrap_or(c)
        })
        .collect()
}

/// Full-width -> half-width, ideographic space, lowercase, t2s, collapse.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            _ => c,
        };
        for lc in c.to_lowercase() {
            out.push(lc);
        }
    }
    let simplified = to_simplified(&out);
    simplified.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_feat(s: &str) -> String {
    let mut t = s;
    if let Some(i) = t.find("(feat.") {
        t = &t[..i];
    }
    if let Some(i) = t.find(" - feat.") {
        t = &t[..i];
    }
    t.trim().to_string()
}

fn bigrams(s: &str) -> std::collections::HashSet<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = std::collections::HashSet::new();
    if chars.is_empty() {
        return out;
    }
    if chars.len() == 1 {
        out.insert(chars[0].to_string());
        return out;
    }
    for i in 0..chars.len() - 1 {
        out.insert(chars[i..i + 2].iter().collect());
    }
    out
}

fn dice(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let aa = bigrams(a);
    let bb = bigrams(b);
    let inter = aa.intersection(&bb).count();
    (2 * inter) as f64 / (aa.len() + bb.len()) as f64
}

fn clean(s: &str) -> String {
    let mut t = s
        .replace('\u{2019}', "'")
        .replace('\u{FF0C}', ",")
        .replace('\u{FF08}', " (")
        .replace('\u{FF09}', " )")
        .replace('[', "(")
        .replace(']', ")");
    while t.contains("  ") {
        t = t.replace("  ", " ");
    }
    t.trim().replace("acoustic version", "acoustic")
}

fn dash_form(s: &str) -> String {
    let t = s.replacen(" - ", " (", 1).trim().to_string();
    format!("{t})").replace(' ', "")
}

/// `compareName` — title/album matching.
pub fn compare_name(a: Option<&str>, b: Option<&str>) -> Option<String> {
    let (a, b) = (a?, b?);
    let mut n1 = normalize(a);
    let mut n2 = normalize(b);
    if n1.is_empty() || n2.is_empty() {
        return None;
    }
    if n1 == n2 {
        return Some(PERFECT.into());
    }
    if title_bridge::bridged(&n1, &n2) {
        return Some(PERFECT.into());
    }
    n1 = clean(&n1);
    n2 = clean(&n2);
    if n1 == n2 {
        return Some(PERFECT.into());
    }
    if dash_form(&n1) == dash_form(&n2) {
        return Some(VERY_HIGH.into());
    }

    let special_compare = |special: &str| -> bool {
        let tag = format!("({special}");
        let c1 = n1.contains(&tag);
        let c2 = n2.contains(&tag);
        if c1 && !c2 {
            let prefix = n1[..n1.find(&tag).unwrap()].trim();
            return prefix == n2;
        }
        if c2 && !c1 {
            let prefix = n2[..n2.find(&tag).unwrap()].trim();
            return prefix == n1;
        }
        false
    };
    let single_special = |special: &str| -> bool {
        let tag = format!("({special}");
        if n1.contains(&tag) && n2.contains(&tag) {
            let p1 = n1[..n1.find(&tag).unwrap()].trim();
            let p2 = n2[..n2.find(&tag).unwrap()].trim();
            return p1 == p2;
        }
        false
    };
    let duo_special = |s1: &str, s2: &str| -> bool {
        let t1 = format!("({s1}");
        let t2 = format!("({s2}");
        if n1.contains(&t1) && n2.contains(&t2) {
            let p1 = n1[..n1.find(&t1).unwrap()].trim();
            let p2 = n2[..n2.find(&t2).unwrap()].trim();
            return p1 == p2;
        }
        if n1.contains(&t2) && n2.contains(&t1) {
            let p1 = n1[..n1.find(&t2).unwrap()].trim();
            let p2 = n2[..n2.find(&t1).unwrap()].trim();
            return p1 == p2;
        }
        false
    };

    for sp in ["deluxe", "explicit", "special edition", "bonus track", "feat", "with"] {
        if special_compare(sp) {
            return Some(VERY_HIGH.into());
        }
    }
    if duo_special("feat", "explicit") || duo_special("with", "explicit") {
        return Some(HIGH.into());
    }
    if single_special("feat") || single_special("with") {
        return Some(HIGH.into());
    }
    if n1.contains('(') && !n2.contains('(') && n1[..n1.find('(').unwrap()].trim() == n2 {
        return Some(MEDIUM.into());
    }
    if n2.contains('(') && !n1.contains('(') && n2[..n2.find('(').unwrap()].trim() == n1 {
        return Some(MEDIUM.into());
    }

    let c1: Vec<char> = n1.chars().collect();
    let c2: Vec<char> = n2.chars().collect();
    if c1.len() == c2.len() {
        let same = c1.iter().zip(c2.iter()).filter(|(a, b)| a == b).count();
        let r = same as f64 / c1.len() as f64;
        if (r >= 0.8 && c1.len() >= 4) || (r >= 0.5 && (2..=3).contains(&c1.len())) {
            return Some(HIGH.into());
        }
    }

    let sim = dice(&n1, &n2) * 100.0;
    if sim > 90.0 {
        return Some(VERY_HIGH.into());
    }
    if sim > 80.0 {
        return Some(HIGH.into());
    }
    if sim > 68.0 {
        return Some(MEDIUM.into());
    }
    if sim > 55.0 {
        return Some(LOW.into());
    }
    let _ = strip_feat(&n1);
    Some(NO_MATCH.into())
}

/// Every reading combination for a string of Han characters, capped at eight.
fn pinyin_forms(s: &str) -> Option<Vec<String>> {
    if s.is_empty() || !s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return None;
    }
    let mut forms = vec![String::new()];
    for ch in s.chars() {
        if !('\u{4e00}'..='\u{9fff}').contains(&ch) {
            continue;
        }
        let readings = PY_TABLE
            .binary_search_by_key(&ch, |(c, _)| *c)
            .ok()
            .map(|i| PY_TABLE[i].1)?;
        let mut next = Vec::new();
        for f in &forms {
            for r in readings.split(',') {
                if next.len() >= 8 {
                    break;
                }
                next.push(format!("{f}{r}"));
            }
        }
        forms = next;
    }
    if forms.is_empty() { None } else { Some(forms) }
}

fn pinyin_equiv_rewrite(reference: &[String], other: &[String]) -> Option<Vec<String>> {
    let mut out: Option<Vec<String>> = None;
    for i in 0..other.len() {
        let Some(forms) = pinyin_forms(&other[i]) else {
            continue;
        };
        for candidate in reference {
            if candidate == &other[i] {
                break;
            }
            let na: String = candidate
                .chars()
                .filter(|c| c.is_ascii_lowercase())
                .collect();
            if na.len() < 2 {
                continue;
            }
            let hit = forms.iter().any(|f| f == &na)
                || (na.len() > 3
                    && forms.iter().any(|f| {
                        f.chars().count() == na.chars().count()
                            && format!("{na}{na}").contains(f.as_str())
                    }));
            if hit {
                let buf = out.get_or_insert_with(|| other.to_vec());
                buf[i] = candidate.clone();
                break;
            }
        }
    }
    out
}

/// `compareArtist` — artist-list matching with cross-script pinyin equivalence.
pub fn compare_artist(a1: Option<&[String]>, a2: Option<&[String]>) -> Option<String> {
    let (a1, a2) = (a1?, a2?);
    let mut l1: Vec<String> = a1
        .iter()
        .map(|s| normalize(s))
        .filter(|s| !s.is_empty())
        .collect();
    let mut l2: Vec<String> = a2
        .iter()
        .map(|s| normalize(s))
        .filter(|s| !s.is_empty())
        .collect();
    if l1.is_empty() || l2.is_empty() {
        return None;
    }

    let han = |s: &str| s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    let latin = |s: &str| s.chars().any(|c| c.is_ascii_lowercase());
    let mixed = |p: &[String], q: &[String]| {
        p.iter().any(|x| latin(x)) && q.iter().any(|x| han(x))
    };
    if mixed(&l1, &l2) {
        if let Some(rewritten) = pinyin_equiv_rewrite(&l1, &l2) {
            l2 = rewritten;
        }
    } else if mixed(&l2, &l1) {
        if let Some(rewritten) = pinyin_equiv_rewrite(&l2, &l1) {
            l1 = rewritten;
        }
    }

    let count = l2.iter().filter(|x| l1.contains(x)).count();

    if count == l1.len() && l1.len() == l2.len() {
        return Some(PERFECT.into());
    }
    if (count + 1 >= l1.len() && l1.len() >= 2)
        || (l1.len() > 6 && count as f64 / l1.len() as f64 > 0.8)
    {
        return Some(VERY_HIGH.into());
    }
    if count == 1 && l1.len() == 1 && l2.len() == 2 {
        return Some(HIGH.into());
    }
    if l1.len() > 5
        && (l2[0].contains("various") || l2[0].contains("群星"))
    {
        return Some(VERY_HIGH.into());
    }
    if l1.len() > 7 && l2.len() > 7 && count as f64 / l1.len() as f64 > 0.66 {
        return Some(HIGH.into());
    }
    if l1.len() == 1 && l2.len() > 1 && l1[0].starts_with(&l2[0]) {
        return Some(HIGH.into());
    }
    if l1.len() == 1
        && l2.len() > 1
        && l2[0].chars().count() > 3
        && l1[0].contains(&l2[0])
    {
        return Some(HIGH.into());
    }
    if l1.len() == 1
        && l2.len() > 1
        && l2[0].chars().count() > 1
        && l1[0].contains(&l2[0])
    {
        return Some(MEDIUM.into());
    }
    if count == 1 && l1.len() == 1 && l2.len() >= 3 {
        return Some(MEDIUM.into());
    }
    if count >= 2 {
        return Some(LOW.into());
    }
    Some(NO_MATCH.into())
}

/// `compareDuration` — millisecond distance buckets.
pub fn compare_duration(d1: Option<f64>, d2: Option<f64>) -> Option<String> {
    let (d1, d2) = (d1?, d2?);
    if d1 == 0.0 || d2 == 0.0 {
        return None;
    }
    let diff = (d1 - d2).abs();
    if diff < 200.0 {
        return Some(PERFECT.into());
    }
    if diff < 700.0 {
        return Some(VERY_HIGH.into());
    }
    if diff < 1500.0 {
        return Some(HIGH.into());
    }
    if diff < 3500.0 {
        return Some(MEDIUM.into());
    }
    if diff < 8000.0 {
        return Some(LOW.into());
    }
    Some(NO_MATCH.into())
}

/// One scored component list, as `compareTrack().parts` exposes it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackParts {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[serde(rename = "albumArtist")]
    pub album_artist: Option<String>,
    pub duration: Option<String>,
}

/// The 30-point verdict `compareTrack` returns.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackScore {
    pub level: String,
    pub score: f64,
    pub percent: f64,
    pub raw_level: String,
    pub parts: TrackParts,
}

/// `toList` — string/array to a flat artist list, splitting `/`, `&`, `、`,
/// `,`, `feat.` and `ft.` both at the outer and the element level.
pub fn to_list(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    let value = value?;
    let raw: Vec<String> = match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            })
            .collect(),
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Null => return None,
        other => vec![other.to_string()],
    };
    let mut out = Vec::new();
    for item in raw {
        for part in split_artist_list(&item) {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
    }
    Some(out)
}

fn split_artist_list(s: &str) -> Vec<String> {
    static SPLIT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\s*[,&、/]\s*|\s+feat\.?\s+|\s+ft\.?\s+")
            .expect("artist list regex")
    });
    SPLIT.split(s).map(|p| p.to_string()).collect()
}

/// `compareTrack` — weighted 30-point score with missing-component stretching.
pub fn compare_track(track: &Track, cand: &Candidate) -> TrackScore {
    let title = compare_name(Some(&track.title), Some(&cand.title));
    let artist = compare_artist(
        track.artist_list().as_deref(),
        cand.artist_list().as_deref(),
    );
    let album = compare_name(track.album.as_deref(), cand.album.as_deref());
    let album_artist = compare_artist(
        track.album_artists.as_deref(),
        cand.album_artists.as_deref(),
    );
    let duration = compare_duration(track.duration_ms, cand.duration_ms);

    let mut total = score_of(title.as_deref())
        + score_of(artist.as_deref())
        + score_of(album.as_deref()) * 0.4
        + score_of(album_artist.as_deref()) * 0.2
        + score_of(duration.as_deref());

    // Stretch missing components so a source that omits the album is not
    // systematically penalised. A missing *candidate* duration is exempt; a
    // missing *track* duration (0 ms at playback start) is not — treating it as
    // "unknown" used to stretch same-name wrong versions to a perfect 100.
    let mut null_count = 0.0;
    if album.is_none() {
        null_count += 0.4;
    }
    if album_artist.is_none() {
        null_count += 0.2;
    }
    if duration.is_none() && !cand.duration_ms.is_some_and(|d| d > 0.0) {
        null_count += 1.0;
    }
    const FULL: f64 = 30.0;
    let denom = FULL - null_count * 7.0;
    if denom > 0.0 {
        total = total * FULL / denom;
    }

    let raw_level = if total > 21.0 {
        PERFECT
    } else if total > 19.0 {
        VERY_HIGH
    } else if total > 17.0 {
        HIGH
    } else if total > 15.0 {
        "prettyHigh"
    } else if total > 11.0 {
        MEDIUM
    } else if total > 8.0 {
        LOW
    } else if total > 3.0 {
        "veryLow"
    } else {
        NO_MATCH
    };

    let percent = if raw_level == "prettyHigh" {
        percent_of(MEDIUM)
    } else if raw_level == "veryLow" {
        -1.0
    } else {
        percent_of(raw_level)
    };
    let level = if raw_level == "prettyHigh" {
        MEDIUM.to_string()
    } else {
        raw_level.to_string()
    };

    TrackScore {
        level,
        score: (total * 100.0).round() / 100.0,
        percent,
        raw_level: raw_level.to_string(),
        parts: TrackParts {
            title,
            artist,
            album,
            album_artist,
            duration,
        },
    }
}

/// Live-search track scoring: normalises `durationSec`-only candidates (such as
/// LRCLIB) and strongly penalises sources whose duration diverges blatantly from
/// the playing track so mismatched cuts drop below the 70% picker threshold.
pub fn score_candidate(track: &Track, cand: &Candidate) -> TrackScore {
    let cand_ms = cand
        .duration_ms
        .filter(|d| d.is_finite() && *d > 0.0)
        .or_else(|| {
            cand.duration_sec
                .filter(|d| d.is_finite() && *d > 0.0)
                .map(|s| s * 1000.0)
        });
    let mut normalized = cand.clone();
    if normalized.duration_ms != cand_ms {
        normalized.duration_ms = cand_ms;
    }
    let mut result = compare_track(track, &normalized);
    if let (Some(song_ms), Some(dur_ms)) = (
        track.duration_ms.filter(|d| d.is_finite() && *d > 0.0),
        cand_ms,
    ) {
        let diff = (dur_ms - song_ms).abs();
        let abnormal_limit = (song_ms * 0.10).clamp(10_000.0, 20_000.0);
        if diff > abnormal_limit {
            result.score = result.score.min(10.0);
            result.percent = percent_of(LOW);
            result.level = LOW.to_string();
            result.raw_level = LOW.to_string();
        }
    }
    result
}

#[cfg(test)]
mod candidate_scoring_tests {
    use super::*;

    #[test]
    fn abnormal_duration_and_sec_only_candidates_are_penalised_below_threshold() {
        let song = Track {
            title: "Night Song".into(),
            artist: Some("Artist".into()),
            duration_ms: Some(210_000.0),
            ..Default::default()
        };
        let mut wrong_sec_only = Candidate {
            source: "lrclib".into(),
            id: "1".into(),
            title: "Night Song".into(),
            artists: Some(vec!["Artist".into()]),
            duration_sec: Some(45.0),
            ..Default::default()
        };
        let bad = score_candidate(&song, &wrong_sec_only);
        assert!(bad.percent < 70.0, "expected < 70%, got {}", bad.percent);

        wrong_sec_only.duration_sec = Some(210.5);
        let good = score_candidate(&song, &wrong_sec_only);
        assert!(good.percent >= 90.0, "expected >= 90%, got {}", good.percent);
    }
}
