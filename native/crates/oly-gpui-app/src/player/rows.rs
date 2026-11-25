//! Shaping a lyric row into the per-visual-line runs the painter submits.
//!
//! One row is shaped once per width and memoised: a whole song must not be
//! re-shaped every frame. The shaping itself goes through
//! `oly_render::text::shape_raw`, which builds the same CJK/Latin run split the
//! reference browser produced, and the wrap decisions come from
//! `oly_render::layout` so balanced and natural wrapping stay identical to the
//! records in `FIDELITY.md`.
//!
//! The result is one `gpui_kit::LineLayout` per *visual* line (rebased to that
//! line's own origin, which is what `split_at` does) plus the word boxes the
//! karaoke mask needs.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui_kit::TextSystem;
use oly_render::layout::{LineBreakMode, Rect as LayoutRect, SourceWord, tracking_offsets, wrap_balanced, wrap_greedy};
use oly_render::text::{FontSpec, shape_raw};

use super::model::{Display, Metrics, Row, row_height, roman_height, roman_metrics};
use super::paint::{RawLine, WordBox, word_boxes};

/// Where each cluster's extra advance starts, as `(byte, extra_px)`.
///
/// `cluster_advance` reports a per-cluster advance that CSS adds *after* the
/// cluster; the table is monotonic so `pen_at`-style lookups stay cheap.
pub type Tracking = Vec<(usize, f32)>;

/// One row, shaped and ready to paint.
pub struct RowLines {
    /// One per visual line, in reading order.
    pub lines: Vec<RawLine>,
    /// Word boxes in line-local coordinates.
    pub words: Vec<WordBox>,
    /// Extra advance per cluster, applied to both glyphs and word boxes.
    pub tracking: Tracking,
    pub masks: Vec<Vec<oly_render::mask::MaskKeyframe>>,
    pub emphasis: Vec<Option<oly_render::emphasis::EmphasisPlan>>,
    pub char_starts: Vec<Vec<usize>>,
    pub line_start_ms: f64,
    pub word_timed: bool,
    pub is_bg: bool,
    pub word_fade: f32,
    /// The (already shaped) translation, when the row has one.
    pub trans: Option<Arc<gpui_kit::LineLayout>>,
    pub trans_lines: Vec<Arc<gpui_kit::LineLayout>>,
    pub trans_font: f32,
    pub trans_gap: f32,
    /// Romanisation is laid out immediately after the translation.
    pub roman: Option<Arc<gpui_kit::LineLayout>>,
    pub roman_lines: Vec<Arc<gpui_kit::LineLayout>>,
    pub roman_font: f32,
    pub roman_line_height: f32,
    pub trans_line_height: f32,
    pub font_size: f32,
    pub line_height: f32,
    /// Row height including translation and the configured line gap.
    pub height: f32,
}

impl RowLines {
    pub fn trans_height(&self) -> f32 {
        self.trans_line_height * self.trans_lines.len() as f32
    }

    pub fn roman_height(&self) -> f32 {
        self.roman_line_height * self.roman_lines.len() as f32
    }
}

/// Shaped bottom credits line, ready to paint.
#[derive(Clone)]
pub struct BottomLines {
    pub lines: Vec<Arc<gpui_kit::LineLayout>>,
    pub font_size: f32,
    pub line_height: f32,
    pub height: f32,
}

/// Shaped-row cache keyed on everything that changes the glyphs.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key(u64);

/// Shapes rows and remembers them.
pub struct Shaper {
    text_system: Arc<TextSystem>,
    memo: HashMap<Key, Arc<RowLines>>,
    bottom_memo: HashMap<Key, Arc<BottomLines>>,
    order: VecDeque<Key>,
    capacity: usize,
    pub shaped_rows: u64,
    pub hits: u64,
    pub misses: u64,
}

impl Shaper {
    pub fn new(text_system: Arc<TextSystem>, capacity: usize) -> Self {
        Self {
            text_system,
            memo: HashMap::new(),
            bottom_memo: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(8),
            shaped_rows: 0,
            hits: 0,
            misses: 0,
        }
    }

    pub fn text_system(&self) -> &Arc<TextSystem> {
        &self.text_system
    }

    pub fn len(&self) -> usize {
        self.memo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.memo.is_empty()
    }

    /// Drop every shaped row; used when the wrap width or font changes.
    pub fn clear(&mut self) {
        self.memo.clear();
        self.bottom_memo.clear();
        self.order.clear();
    }

    pub fn bottom_line(
        &mut self,
        text: &str,
        m: &Metrics,
        d: &Display,
        wrap_width: f32,
    ) -> Result<Option<Arc<BottomLines>>> {
        if text.trim().is_empty() {
            return Ok(None);
        }
        let font_size = (m.font * 0.66).clamp(13.0, 34.0);
        let line_height = font_size * 1.55;
        let wrap_width = wrap_width.max(font_size * 0.5);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        font_size.to_bits().hash(&mut hasher);
        line_height.to_bits().hash(&mut hasher);
        wrap_width.to_bits().hash(&mut hasher);
        d.font_family.hash(&mut hasher);
        d.weight.to_bits().hash(&mut hasher);
        d.line_gap.to_bits().hash(&mut hasher);
        let key = Key(hasher.finish());
        if let Some(hit) = self.bottom_memo.get(&key) {
            return Ok(Some(hit.clone()));
        }
        let input = oly_render::text::TextLayoutInput {
            text: text.to_string(),
            words: Vec::new(),
            font: font_spec(d.font_family, d.weight),
            font_size,
            letter_spacing_em: 0.0,
            wrap_width: Some(wrap_width),
            line_break: LineBreakMode::Natural,
            line_height,
            src_line: 0,
        };
        let lines = shape_subline(&self.text_system, &input, wrap_width)?;
        let height = lines.len().max(1) as f32 * line_height
            + super::model::row_padding(m, d, false) * 2.0;
        let built = Arc::new(BottomLines {
            lines,
            font_size,
            line_height,
            height,
        });
        if self.bottom_memo.len() > 16 {
            self.bottom_memo.clear();
        }
        self.bottom_memo.insert(key, built.clone());
        Ok(Some(built))
    }

    /// Shape one row for the current metrics.
    ///
    /// `wrap_width` is the lyric column's content width in the same px space as
    /// `Metrics`; the mask geometry uses the row's own font size, so a
    /// background row scaled to 70 % keeps its own baseline and band width.
    pub fn row(
        &mut self,
        row: &Row,
        m: &Metrics,
        d: &Display,
        wrap_width: f32,
        key_extra: u64,
    ) -> Result<Arc<RowLines>> {
        let key = Key(hash_row(row, m, d, wrap_width, key_extra));
        if let Some(hit) = self.memo.get(&key) {
            self.hits += 1;
            return Ok(hit.clone());
        }
        self.misses += 1;
        let shaped = Arc::new(self.shape(row, m, d, wrap_width)?);
        self.shaped_rows += 1;
        if self.memo.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.memo.remove(&oldest);
            }
        }
        self.memo.insert(key, shaped.clone());
        self.order.push_back(key);
        Ok(shaped)
    }

    fn shape(
        &mut self,
        row: &Row,
        m: &Metrics,
        d: &Display,
        wrap_width: f32,
    ) -> Result<RowLines> {
        let scale = if row.is_bg { d.bg_scale } else { 1.0 };
        let font_size = (m.font * scale).max(8.0);
        let line_height = (m.line_height * scale).max(font_size);
        let wrap_width = wrap_width.max(font_size * 0.5);

        let text = row.text.clone();
        let input = oly_render::text::TextLayoutInput {
            text: text.clone(),
            words: row
                .words
                .iter()
                .map(|(t, s, e)| SourceWord {
                    text: t.clone(),
                    start_ms: *s,
                    end_ms: *e,
                })
                .collect(),
            font: font_spec(d.font_family, d.weight),
            font_size,
            // Tracking is applied when painting and when measuring word boxes,
            // not in the shaped layout: `paint_glyph` takes the glyph's own
            // position, so the same table has to drive both sides.
            letter_spacing_em: 0.0,
            wrap_width: Some(wrap_width),
            line_break: if d.balanced {
                LineBreakMode::Balanced
            } else {
                LineBreakMode::Natural
            },
            line_height,
            src_line: row.group,
        };
        let raw = shape_raw(&self.text_system, &text, &input)?;

        let tracking = tracking_offsets(&text, font_size, d.letter_spacing_em);
        let track_at = |byte: usize| -> f32 { tracking_at(&tracking, byte) };

        // Pen table in the unwrapped text, with tracking folded in so the wrap
        // decisions and the mask boxes agree with what is actually drawn.
        let mut pens: Vec<(usize, f32)> = raw
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|g| (g.index, f32::from(g.position.x))))
            .collect();
        pens.sort_by_key(|(byte, _)| *byte);
        pens.dedup_by_key(|(byte, _)| *byte);
        for pen in pens.iter_mut() {
            pen.1 += track_at(pen.0);
        }
        if let Some(last) = pens.last() {
            if last.0 != text.len() {
                pens.push((text.len(), f32::from(raw.width) + track_at(text.len())));
            }
        }

        let ranges: Vec<Range<usize>> = if raw.width <= gpui_kit::px(wrap_width) && !text.is_empty() {
            vec![0..text.len()]
        } else if d.balanced {
            wrap_balanced(&text, &pens, wrap_width)
        } else {
            wrap_greedy(&text, &pens, wrap_width)
        };

        // Split the shaped line at each wrap point. `split_at` rebases the
        // right-hand side, so splitting the remainder repeatedly yields one
        // layout per visual line whose glyph indices start at zero.
        let mut rest = match Arc::try_unwrap(raw) {
            Ok(layout) => layout,
            Err(shared) => shared.split_at(text.len()).0,
        };
        let mut lines = Vec::with_capacity(ranges.len());
        for (i, range) in ranges.iter().enumerate() {
            let len = range.end - range.start;
            let layout = if i + 1 == ranges.len() {
                std::mem::take(&mut rest)
            } else {
                let (left, right) = rest.split_at(len.min(text.len()));
                rest = right;
                left
            };
            lines.push(RawLine {
                layout: Arc::new(layout),
                start: range.start,
                end: range.end,
            });
        }

        let line_rects: Vec<(Range<usize>, LayoutRect)> = ranges
            .iter()
            .enumerate()
            .map(|(i, range)| {
                let x = pen_x(&pens, range.start);
                let y = i as f32 * line_height;
                (range.clone(), LayoutRect::new(x, y, 0.0, line_height))
            })
            .collect();
        let words = word_boxes(&text, &row.words, &pens, &line_rects, |_| 0.0);

        let mask_words: Vec<_> = row.words.iter().enumerate().map(|(i, (_, start, end))| oly_render::mask::MaskWord {
            start_ms: *start, end_ms: *end,
            width: words.iter().find(|w| w.source == i).map(|w| w.total_w).unwrap_or(0.0),
            padding: font_size,
        }).collect();
        let masks = words.iter().map(|w| oly_render::mask::word_keyframes(
            &mask_words, w.source, line_height*d.word_fade, row.start_ms as i64, row.duration_ms()
        )).collect();
        let emphasis = row.words.iter().enumerate().map(|(i, (text, start, end))| {
            if !row.word_timed { return None; }
            oly_render::emphasis::EmphasisPlan::new_with_threshold(text.trim(), *start, *end, row.start_ms as i64, row.is_bg, i+1 == row.words.len(), d.long_syllable)
        }).collect();
        let char_starts = row.words.iter().map(|(text, _, _)| {
            let mut byte = 0;
            oly_render::emphasis::split_graphemes(text.trim()).iter().map(|c| { let start=byte; byte += c.len(); start }).collect()
        }).collect();
        let (trans_lines, trans_font, trans_line_height) =
            if d.show_translation && !row.translation.is_empty() {
                let trans_font = (m.trans_font * scale).max(8.0);
                let trans_line_height = (m.trans_height * scale).max(trans_font);
                let tinput = oly_render::text::TextLayoutInput {
                    text: row.translation.clone(),
                    words: Vec::new(),
                    font: font_spec(d.trans_font_family, d.trans_weight),
                    font_size: trans_font,
                    letter_spacing_em: 0.0,
                    wrap_width: Some(wrap_width),
                    line_break: LineBreakMode::Natural,
                    line_height: trans_line_height,
                    src_line: row.group,
                };
                let layouts = shape_subline(&self.text_system, &tinput, wrap_width)?;
                (layouts, trans_font, trans_line_height)
            } else {
                (Vec::new(), 0.0, 0.0)
            };

        let (roman_lines, roman_font, roman_line_height) = if d.show_romanization && !row.romanization.is_empty() {
            let (font_size, line_height) = roman_metrics(m,d,row.is_bg);
            let input = oly_render::text::TextLayoutInput {
                text: row.romanization.clone(), words: Vec::new(),
                font: font_spec(d.font_family,d.weight), font_size,
                letter_spacing_em: 0.0, wrap_width: Some(wrap_width),
                line_break: LineBreakMode::Natural, line_height, src_line: row.group,
            };
            (shape_subline(&self.text_system,&input,wrap_width)?,font_size,line_height)
        } else { (Vec::new(),0.0,0.0) };
        let trans = trans_lines.first().cloned();
        let roman = roman_lines.first().cloned();
        let height = row_height(lines.len(), trans.is_some(), m, d, row.is_bg)
            + roman_height(roman.is_some(),m,d,row.is_bg)
            + trans_lines.len().saturating_sub(1) as f32 * trans_line_height
            + roman_lines.len().saturating_sub(1) as f32 * roman_line_height;
        Ok(RowLines {
            lines,
            words,
            tracking, masks, emphasis, char_starts,
            line_start_ms: row.start_ms, word_timed: row.word_timed, is_bg: row.is_bg, word_fade: d.word_fade,
            trans,
            trans_lines,
            trans_font,
            trans_gap: trans_font * d.inner_gap,
            trans_line_height,
            roman, roman_lines, roman_font, roman_line_height,
            font_size,
            line_height,
            height,
        })
    }
}

/// The same natural wrap policy applies to translations and romanisation.
fn subline_ranges(text: &str, pens: &[(usize, f32)], wrap_width: f32) -> Vec<Range<usize>> {
    wrap_greedy(text,pens,wrap_width)
}

fn shape_subline(text_system: &Arc<TextSystem>, input: &oly_render::text::TextLayoutInput,
    wrap_width: f32) -> Result<Vec<Arc<gpui_kit::LineLayout>>> {
    let raw = shape_raw(text_system,&input.text,input)?;
    let mut pens: Vec<_> = raw.runs.iter().flat_map(|run| run.glyphs.iter()
        .map(|g| (g.index,f32::from(g.position.x)))).collect();
    pens.sort_by_key(|(byte,_)| *byte);
    pens.dedup_by_key(|(byte,_)| *byte);
    if pens.last().is_none_or(|(byte,_)| *byte != input.text.len()) {
        pens.push((input.text.len(),f32::from(raw.width)));
    }
    let ranges = subline_ranges(&input.text,&pens,wrap_width);
    let mut rest = match Arc::try_unwrap(raw) {
        Ok(layout) => layout,
        Err(shared) => shared.split_at(input.text.len()).0,
    };
    let mut lines = Vec::with_capacity(ranges.len());
    for (index,range) in ranges.iter().enumerate() {
        let layout = if index + 1 == ranges.len() { std::mem::take(&mut rest) } else {
            let (left,right) = rest.split_at(range.end-range.start);
            rest = right;
            left
        };
        lines.push(Arc::new(layout));
    }
    Ok(lines)
}

/// Match the legacy fontForFamily stack: bundled family aliases share the same
/// Latin/CJK split; a system font takes priority for the entire line.
fn font_spec(family: &str, weight: f32) -> FontSpec {
    if matches!(family, "Pretendard" | "檎风黑体 CHS") {
        FontSpec { latin_family: "Pretendard".into(), cjk_family: "檎风黑体 CHS".into(), weight }
    } else {
        FontSpec { latin_family: family.into(), cjk_family: family.into(), weight }
    }
}

/// Extra advance accumulated at `byte`.
pub fn tracking_at(tracking: &[(usize, f32)], byte: usize) -> f32 {
    let idx = tracking.partition_point(|(at, _)| *at <= byte);
    if idx == 0 {
        0.0
    } else {
        tracking[idx - 1].1
    }
}

fn pen_x(pens: &[(usize, f32)], byte: usize) -> f32 {
    let idx = pens.partition_point(|(at, _)| *at <= byte);
    if idx == 0 {
        0.0
    } else {
        pens[idx - 1].1
    }
}

fn hash_row(row: &Row, m: &Metrics, d: &Display, wrap_width: f32, extra: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    row.text.hash(&mut hasher);
    row.start_ms.to_bits().hash(&mut hasher);
    row.end_ms.to_bits().hash(&mut hasher);
    d.word_fade.to_bits().hash(&mut hasher);
    d.long_syllable.hash(&mut hasher);
    row.word_timed.hash(&mut hasher);
    row.translation.hash(&mut hasher);
    row.romanization.hash(&mut hasher);
    d.show_romanization.hash(&mut hasher);
    d.roman_font_scale.to_bits().hash(&mut hasher);
    row.is_bg.hash(&mut hasher);
    row.words.len().hash(&mut hasher);
    for (text, start, end) in &row.words {
        text.hash(&mut hasher);
        start.hash(&mut hasher);
        end.hash(&mut hasher);
    }
    m.font.to_bits().hash(&mut hasher);
    m.line_height.to_bits().hash(&mut hasher);
    m.trans_font.to_bits().hash(&mut hasher);
    m.trans_height.to_bits().hash(&mut hasher);
    wrap_width.to_bits().hash(&mut hasher);
    row.group.hash(&mut hasher);
    d.font_family.hash(&mut hasher);
    d.trans_font_family.hash(&mut hasher);
    d.line_gap.to_bits().hash(&mut hasher);
    d.inner_gap.to_bits().hash(&mut hasher);
    d.line_width_aspect.to_bits().hash(&mut hasher);
    d.line_padding_x.map(f32::to_bits).hash(&mut hasher);
    d.weight.to_bits().hash(&mut hasher);
    d.trans_weight.to_bits().hash(&mut hasher);
    d.letter_spacing_em.to_bits().hash(&mut hasher);
    d.balanced.hash(&mut hasher);
    d.show_translation.hash(&mut hasher);
    d.bg_scale.to_bits().hash(&mut hasher);
    extra.hash(&mut hasher);
    hasher.finish()
}

/// Build a shaped row without the cache, for tests and one-shot probes.
pub fn shape_once(
    text_system: &Arc<TextSystem>,
    row: &Row,
    m: &Metrics,
    d: &Display,
    wrap_width: f32,
) -> Result<Arc<RowLines>> {
    let mut shaper = Shaper::new(text_system.clone(), 4);
    Ok(Arc::new(shaper.shape(row, m, d, wrap_width)?))
}

/// A row that has always to exist: `Shaper::row` needs a `Metrics`, and the
/// caller builds it from the window size, so this keeps the error surface tiny.
pub fn require_font(text_system: &Arc<TextSystem>, family: &str) -> Result<()> {
    let names = text_system.all_font_names();
    if !names.iter().any(|name| name == family) {
        return Err(anyhow!("font family {family:?} is not registered"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_sublines_wrap_with_complete_utf8_coverage_instead_of_clipping() {
        for text in ["alpha beta gamma delta", "这是较长的中文歌词译文"] {
            let mut pens: Vec<_> = text.char_indices().enumerate().map(|(index,(byte,_))| (byte,index as f32*10.0)).collect();
            pens.push((text.len(),text.chars().count() as f32*10.0));
            let lines = subline_ranges(text,&pens,100.0);
            assert!(lines.len()>1);
            let reconstructed: String = lines.iter().map(|range| &text[range.clone()]).collect();
            assert_eq!(reconstructed,text);
            for range in lines {
                assert!(pen_x(&pens,range.end)-pen_x(&pens,range.start)<=100.0);
            }
        }
    }

    #[test]
    fn selected_font_changes_shaping_and_cached_geometry() {
        let mut d = Display::default();
        let m = super::super::model::metrics(1272.0,843.0,(1920.0,1080.0),false,&d);
        let r = Row { text: "word".into(), words: vec![("word".into(), 1000, 3000)], start_ms: 1000.0, end_ms: 3000.0, is_bg: false, is_duet: false, word_timed: true, group: 0, translation: "译".into(), romanization: "yi".into() };
        let a = hash_row(&r,&m,&d,500.0,0);
        d.font_family = "宋体";
        let b = hash_row(&r,&m,&d,500.0,0);
        d.trans_font_family = "楷体";
        let c = hash_row(&r,&m,&d,500.0,0);
        assert_ne!(a,b); assert_ne!(b,c);
        let spec = font_spec(d.font_family, d.weight);
        assert_eq!((spec.latin_family.as_str(),spec.cjk_family.as_str()), ("宋体","宋体"));
    }

    #[test]
    fn changing_em_gaps_invalidates_row_height_cache() {
        let mut d = Display::default();
        let m = super::super::model::metrics(1272.0,843.0,(1920.0,1080.0),false,&d);
        let r = Row { text: "word".into(), words: vec![("word".into(), 1000, 3000)], start_ms: 1000.0, end_ms: 3000.0, is_bg: false, is_duet: false, word_timed: true, group: 0, translation: "译".into(), romanization: "yi".into() };
        let a = hash_row(&r,&m,&d,500.0,0);
        d.line_gap += 0.1;
        let b = hash_row(&r,&m,&d,500.0,0);
        d.inner_gap += 0.1;
        let c = hash_row(&r,&m,&d,500.0,0);
        d.show_romanization = !d.show_romanization;
        let roman_toggle = hash_row(&r,&m,&d,500.0,0);
        d.roman_font_scale += 0.1;
        let roman_size = hash_row(&r,&m,&d,500.0,0);
        assert_ne!(a,b); assert_ne!(b,c);
        assert_ne!(c,roman_toggle); assert_ne!(roman_toggle,roman_size);
    }

    #[test]
    fn tracking_accumulates_up_to_the_byte() {
        let table = vec![(0usize, 0.0f32), (1, 0.5), (2, 1.0)];
        assert_eq!(tracking_at(&table, 0), 0.0);
        assert_eq!(tracking_at(&table, 1), 0.5);
        assert_eq!(tracking_at(&table, 5), 1.0);
    }

    #[test]
    fn a_pen_lookup_clamps_past_the_last_entry() {
        let pens = vec![(0usize, 0.0f32), (4, 12.0)];
        assert_eq!(pen_x(&pens, 0), 0.0);
        assert_eq!(pen_x(&pens, 2), 0.0);
        assert_eq!(pen_x(&pens, 4), 12.0);
        assert_eq!(pen_x(&pens, 99), 12.0);
    }
    #[test]
    fn timing_and_mask_width_invalidate_cached_animation_plans() {
        let mut d = Display::default();
        let m = super::super::model::metrics(1272.0,843.0,(1920.0,1080.0),false,&d);
        let mut r = Row { text: "word".into(), words: vec![("word".into(), 1000, 3000)], start_ms: 1000.0, end_ms: 3000.0, is_bg: false, is_duet: false, word_timed: true, group: 0, translation: String::new(), romanization: String::new() };
        let a = hash_row(&r,&m,&d,500.0,0);
        r.start_ms = 900.0;
        let b = hash_row(&r,&m,&d,500.0,0);
        r.end_ms = 3100.0;
        let c = hash_row(&r,&m,&d,500.0,0);
        d.word_fade = 0.8;
        let e = hash_row(&r,&m,&d,500.0,0);
        assert_ne!(a,b); assert_ne!(b,c); assert_ne!(c,e);
    }

}
