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

/// Normalisation for artist comparison: punctuation → space (not dropped),
/// simplify, collapse.
fn norm_artist(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_lowercase().chars() {
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

/// Loose artist match: containment, token overlap, or prefix agreement.
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
        return true;
    }
    list.iter().any(|a| {
        if a.chars().count() == 1 || want.chars().count() == 1 {
            if a == &want {
                return true;
            }
        } else if a.contains(&want) || want.contains(a) {
            return true;
        }
        let at = artist_tokens(a);
        let wt = artist_tokens(&want);
        at.iter()
            .any(|x| x.chars().count() >= 2 && wt.iter().any(|w| w == x))
    })
}

/// Candidate duration within ±3.5 s of the playing track.
pub fn duration_ok(c: &Candidate, song: &Track) -> bool {
    let dur = c.duration_sec.unwrap_or(0.0) * 1000.0;
    let want = song.duration_ms.unwrap_or(0.0);
    dur > 0.0 && want > 0.0 && (dur - want).abs() <= crate::crosslang::DURATION_IDENTITY_WINDOW_MS
}

/// Identity guard for automatic source selection: artist agreement, or a tight
/// duration match when the artist cannot agree (cross-language).
pub fn identity_plausible(c: &Candidate, song: &Track) -> bool {
    let artists = c.artist_list().unwrap_or_default();
    if artist_ok(&artists, &primary_artist(song)) {
        return true;
    }
    duration_ok(c, song)
}

/// List-membership gate: title must agree, and either the artist or the
/// duration must agree. A title mismatch can still pass when the artist is
/// pinyin-equivalent (≥ medium) *and* the duration is tight.
pub fn identity_gate(c: &Candidate, song: &Track) -> bool {
    if c.rescue == Some(true) && c.verified.is_some() {
        return true;
    }
    let artists = c.artist_list().unwrap_or_default();
    let want = primary_artist(song);
    let t_ok = title_ok(&c.title, &song.title, Some(&want));
    let a_ok = artist_ok(&artists, &want);
    if t_ok && (a_ok || duration_ok(c, song)) {
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
}
