//! Text shaping for lyric lines: one DirectWrite pass over the whole string,
//! then per-word geometry derived from the resulting glyph pens.
//!
//! Shaping the full line first is what keeps kerning and ligatures intact: a
//! word never gets shaped on its own, so "Van" + "illa" cannot lose the `Va` pair
//! the reference renderer applied to the whole run.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui_kit::{
    Font, FontFallbacks, FontWeight, Hsla, SharedString, TextRun, TextSystem, WindowTextSystem, font,
    px,
};
use oly_contracts::lyrics::is_cjk;

use crate::layout::{
    BreakOpportunity, LineBreakMode, Rect, SourceWord, cluster_starts, segments_for_range,
    tracking_offsets, wrap_balanced, wrap_greedy, x_at_byte,
};

/// Which faces a lyric line resolves to.
///
/// The CSS stack is `"Pretendard", "檎风黑体 CHS", ...` with Pretendard's
/// `unicode-range` deliberately excluding CJK, so in practice Latin resolves to
/// Pretendard and Han resolves to the host CJK face. Building the runs here with
/// the same split reproduces that resolution without relying on the platform's
/// implicit fallback order, which would silently pick a different face for a
/// missing glyph and move the line break.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub latin_family: String,
    pub cjk_family: String,
    pub weight: f32,
}

impl FontSpec {
    /// The frozen profile: Pretendard at the lyric weight, Han from Qyn CHS.
    ///
    /// The family name is the simplified `檎风黑体 CHS`, which is what the host
    /// font collection actually registers; the traditional spelling in the CSS
    /// stack only reaches the face through the browser's family matching. Asking
    /// DirectWrite for the traditional name misses and silently falls back to a
    /// system face, which changes the CJK advance from 1.0em to 0.65em and moves
    /// every Han break. `Qyn Flavor Grotesk CHS` is the same font's English name
    /// and stays in the fallback chain because it is what the stylesheet lists.
    pub fn frozen(weight: f32) -> Self {
        Self {
            latin_family: "Pretendard".to_string(),
            cjk_family: "檎风黑体 CHS".to_string(),
            weight,
        }
    }
}

/// Everything `TextLayoutCache::layout` needs, and nothing that changes per frame.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLayoutInput {
    pub text: String,
    pub words: Vec<SourceWord>,
    pub font: FontSpec,
    pub font_size: f32,
    pub letter_spacing_em: f32,
    /// `None` means "do not wrap", which is what a single-line mode wants.
    pub wrap_width: Option<f32>,
    pub line_break: LineBreakMode,
    pub line_height: f32,
    /// Index of the source lyric line, carried into every word so the caller can
    /// jump from a hit test back to the document without a second lookup.
    pub src_line: usize,
}

impl TextLayoutInput {
    fn cache_key(&self) -> LayoutKey {
        LayoutKey {
            text: self.text.clone(),
            words: self.words.clone(),
            latin_family: self.font.latin_family.clone(),
            cjk_family: self.font.cjk_family.clone(),
            weight_bits: self.font.weight.to_bits(),
            font_size_bits: self.font_size.to_bits(),
            letter_spacing_bits: self.letter_spacing_em.to_bits(),
            wrap_width_bits: self.wrap_width.map(f32::to_bits),
            line_break: self.line_break,
            line_height_bits: self.line_height.to_bits(),
            src_line: self.src_line,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LayoutKey {
    text: String,
    words: Vec<SourceWord>,
    latin_family: String,
    cjk_family: String,
    weight_bits: u32,
    font_size_bits: u32,
    letter_spacing_bits: u32,
    wrap_width_bits: Option<u32>,
    line_break: LineBreakMode,
    line_height_bits: u32,
    src_line: usize,
}

/// One visual line after wrapping.
#[derive(Clone, Debug, PartialEq)]
pub struct VisualLine {
    /// Byte range of the shaped text this line covers.
    pub byte_range: Range<usize>,
    /// Top-left corner inside the line box.
    pub rect: Rect,
    /// Byte offsets where a break was allowed, kept for diagnostics.
    pub opportunities: Vec<BreakOpportunity>,
}

/// The part of a word that landed on one visual line.
///
/// A single source word can span two visual lines (a long held syllable in a
/// narrow window). Each segment carries its own rect and the fraction of the
/// word's time window it covers, so the mask pass can highlight a half-word.
#[derive(Clone, Debug, PartialEq)]
pub struct WordSegment {
    pub line: usize,
    pub byte_range: Range<usize>,
    pub rect: Rect,
    /// `[0,1]` span of this word covered by this segment, in reading order.
    pub progress: Range<f32>,
}

/// A source word mapped onto shaped geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct WordGeometry {
    /// Index into `TextLayoutInput::words`.
    pub src_word: usize,
    pub src_line: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    pub segments: Vec<WordSegment>,
    /// Union of every segment, in line-box coordinates.
    pub rect: Rect,
}

/// Shaped geometry for one lyric line.
#[derive(Clone, Debug, PartialEq)]
pub struct LineLayout {
    pub font_size: f32,
    pub line_height: f32,
    pub ascent: f32,
    pub descent: f32,
    pub width: f32,
    pub height: f32,
    pub lines: Vec<VisualLine>,
    pub words: Vec<WordGeometry>,
    /// Advance of every grapheme cluster start, in line-box coordinates.
    pub pens: Vec<(usize, f32)>,
    pub src_line: usize,
}

impl LineLayout {
    pub fn line_of_byte(&self, byte: usize) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| l.byte_range.start <= byte && byte < l.byte_range.end)
            .or_else(|| self.lines.len().checked_sub(1))
    }

    /// Index of the word whose time window contains `at_ms`, if any.
    pub fn word_at(&self, at_ms: i64) -> Option<usize> {
        self.words
            .iter()
            .position(|w| w.start_ms <= at_ms && at_ms < w.end_ms)
    }
}

/// Bounded cache of shaped lines.
///
/// The key is the full layout input, so a brightness or frame-rate change cannot
/// invalidate it while a font, size, weight, DPI, width, spacing or break-mode
/// change must. Entries are evicted least-recently-used once `capacity` is
/// exceeded; a layout handed out as `Arc` stays alive in the caller's hands even
/// after eviction, so eviction never invalidates a frame in flight.
pub struct TextLayoutCache {
    text_system: Arc<TextSystem>,
    window_text_system: WindowTextSystem,
    entries: HashMap<LayoutKey, (Arc<LineLayout>, u64)>,
    capacity: usize,
    clock: u64,
    pub shaped_lines: u64,
    pub cache_hits: u64,
    pub misses: u64,
}

impl TextLayoutCache {
    pub fn new(text_system: Arc<TextSystem>, capacity: usize) -> Self {
        let window_text_system = WindowTextSystem::new(text_system.clone());
        Self {
            text_system,
            window_text_system,
            entries: HashMap::new(),
            capacity: capacity.max(1),
            clock: 0,
            shaped_lines: 0,
            cache_hits: 0,
            misses: 0,
        }
    }

    /// Install the bundled faces. Call once before the first layout.
    pub fn add_fonts(&self, fonts: Vec<std::borrow::Cow<'static, [u8]>>) -> Result<()> {
        self.text_system.add_fonts(fonts)
    }

    pub fn text_system(&self) -> &Arc<TextSystem> {
        &self.text_system
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn layout(&mut self, input: &TextLayoutInput) -> Result<Arc<LineLayout>> {
        let key = input.cache_key();
        self.clock += 1;
        if let Some((layout, used)) = self.entries.get_mut(&key) {
            *used = self.clock;
            self.cache_hits += 1;
            return Ok(layout.clone());
        }
        self.misses += 1;
        let layout = Arc::new(self.shape(input)?);
        self.shaped_lines += 1;
        self.entries.insert(key.clone(), (layout.clone(), self.clock));
        self.evict();
        Ok(layout)
    }

    fn evict(&mut self) {
        while self.entries.len() > self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    self.entries.remove(&k);
                }
                None => break,
            }
        }
    }

    fn shape(&self, input: &TextLayoutInput) -> Result<LineLayout> {
        let _ = self.text_system.all_font_names();
        let runs = build_runs(input);
        let shaped = self.window_text_system.layout_line(
            &input.text,
            px(input.font_size),
            &runs,
            None,
        );

        let mut pens: Vec<(usize, f32)> = shaped
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter())
            .map(|glyph| (glyph.index, f32::from(glyph.position.x)))
            .collect();
        pens.sort_by_key(|(byte, _)| *byte);
        pens.dedup_by_key(|(byte, _)| *byte);

        // `LineLayout::width` is the pen position after the final glyph, so it
        // is exactly the end-of-text pen without a second advance lookup.
        let total_width = f32::from(shaped.width);
        pens.push((input.text.len(), total_width));

        // Tracking is applied after shaping, per cluster, the way CSS
        // `letter-spacing` is: it never changes the glyph order or the shaping.
        let tracking = tracking_offsets(&input.text, input.font_size, input.letter_spacing_em);
        let tracking_map: HashMap<usize, f32> = tracking.iter().copied().collect();

        let measures = input
            .wrap_width
            .map(|w| w.max(input.font_size * 0.5))
            .unwrap_or(f32::INFINITY);
        let byte_lines = match input.line_break {
            LineBreakMode::Balanced => wrap_balanced(&input.text, &pens, measures),
            LineBreakMode::Natural => wrap_greedy(&input.text, &pens, measures),
        };
        let line_ranges: Vec<Range<usize>> = byte_lines;

        let line_height = input.line_height.max(input.font_size);
        let ascent = f32::from(shaped.ascent);
        let descent = f32::from(shaped.descent);
        let mut lines = Vec::with_capacity(line_ranges.len());
        for (i, range) in line_ranges.iter().enumerate() {
            let x0 = x_at_byte(&pens, range.start) + tracking_at(&tracking_map, range.start);
            let x1 = x_at_byte(&pens, range.end) + tracking_at(&tracking_map, range.end);
            let mut opportunities = crate::layout::break_opportunities(&input.text);
            opportunities.retain(|b| range.start < *b && *b < range.end);
            lines.push(VisualLine {
                byte_range: range.clone(),
                rect: Rect::new(x0, i as f32 * line_height, x1 - x0, line_height),
                opportunities: opportunities
                    .into_iter()
                    .map(|byte| BreakOpportunity {
                        byte,
                        width_bits: (x_at_byte(&pens, byte) + tracking_at(&tracking_map, byte)).to_bits(),
                    })
                    .collect(),
            });
        }
        // The first line starts at the origin; every line box is as tall as the
        // line height and its baseline sits at `ascent`.
        for (i, line) in lines.iter_mut().enumerate() {
            line.rect.y = i as f32 * line_height;
        }

        let width = lines
            .iter()
            .map(|l| l.rect.w)
            .fold(0.0f32, f32::max);
        let height = line_height * lines.len() as f32;

        let words = map_words(input, &pens, &tracking_map, &lines)?;

        Ok(LineLayout {
            font_size: input.font_size,
            line_height,
            ascent,
            descent,
            width,
            height,
            lines,
            words,
            pens,
            src_line: input.src_line,
        })
    }
}

fn tracking_at(map: &HashMap<usize, f32>, byte: usize) -> f32 {
    map.get(&byte).copied().unwrap_or_else(|| {
        // A byte offset inside the text still needs the tracking of the cluster
        // that contains it.
        map.iter()
            .filter(|(b, _)| **b <= byte)
            .max_by_key(|(b, _)| **b)
            .map(|(_, v)| *v)
            .unwrap_or(0.0)
    })
}

/// Split the line into script runs so each face shapes its own span.
fn build_runs(input: &TextLayoutInput) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut start = 0usize;
    let mut current: Option<bool> = None;
    for (byte, ch) in input.text.char_indices() {
        let cjk = is_cjk(ch) || is_wide(ch);
        match current {
            Some(prev) if prev == cjk => {}
            Some(prev) => {
                runs.push(build_run(&input.text[start..byte], prev, input));
                start = byte;
                current = Some(cjk);
            }
            None => current = Some(cjk),
        }
    }
    if let Some(prev) = current {
        runs.push(build_run(&input.text[start..], prev, input));
    }
    runs
}

fn build_run(text: &str, cjk: bool, input: &TextLayoutInput) -> TextRun {
    let family = if cjk {
        input.font.cjk_family.clone()
    } else {
        input.font.latin_family.clone()
    };
    let mut f: Font = font(family);
    f.weight = FontWeight(input.font.weight);
    f.fallbacks = Some(FontFallbacks::from_fonts(vec![if cjk {
        input.font.latin_family.clone()
    } else {
        input.font.cjk_family.clone()
    }]));
    TextRun {
        len: text.len(),
        font: f,
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

fn is_wide(ch: char) -> bool {
    matches!(ch as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF)
}

/// Map each source word onto the byte offsets it occupies, then onto geometry.
fn map_words(
    input: &TextLayoutInput,
    pens: &[(usize, f32)],
    tracking: &HashMap<usize, f32>,
    lines: &[VisualLine],
) -> Result<Vec<WordGeometry>> {
    let mut out = Vec::with_capacity(input.words.len());
    let mut cursor = 0usize;
    for (i, word) in input.words.iter().enumerate() {
        let needle = word.text.trim();
        if needle.is_empty() {
            out.push(WordGeometry {
                src_word: i,
                src_line: input.src_line,
                start_ms: word.start_ms,
                end_ms: word.end_ms,
                segments: Vec::new(),
                rect: Rect::default(),
            });
            continue;
        }
        let Some(found) = input.text[cursor..].find(needle) else {
            anyhow::bail!(
                "word {i} ({needle:?}) is not found in the shaped line {:?} after byte {cursor}",
                input.text
            );
        };
        let start = cursor + found;
        let end = start + needle.len();
        cursor = end;

        let x0 = x_at_byte(pens, start) + tracking_at(tracking, start);
        let x1 = x_at_byte(pens, end) + tracking_at(tracking, end);
        let mut segments = Vec::new();
        let touched = segments_for_range(
            &lines
                .iter()
                .map(|l| l.byte_range.clone())
                .collect::<Vec<_>>(),
            start,
            end,
        );
        let total = (x1 - x0).max(0.0);
        for (n, line_index) in touched.iter().enumerate() {
            let line = &lines[*line_index];
            let seg_start = start.max(line.byte_range.start);
            let seg_end = end.min(line.byte_range.end);
            let sx0 = x_at_byte(pens, seg_start) + tracking_at(tracking, seg_start);
            let sx1 = x_at_byte(pens, seg_end) + tracking_at(tracking, seg_end);
            let from = if total > 0.0 { (sx0 - x0) / total } else { 0.0 };
            let to = if total > 0.0 { (sx1 - x0) / total } else { 1.0 };
            segments.push(WordSegment {
                line: *line_index,
                byte_range: seg_start..seg_end,
                rect: Rect::new(
                    sx0,
                    line.rect.y,
                    (sx1 - sx0).max(0.0),
                    line.rect.h,
                ),
                progress: from.min(to)..to.max(from),
            });
            let _ = n;
        }
        out.push(WordGeometry {
            src_word: i,
            src_line: input.src_line,
            start_ms: word.start_ms,
            end_ms: word.end_ms,
            rect: Rect::new(x0, 0.0, total, lines.first().map(|l| l.rect.h).unwrap_or(0.0)),
            segments,
        });
    }
    Ok(out)
}

/// Width of every cluster in the shaped line, used by the atlas prewarm.
pub fn cluster_widths(text: &str) -> Vec<usize> {
    cluster_starts(text)
        .windows(2)
        .map(|w| w[1] - w[0])
        .chain(std::iter::once(text.len().saturating_sub(
            cluster_starts(text).last().copied().unwrap_or(text.len()),
        )))
        .collect()
}

/// Convenience for a single face with no fallback, used by unit tests.
pub fn latin_only(family: &str, weight: f32) -> FontSpec {
    FontSpec {
        latin_family: family.to_string(),
        cjk_family: family.to_string(),
        weight,
    }
}

/// Wrap `text` with an explicit shaped pen table. Exposed for the golden tests,
/// which compare wrap output without a live text system.
pub fn wrap_with_pens(
    text: &str,
    pens: &[(usize, f32)],
    width: f32,
    mode: LineBreakMode,
) -> Vec<Range<usize>> {
    match mode {
        LineBreakMode::Natural => wrap_greedy(text, pens, width),
        LineBreakMode::Balanced => wrap_balanced(text, pens, width),
    }
}

/// Shared string helper so callers do not have to import gpui just for this.
pub fn shared(text: &str) -> SharedString {
    SharedString::new(text)
}

/// Shape one line against a live text system and return the raw GPUI layout.
///
/// Only the tests use this; production goes through [`TextLayoutCache`].
pub fn shape_raw(
    text_system: &Arc<TextSystem>,
    text: &str,
    input: &TextLayoutInput,
) -> Result<Arc<gpui_kit::LineLayout>> {
    let window = WindowTextSystem::new(text_system.clone());
    let runs = build_runs(input);
    Ok(window.layout_line(text, px(input.font_size), &runs, None))
}

/// Count of clusters in `text`, the unit CSS letter-spacing advances.
pub fn cluster_count(text: &str) -> usize {
    cluster_starts(text).len()
}

/// Reads a shaped advance for a cluster from the font, in logical pixels.
pub fn cluster_advance(
    text_system: &Arc<TextSystem>,
    font: &Font,
    font_size: f32,
    ch: char,
) -> Result<f32> {
    let font_id = text_system.resolve_font(font);
    text_system
        .advance(font_id, px(font_size), ch)
        .map(|s| f32::from(s.width))
        .context("advance")
}
