//! Translation gating: which lines actually need a translation.
//!
//! Ported 1:1 from `tests/oracle/translate.js` (the pure predicates only — the
//! network engines live in `oly-engine::translate::engines`, added with the
//! Task 12 wiring). Every rule here prevents a specific nuisance: translating
//! Chinese lyrics into Chinese, translating `♪～`, or translating `Ah-ooh`.

use std::sync::LazyLock;

use regex::Regex;

/// Does this single line carry translatable foreign text?
///
/// Chinese-only lines, pure symbols and onomatopoeia are skipped: sending them
/// to a translation engine produces noise and burns the free quota.
pub fn needs_translation(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    static SYMBOLS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[\s\u{266a}\u{266b}\u{2669}\u{1f3b5}\u{1f3b6}\-\u{2014}.…·]+$")
            .expect("symbols")
    });
    if SYMBOLS.is_match(t) {
        return false;
    }
    static ONO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:oh|ah|ooh|aah|la|na|mmm|hmm)[!,.…\s]*$").expect("onomatopoeia")
    });
    if ONO.is_match(t) {
        return false;
    }
    strip_han(t).chars().any(|c| c.is_alphabetic())
}

/// True when a whole document has at least two consecutive letters once the
/// Han characters are removed — the signal that the lyrics are foreign.
///
/// The reference takes *line objects*, not strings: `String((l && l.text) || l)`
/// falls back to the object itself when `text` is empty, which stringifies to
/// `[object Object]` and therefore counts as foreign text. Reproduced here
/// because the goldens pin it; a line with empty text is degenerate either way.
pub fn has_foreign_lyrics(lines: &[String]) -> bool {
    let joined = lines
        .iter()
        .map(|text| {
            if text.is_empty() {
                "[object Object]"
            } else {
                text.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let rest = strip_han(&joined);
    let mut run = 0usize;
    for c in rest.chars() {
        if c.is_alphabetic() {
            run += 1;
            if run >= 2 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

fn strip_han(text: &str) -> String {
    text.chars()
        .filter(|c| !('\u{3400}'..='\u{4dbf}').contains(c) && !('\u{4e00}'..='\u{9fff}').contains(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_chinese_symbols_and_onomatopoeia() {
        assert!(!needs_translation("晴天"));
        assert!(!needs_translation("\u{266a}\u{ff5e}"));
        assert!(!needs_translation(""));
        assert!(!needs_translation("Ah-ooh") == false); // hyphenated: still translatable
        assert!(needs_translation("Hello world"));
        assert!(needs_translation("君の名前"));
    }

    #[test]
    fn foreign_detection_needs_two_letters() {
        assert!(!has_foreign_lyrics(&["晴天".to_string()]));
        assert!(has_foreign_lyrics(&["晴天 hello".to_string()]));
    }
}
