//! Track identity strings: `primaryArtist`, `trackKey`, `titleVariants`.
//!
//! Ported 1:1 from the helper half of `src/engine/index.ts`. These three feed
//! the lyric cache key and the cross-language rescue chain, so their output is
//! part of the on-disk contract: a change here silently re-keys every cached
//! document.

use std::sync::LazyLock;

use regex::Regex;

use crate::types::Track;

/// The lead artist, with Apple Music's "Artist — Album" suffix stripped.
///
/// Apple's SMTC folds the album into the artist field and leaves `album` empty,
/// so `Lorde — Melodrama` has to become `Lorde` before it reaches a search
/// query or a cache key.
pub fn primary_artist(track: &Track) -> String {
    let raw = track
        .artist
        .clone()
        .filter(|a| !a.is_empty())
        .or_else(|| {
            track
                .artists
                .as_ref()
                .map(|a| a.join(" / "))
                .filter(|a| !a.is_empty())
        })
        .unwrap_or_default();
    static SEP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s+[\u{2014}\u{2013}]\s+|\s+-\s+").expect("artist sep"));
    let first = SEP.split(&raw).next().unwrap_or("").trim();
    if first.is_empty() {
        raw.trim().to_string()
    } else {
        first.to_string()
    }
}

/// Cache key: title + lead artist, lowercased with **all** whitespace removed.
///
/// Album and duration used to be part of this key, but both are still empty at
/// playback start, so one song landed on several cache entries — and the
/// "duration unknown" search often returned the wrong version.
pub fn track_key(track: &Track) -> String {
    let norm = |s: &str| s.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect::<String>();
    format!("{}\u{1}{}", norm(&track.title), norm(&primary_artist(track)))
}

/// ` - Remastered 2011` / ` - Live` / ` - Demo` and friends. The spaces around
/// the dash are required so `Love-Part 1` survives.
static VERSION_TAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s+[-\u{2013}\u{2014}]\s+(remaster(ed)?(\s*\d{4})?|live|demo|acoustic|instrumental|single|ep|album version|radio edit|edit|mix|remix|version|sessions?|mono|stereo|deluxe|karaoke|slowed|sped up|现场|伴奏|纯音乐|翻唱|重制|重置).*$",
    )
    .expect("version tail")
});

/// Unparenthesised `feat.` / `ft.` / `featuring` tail.
static FEAT_TAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(feat\.?|ft\.?|featuring)\s+.*$").expect("feat tail"));

/// Map the curly/fullwidth punctuation that shows up in CJK titles onto ASCII.
fn halfwidth_punct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let mapped = match c {
            '\u{2018}' | '\u{2019}' | '\u{201C}' | '\u{201D}' => '"',
            '\u{FF08}' => '(',
            '\u{FF09}' => ')',
            '\u{FF0C}' => ',',
            '\u{FF01}' => '!',
            '\u{FF1F}' => '?',
            '\u{FF1A}' => ':',
            '\u{FF1B}' => ';',
            '\u{3001}' => ',',
            '\u{3002}' => '.',
            '\u{3010}' => '[',
            '\u{3011}' => ']',
            '\u{FF0D}' => '-',
            '\u{FF5E}' => '~',
            other => other,
        };
        out.push(mapped);
    }
    out
}

/// Short title forms used by the identity gate and the rescue chain.
///
/// Order is meaningful: the raw title first, the half-width punctuation form
/// second (CJK sources use full-width brackets), and the "base" title — with a
/// parenthetical tail or a `- Remastered` style suffix removed — last. Capped at
/// three so a pathological title cannot balloon the search matrix.
pub fn title_variants(title: &str) -> Vec<String> {
    let raw = title.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out = vec![raw.to_string()];

    let half = halfwidth_punct(raw);
    if half != raw {
        out.push(half);
    }

    let base = out.last().cloned().unwrap_or_default();
    let mut stripped = base.clone();
    if let Some(i) = stripped.find('(') {
        let head = stripped[..i].trim().to_string();
        if !head.is_empty() {
            stripped = head;
        }
    }
    stripped = VERSION_TAIL_RE.replace(&stripped, "").to_string();
    stripped = FEAT_TAIL_RE.replace(&stripped, "").to_string();
    let stripped = stripped.trim().to_string();
    if !stripped.is_empty() && !out.contains(&stripped) {
        out.push(stripped);
    }

    out.truncate(3);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_artist_suffix_is_stripped() {
        let track = Track {
            title: "Green Light".into(),
            artist: Some("Lorde \u{2014} Melodrama".into()),
            ..Track::default()
        };
        assert_eq!(primary_artist(&track), "Lorde");
        assert_eq!(track_key(&track), "greenlight\u{1}lorde");
    }

    #[test]
    fn variants_strip_version_and_feat_tails() {
        assert_eq!(title_variants("Lust For Life").len(), 1);
        assert_eq!(
            title_variants("Lust For Life (Explicit)"),
            vec!["Lust For Life (Explicit)", "Lust For Life"]
        );
        assert_eq!(
            title_variants("Song - Remastered 2011"),
            vec!["Song - Remastered 2011", "Song"]
        );
        assert_eq!(
            title_variants("Song (feat. Someone)"),
            vec!["Song (feat. Someone)", "Song"]
        );
    }
}
