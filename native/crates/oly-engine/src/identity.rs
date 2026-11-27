//! Candidate identity guards: `titleOk`, `artistOk`, `durationOk`,
//! `identityPlausible`, `identityGate`.
//!
//! Ported 1:1 from the helper half of `src/engine/index.ts`. These decide
//! whether a search hit is *this* song at all, before any scoring or ordering
//! gets a say. Every branch exists because a specific wrong match reached the
//! screen: Lorde's `Green Light` borrowed Sun Yanzi's lyrics, `Hush` matched
//! `Hush My Heart`, a one-character artist name matched every string containing
//! that letter.

use std::sync::LazyLock;

use regex::Regex;

use crate::matching::compare_artist;
use crate::strings::primary_artist;
use crate::title_bridge;
use crate::types::{Candidate, Track};
use crate::zhconv::to_simplified;

/// Lowercase, drop punctuation, simplify, collapse spaces. Note that unlike
/// [`crate::matching::normalize`] this one **removes** punctuation instead of
/// mapping full-width forms.
fn n0(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
        if c.is_alphanumeric() || c.is_whitespace() {
            out.push(c);
        }
    }
    to_simplified(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Version remarks that may follow a short title without changing its identity.
static BENIGN_TAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(live|remastered?\d*|acoustic|demo|instrumental|karaoke|radioedit|edit|mix|remix|version|single|ep|mono|stereo|deluxe|explicit|bonus|spedup|slowed|reverb|cover|piano|stripped|现场|伴奏|纯音乐|翻唱|重制|重置|版)+$",
    )
    .expect("benign tail")
});

static FEAT_IN_PARENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\((?:feat\.?|ft\.?|featuring|with)\s+([^)]+)\)").expect("feat parens")
});

static CJK_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\u{4e00}-\u{9fff}]+").expect("cjk run"));

/// Are these two titles plausibly the same song?
///
/// The mixed-title split (`綠光 - Green Light`) is why the function looks at
/// both sides: LRCLIB submissions put the Chinese title and the English title
/// in one string, and neither half equals the wanted title as a whole.
pub fn title_ok(cand: &str, want: &str, artist_hint: Option<&str>) -> bool {
    let want_n = n0(want);
    let want_sq = squash(&want_n);

    for src in [cand, want] {
        if src.contains(" - ") {
            for part in src.split(" - ") {
                let p = squash(&n0(part));
                if !p.is_empty()
                    && (p == want_sq
                        || title_bridge::bridged(&p, &want_sq)
                        || title_bridge::bridged(&want_sq, &p))
                {
                    return true;
                }
            }
        }
    }

    let a = n0(cand);
    let b = n0(want);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if squash(&a) == squash(&b) {
        return true;
    }
    if title_bridge::bridged(&a, &b) {
        return true;
    }

    if a.contains(&b) || b.contains(&a) {
        // A substring hit is only identity evidence when the contained side is
        // substantial: two words, or a run of four Han characters. `Hush` ⊂
        // `Hush My Heart` is a different song.
        let (short, long) = if a.chars().count() <= b.chars().count() {
            (a.as_str(), b.as_str())
        } else {
            (b.as_str(), a.as_str())
        };
        let words = short.split(' ').filter(|w| !w.is_empty()).count();
        let han_run = CJK_RUN
            .find_iter(short)
            .map(|m| m.as_str().chars().count())
            .max()
            .unwrap_or(0);
        if words >= 2 || han_run >= 4 {
            return true;
        }

        // Single-word titles: only a prefix/suffix embedding whose remainder is
        // a known version remark (or names the artist / a feat participant) is
        // the same song.
        let sq = squash(short);
        let lq = squash(long);
        let prefix = lq.starts_with(&sq);
        let suffix = lq.ends_with(&sq);
        if !prefix && !suffix {
            return false;
        }
        let short_chars = short.chars().count();
        let long_chars = long.chars().count();
        let rest: String = if prefix {
            long.chars().skip(short_chars).collect()
        } else {
            long.chars().take(long_chars - short_chars).collect()
        };
        let rest = rest.trim().to_string();
        if rest.is_empty() {
            return true;
        }
        if BENIGN_TAIL_RE.is_match(&squash(&rest)) {
            return true;
        }
        let artist_tokens: Vec<String> = artist_hint
            .unwrap_or("")
            .to_lowercase()
            .split_whitespace()
            .filter(|t| t.chars().count() >= 2)
            .map(|t| t.to_string())
            .collect();
        let rest_tokens: Vec<&str> = rest.split_whitespace().collect();
        if !artist_tokens.is_empty()
            && rest_tokens
                .iter()
                .any(|t| artist_tokens.iter().any(|a| a == t))
        {
            return true;
        }
        // `Fortnight (feat. Post Malone)` against a clean `Fortnight`: the
        // remainder names the feat participants, which are part of the title.
        if let Some(caps) = FEAT_IN_PARENS.captures(want) {
            for name in caps[1].split(['/', ',', '、', '&']) {
                let ft: Vec<String> = name
                    .to_lowercase()
                    .split_whitespace()
                    .filter(|t| t.chars().count() >= 2)
                    .map(|t| t.to_string())
                    .collect();
                if !ft.is_empty()
                    && rest_tokens
                        .iter()
                        .any(|t| ft.iter().any(|f| f == t))
                {
                    return true;
                }
            }
        }
        return false;
    }
    false
}

fn fold_latin_diacritic(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ú' | 'ù' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ß' => 's',
        'ź' | 'ż' | 'ž' => 'z',
        'ł' => 'l',
        'ð' | 'ď' | 'đ' => 'd',
        _ => c,
    }
}

/// Normalisation for artist comparison: punctuation → space (not dropped),
/// fold Latin diacritics, simplify Chinese, collapse spaces.
fn norm_artist(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
        let c = fold_latin_diacritic(c);
        if c.is_alphanumeric() || c.is_whitespace() {
            out.push(c);
        } else {
            out.push(' ');
        }
    }
    to_simplified(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Common connector/prefix words that must not by themselves prove two
/// different artists are the same (e.g. "The Weeknd" vs "The Chainsmokers").
fn is_artist_stopword(token: &str) -> bool {
    matches!(
        token,
        "the"
            | "and"
            | "of"
            | "in"
            | "on"
            | "at"
            | "to"
            | "for"
            | "de"
            | "la"
            | "le"
            | "el"
            | "los"
            | "las"
            | "dj"
            | "mc"
            | "mr"
            | "ms"
            | "dr"
            | "lil"
            | "feat"
            | "ft"
            | "with"
            | "vs"
    )
}

fn script_flags(s: &str) -> (bool, bool) {
    let mut has_latin = false;
    let mut has_non_latin = false;
    for c in s.chars() {
        if !c.is_alphabetic() {
            continue;
        }
        let folded = fold_latin_diacritic(c.to_lowercase().next().unwrap_or(c));
        if folded.is_ascii_alphabetic() {
            has_latin = true;
        } else {
            has_non_latin = true;
        }
    }
    (has_latin, has_non_latin)
}

/// True when the playing artist and candidate artists are written in different
/// scripts (e.g. Latin `"Jay Chou"` vs CJK `"周杰伦"`, `"IU"` vs `"아이유"`).
/// Duration-only artist exemption is restricted to this cross-script case so
/// two unrelated English (or two unrelated Chinese) artists cannot collide.
pub fn is_cross_script(want_artist: &str, cand_artists: &[String]) -> bool {
    let (w_lat, w_non) = script_flags(want_artist);
    if !w_lat && !w_non {
        return false;
    }
    let mut c_lat = false;
    let mut c_non = false;
    for a in cand_artists {
        let (l, n) = script_flags(a);
        c_lat |= l;
        c_non |= n;
    }
    if !c_lat && !c_non {
        return false;
    }
    (w_non && !c_non && c_lat) || (c_non && !w_non && w_lat)
}

/// Split an artist string the way `artistOk` does: whitespace separates tokens
/// too, so `Taylor Swift` yields `["taylor", "swift"]`.
fn artist_tokens(s: &str) -> Vec<String> {
    static SPLIT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\s+|\s*[,&/、]\s*|feat\.?|ft\.?|with\s").expect("artist split")
    });
    SPLIT
        .split(s)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Loose artist match: containment, non-stopword token overlap, or exact match.
pub fn artist_ok(cand_artists: &[String], want_artist: &str) -> bool {
    let want = norm_artist(want_artist);
    if want.is_empty() {
        return true;
    }
    let list: Vec<String> = cand_artists
        .iter()
        .map(|a| norm_artist(a))
        .filter(|a| !a.is_empty())
        .collect();
    if list.is_empty() {
        return false;
    }
    let wt: Vec<String> = artist_tokens(&want)
        .into_iter()
        .filter(|t| t.chars().count() >= 2 && !is_artist_stopword(t))
        .collect();
    list.iter().any(|a| {
        if a.chars().count() == 1 || want.chars().count() == 1 {
            if a == &want {
                return true;
            }
        } else if a == &want || a.contains(&want) || want.contains(a) {
            return true;
        }
        let at = artist_tokens(a);
        at.iter().any(|x| {
            x.chars().count() >= 2 && !is_artist_stopword(x) && wt.iter().any(|w| w == x)
        })
    })
}

/// Candidate duration within ±3.5 s of the playing track.
pub fn duration_ok(c: &Candidate, song: &Track) -> bool {
    let dur = c.duration_sec.unwrap_or(0.0) * 1000.0;
    let want = song.duration_ms.unwrap_or(0.0);
    dur > 0.0 && want > 0.0 && (dur - want).abs() <= crate::crosslang::DURATION_IDENTITY_WINDOW_MS
}

/// Identity guard for automatic source selection: artist agreement, or a tight
/// duration match when the artist cannot agree across different scripts.
pub fn identity_plausible(c: &Candidate, song: &Track) -> bool {
    let artists = c.artist_list().unwrap_or_default();
    let want = primary_artist(song);
    if artist_ok(&artists, &want) {
        return true;
    }
    is_cross_script(&want, &artists) && duration_ok(c, song)
}

/// List-membership gate: title must agree, and either the artist agrees or
/// (for cross-script names such as Latin vs CJK) the duration is tight.
/// A title mismatch can still pass when the artist is pinyin-equivalent
/// (≥ medium) *and* the duration is tight.
pub fn identity_gate(c: &Candidate, song: &Track) -> bool {
    if c.rescue == Some(true) && c.verified.is_some() {
        return true;
    }
    let artists = c.artist_list().unwrap_or_default();
    let want = primary_artist(song);
    let t_ok = title_ok(&c.title, &song.title, Some(&want));
    let a_ok = artist_ok(&artists, &want);
    if t_ok && (a_ok || (is_cross_script(&want, &artists) && duration_ok(c, song))) {
        return true;
    }
    if !t_ok {
        let level = compare_artist(Some(&[want.clone()]), Some(&artists));
        let a_ok2 = a_ok
            || level
                .as_deref()
                .is_some_and(|l| l != "noMatch" && l != "low");
        if a_ok2 && duration_ok(c, song) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_titles_are_not_substring_evidence() {
        assert!(!title_ok("Hush My Heart", "Hush", None));
        assert!(title_ok("Hush (Live)", "Hush", None));
        assert!(title_ok("Green Light", "Green Light", None));
    }

    #[test]
    fn mixed_language_titles_split() {
        assert!(title_ok("綠光 - Green Light", "Green Light", Some("Yanzi Sun")));
    }

    #[test]
    fn one_character_artists_need_exact_equality() {
        assert!(!artist_ok(&["A".to_string()], "Night Tapes"));
        assert!(artist_ok(&["A".to_string()], "A"));
    }

    #[test]
    fn stopwords_do_not_match_unrelated_artists_while_diacritics_do() {
        assert!(!artist_ok(&["The Chainsmokers".into()], "The Weeknd"));
        assert!(!artist_ok(&["Lil Wayne".into()], "Lil Nas X"));
        assert!(!artist_ok(&["DJ Khaled".into()], "DJ Snake"));
        assert!(!artist_ok(&[], "Dua Lipa"));
        assert!(artist_ok(&["The Weeknd".into()], "The Weeknd"));
        assert!(artist_ok(&["The Marias".into()], "The Marías"));
        assert!(artist_ok(&["Renee Rapp".into()], "Reneé Rapp"));
        assert!(artist_ok(
            &["Gracie Abrams".into(), "Taylor Swift".into()],
            "Gracie Abrams"
        ));
    }

    #[test]
    fn identity_gate_rejects_same_script_wrong_artist_even_with_matching_duration() {
        let song = Track {
            title: "Wild Horses".into(),
            artist: Some("Birdy".into()),
            duration_ms: Some(181_000.0),
            ..Default::default()
        };
        let wrong_artist = Candidate {
            title: "Wild Horses".into(),
            artist: Some("The Rolling Stones".into()),
            duration_sec: Some(181.0),
            ..Default::default()
        };
        let right_artist = Candidate {
            title: "Wild Horses".into(),
            artist: Some("Birdy".into()),
            duration_sec: Some(181.0),
            ..Default::default()
        };
        assert!(!identity_gate(&wrong_artist, &song));
        assert!(identity_gate(&right_artist, &song));
    }

    #[test]
    fn identity_gate_allows_cross_script_artist_with_tight_duration() {
        let zh_song = Track {
            title: "Mojito".into(),
            artist: Some("Jay Chou".into()),
            duration_ms: Some(185_000.0),
            ..Default::default()
        };
        let zh_cand = Candidate {
            title: "Mojito".into(),
            artist: Some("周杰伦".into()),
            duration_sec: Some(185.0),
            ..Default::default()
        };
        assert!(identity_gate(&zh_cand, &zh_song));

        let kr_song = Track {
            title: "Love wins all".into(),
            artist: Some("IU".into()),
            duration_ms: Some(271_000.0),
            ..Default::default()
        };
        let kr_cand = Candidate {
            title: "Love wins all".into(),
            artist: Some("아이유".into()),
            duration_sec: Some(271.0),
            ..Default::default()
        };
        assert!(identity_gate(&kr_cand, &kr_song));
    }
}
