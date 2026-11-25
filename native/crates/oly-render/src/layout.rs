//! Pure layout arithmetic: wrap decisions, per-word segmentation and the
//! geometry a line hands to the mask pass.
//!
//! Nothing here touches a text system, so every rule in this file is unit
//! testable and deterministic. [`crate::text::TextLayoutCache`] shapes first and
//! then feeds the shaped advances in.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// A rectangle in logical pixels, origin at the line's left/top edge.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// How a line breaks when it does not fit the panel width.
///
/// `Natural` is the browser's greedy `text-wrap: wrap`; `Balanced` is the
/// `text-wrap: balance` mode the config exposes. Both only break at
/// opportunities the Unicode line-breaking rules allow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineBreakMode {
    #[default]
    Natural,
    Balanced,
}

/// One word plus the media window it is highlighted over.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl SourceWord {
    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

/// A break opportunity inside the shaped text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakOpportunity {
    /// UTF-8 byte offset the next visual line starts at.
    pub byte: usize,
    /// Width of the text before the break, in logical pixels.
    pub width_bits: u32,
}

/// Every byte offset where a line may legally break.
///
/// A break is allowed after a space, after a hyphen, and between two CJK
/// characters. That mirrors what Chromium does for `word-break: normal` with a
/// CJK fallback available, which is what the reference screenshots came from.
pub fn break_opportunities(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut prev: Option<(usize, usize, char)> = None;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let ch = grapheme.chars().next().unwrap_or(' ');
        if let Some((_, prev_end, prev_ch)) = prev {
            let space_before = prev_ch.is_whitespace();
            let cjk_pair = is_cjk(prev_ch) && is_cjk(ch);
            let hyphen_before = prev_ch == '-' || prev_ch == '\u{2010}';
            if space_before || cjk_pair || hyphen_before {
                out.push(prev_end);
            }
        }
        prev = Some((byte, byte + grapheme.len(), ch));
    }
    let _ = byte_offset_of_last(text);
    out.retain(|b| *b < text.len());
    out
}

fn byte_offset_of_last(text: &str) -> usize {
    text.len()
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x2E80..=0x2FFF
        | 0x3040..=0x30FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF
        | 0x20000..=0x2FA1F)
}

/// Byte offset -> x position, using shaped glyph pen advances.
///
/// `pens` must be sorted by byte index. A byte offset that falls inside a
/// cluster resolves to that cluster's pen, which is what a per-word mask wants:
/// the highlight boundary never lands inside a glyph.
pub fn x_at_byte(pens: &[(usize, f32)], byte: usize) -> f32 {
    if pens.is_empty() {
        return 0.0;
    }
    match pens.binary_search_by_key(&byte, |(b, _)| *b) {
        Ok(i) => pens[i].1,
        Err(0) => pens[0].1,
        Err(i) => pens[i - 1].1,
    }
}

/// Greedy wrap: fill each line until the next opportunity would overflow.
pub fn wrap_greedy(
    text: &str,
    pens: &[(usize, f32)],
    max_width: f32,
) -> Vec<Range<usize>> {
    let total = x_at_byte(pens, text.len());
    if max_width <= 0.0 || total <= max_width {
        return vec![0..text.len()];
    }
    let opportunities = break_opportunities(text);
    let mut lines = Vec::new();
    let mut start = 0usize;
    loop {
        let start_x = x_at_byte(pens, start);
        // What is left fits whole: emit it even if the tail has no break
        // opportunity, which is what a browser does with the last word.
        if x_at_byte(pens, text.len()) - start_x <= max_width {
            lines.push(start..text.len());
            break;
        }
        let mut last_fit = None;
        for &opp in opportunities.iter().filter(|b| **b > start) {
            let w = x_at_byte(pens, opp) - start_x;
            if w <= max_width {
                last_fit = Some(opp);
            } else {
                break;
            }
        }
        let Some(end) = last_fit else {
            // Not even one opportunity fits: emit a single cluster so the caller
            // makes progress instead of looping forever.
            let end = text[start..]
                .grapheme_indices(true)
                .nth(1)
                .map(|(b, _)| start + b)
                .unwrap_or(text.len());
            lines.push(start..end);
            start = end;
            if start >= text.len() {
                break;
            }
            continue;
        };
        lines.push(start..end);
        start = end;
        if start >= text.len() {
            break;
        }
    }
    lines
}

/// Balanced wrap: keep the greedy line count, then shrink the measure until the
/// greedy fill needs one more line. The result fills each line more evenly than
/// `wrap_greedy`, which is the observable effect of `text-wrap: balance`.
pub fn wrap_balanced(
    text: &str,
    pens: &[(usize, f32)],
    max_width: f32,
) -> Vec<Range<usize>> {
    let total = x_at_byte(pens, text.len());
    if max_width <= 0.0 || total <= max_width {
        return vec![0..text.len()];
    }
    let target_lines = wrap_greedy(text, pens, max_width).len();
    if target_lines <= 1 {
        return vec![0..text.len()];
    }
    let mut lo = max_width / target_lines as f32;
    let mut hi = max_width;
    let mut best = wrap_greedy(text, pens, max_width);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        let wrapped = wrap_greedy(text, pens, mid);
        if wrapped.len() <= target_lines {
            best = wrapped;
            hi = mid;
        } else {
            lo = mid;
        }
    }
    best
}

/// Offsets of every grapheme cluster start, used to apply tracking.
pub fn cluster_starts(text: &str) -> Vec<usize> {
    text.grapheme_indices(true).map(|(b, _)| b).collect()
}

/// Tracking in pixels applied after every cluster except the last.
///
/// GPUI shapes without letter spacing; the reference applies `letter-spacing`,
/// which shifts every cluster after the first by `em * font_size`.
pub fn tracking_offsets(text: &str, font_size: f32, letter_spacing_em: f32) -> Vec<(usize, f32)> {
    let spacing = font_size * letter_spacing_em;
    cluster_starts(text)
        .into_iter()
        .enumerate()
        .map(|(i, byte)| (byte, spacing * i as f32))
        .collect()
}

/// Split `[start,end)` into the visual lines it spans.
pub fn segments_for_range(lines: &[Range<usize>], start: usize, end: usize) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.start < end && start < line.end)
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pens(text: &str, per_char: f32) -> Vec<(usize, f32)> {
        let mut out = Vec::new();
        let mut x = 0.0;
        for (b, g) in text.grapheme_indices(true) {
            out.push((b, x));
            x += per_char * g.chars().count() as f32;
        }
        out.push((text.len(), x));
        out
    }

    #[test]
    fn breaks_after_spaces_and_between_cjk() {
        let latin = break_opportunities("all that i want");
        assert_eq!(latin, vec![4, 9, 11]);
        let cjk = break_opportunities("身边身边");
        assert_eq!(cjk.len(), 3);
    }

    #[test]
    fn greedy_fills_to_the_measure() {
        let text = "alpha beta gamma delta";
        let p = pens(text, 10.0);
        let lines = wrap_greedy(text, &p, 17.0 * 10.0);
        assert_eq!(lines, vec![0..17, 17..22]);
        for line in &lines {
            let w = x_at_byte(&p, line.end) - x_at_byte(&p, line.start);
            assert!(w <= 17.0 * 10.0 + f32::EPSILON, "line overflows: {w}");
        }
    }

    #[test]
    fn balanced_never_adds_lines_and_evens_them_out() {
        let text = "alpha beta gamma delta epsilon zeta";
        let p = pens(text, 10.0);
        let greedy = wrap_greedy(text, &p, 20.0 * 10.0);
        let balanced = wrap_balanced(text, &p, 20.0 * 10.0);
        assert_eq!(balanced.len(), greedy.len());
        let widest = |lines: &[Range<usize>]| {
            lines
                .iter()
                .map(|l| x_at_byte(&p, l.end) - x_at_byte(&p, l.start))
                .fold(0.0f32, f32::max)
        };
        assert!(
            widest(&balanced) <= widest(&greedy) + 0.001,
            "balanced should not be wider than greedy"
        );
    }

    #[test]
    fn tracking_applies_per_cluster() {
        let offsets = tracking_offsets("abcd", 10.0, 0.1);
        assert_eq!(offsets, vec![(0, 0.0), (1, 1.0), (2, 2.0), (3, 3.0)]);
    }

    #[test]
    fn a_word_can_span_two_lines() {
        let lines = vec![0..10, 10..20];
        assert_eq!(segments_for_range(&lines, 5, 15), vec![0, 1]);
        assert_eq!(segments_for_range(&lines, 0, 3), vec![0]);
    }
}
