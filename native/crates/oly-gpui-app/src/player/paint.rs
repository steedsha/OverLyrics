//! Glyph-atlas lyrics: pixel masks, cached glyph shadows and GPU-only motion.
use gpui_kit::{px, point, Hsla, Window};
use oly_render::layout::Rect as LayoutRect;
use oly_render::mask::{alpha_at_x, MaskSpec};
use super::rows::{RowLines, tracking_at};

/// Normalized line time expected by the mask keyframes (never milliseconds).
pub fn line_progress(playhead: f64, start: f64, duration: f64) -> f32 {
    ((playhead-start) / duration.max(1.0)).clamp(0.0, 1.0) as f32
}

/// Straight sRGB #f4f6f8, the reference `--text`.
pub const TEXT_RGB: u32 = 0xF4F6F8;

/// One word's box on one visual line, in line-local coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordBox {
    /// Visual line the box belongs to.
    pub line: usize,
    pub source: usize,
    pub source_start: usize,
    pub fragment_offset: f32,
    pub total_w: f32,
    /// Left edge, relative to the visual line's own origin.
    pub x: f32,
    /// Shaped advance of the word inside its box.
    pub w: f32,
    /// UTF-8 range inside the row's full text.
    pub start: usize,
    pub end: usize,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl WordBox {
    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

/// One visual line as GPUI shaped it, plus the byte range it covers.
pub struct RawLine {
    pub layout: std::sync::Arc<gpui_kit::LineLayout>,
    pub start: usize,
    pub end: usize,
}

/// How a row is lit this frame.
pub struct RowLight {
    /// Alpha for every non-karaoke glyph (unplayed, sung, idle).
    pub base: f32,
    /// Alpha for the karaoke band's trailing edge.
    pub dark: f32,
    /// Alpha for the karaoke band's leading edge.
    pub bright: f32,
    /// Per-word mask positions in pixels, parallel to the row's word boxes.
    /// `None` leaves the row uniform (not the active row).
    pub mask: Option<Vec<f32>>,
    /// Per-word glow alpha, parallel to the word boxes.
    pub glow: Option<Vec<f32>>,
}

impl RowLight {
    pub fn uniform(alpha: f32) -> Self {
        Self {
            base: alpha,
            dark: alpha,
            bright: alpha,
            mask: None,
            glow: None,
        }
    }
}

/// A vertical feather factor in `[0, 1]`: 0 outside the band, 1 well inside it.
///
/// The reference's `mask-image: linear-gradient(transparent, black 64px, ...)`
/// has exactly this shape: a straight ramp over `feather` pixels at each edge
/// and a flat middle.
pub fn feather(y: f32, top: f32, bottom: f32, feather: f32) -> f32 {
    if bottom <= top {
        return 0.0;
    }
    let f = feather.max(1.0);
    let enter = ((y - top) / f).clamp(0.0, 1.0);
    let leave = ((bottom - y) / f).clamp(0.0, 1.0);
    enter.min(leave)
}

/// Alpha for a glyph at `x` inside a word box, using the recorded mask geometry.
///
/// `padding` is one em a side, the same box the reference's
/// `--amll-lp-word-padding` produces; `fade_width` is `lineHeight * 0.5` because
/// `wordFadeWidth` is `0.5` in the frozen profile.
///
/// Callers pass `x_in_box` measured from the word box's own left edge, i.e. the
/// text's left edge plus one em of padding. Measuring from the glyph instead
/// would start the wipe one em early.
pub fn word_alpha(
    word: &WordBox,
    mask_position: f32,
    x_in_box: f32,
    dark: f32,
    bright: f32,
    font_size: f32,
    line_height: f32,
) -> f32 {
    let spec = MaskSpec::new(word.total_w, font_size, line_height, 0.5);
    alpha_at_x(&spec, mask_position, x_in_box, dark, bright)
}

/// Optional outer wrapper scale/pivot used by AMLL's `.bgWrapper` (`0.8 -> 1.0`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WrapperTransform {
    pub scale: f32,
    pub pivot_x: f32,
    pub pivot_y: f32,
}

impl Default for WrapperTransform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            pivot_x: 0.0,
            pivot_y: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineAlign {
    Left,
    Center,
    Right,
}

impl LineAlign {
    pub fn offset(self, avail: f32, line_w: f32) -> f32 {
        let slack = (avail - line_w).max(0.0);
        match self {
            Self::Left => 0.0,
            Self::Center => slack * 0.5,
            Self::Right => slack,
        }
    }

    pub fn pivot_x(self, x: f32, w: f32) -> f32 {
        match self {
            Self::Left => x,
            Self::Center => x + w * 0.5,
            Self::Right => x + w,
        }
    }
}

/// Paint one row's glyphs.
///
/// `origin` is the row's top-left on screen. `line_height` and `font_size` come
/// from the row's own layout so a background row scaled to 70 % still lands on
/// its own baseline.
pub fn paint_row(
    window: &mut Window,
    shaped: &RowLines,
    origin_x: f32,
    origin_y: f32,
    line_height: f32,
    font_size: f32,
    words: &[WordBox],
    light: &RowLight,
    playhead_ms: f64,
    line_scale: f32,
    wrapper: WrapperTransform,
    feather_top: f32,
    feather_bottom: f32,
    feather_px: f32,
    wrap_width: f32,
    align: LineAlign,
    row_blur: f32,
) {
    paint_row_with_shadow(
        window,
        shaped,
        origin_x,
        origin_y,
        line_height,
        font_size,
        words,
        light,
        playhead_ms,
        line_scale,
        wrapper,
        feather_top,
        feather_bottom,
        feather_px,
        wrap_width,
        align,
        row_blur,
        0.0,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
pub fn paint_row_with_shadow(
    window: &mut Window,
    shaped: &RowLines,
    origin_x: f32,
    origin_y: f32,
    line_height: f32,
    font_size: f32,
    words: &[WordBox],
    light: &RowLight,
    playhead_ms: f64,
    line_scale: f32,
    wrapper: WrapperTransform,
    feather_top: f32,
    feather_bottom: f32,
    feather_px: f32,
    wrap_width: f32,
    align: LineAlign,
    row_blur: f32,
    drop_shadow: f32,
    custom_rgb: Option<u32>,
) {
    use oly_render::emphasis::{sample_glow, sample_float, PlainFloat};
    let row_lines = &shaped.lines;
    let scale_factor = window.scale_factor();
    let elapsed_line = playhead_ms - shaped.line_start_ms;
    let emphasis_alive = shaped.word_timed
        && elapsed_line >= 0.0
        && shaped.emphasis.iter().any(|opt| {
            opt.as_ref().is_some_and(|plan| {
                plan.chars.iter().any(|ch| {
                    let end_glow = ch.word_delay + plan.glow_duration;
                    let end_float = plan.float_delay(ch) + plan.float_duration;
                    elapsed_line <= end_glow.max(end_float) + 20.0
                })
            })
        });
    let allow_emphasis = light.mask.is_some() || emphasis_alive;
    // Shadows first so their coverage never washes out the sharp glyph cores.
    for shadow in [true, false] {
        if shadow && (!allow_emphasis || !shaped.word_timed || shaped.emphasis.iter().all(Option::is_none)) { continue; }
        for (li, raw) in row_lines.iter().enumerate() {
            let line_width=f32::from(raw.layout.width)+tracking_at(&shaped.tracking,raw.end)-tracking_at(&shaped.tracking,raw.start);
            let origin_x = origin_x + align.offset(wrap_width, line_width);
            let line_top = origin_y + li as f32 * line_height;
            let baseline = line_top + f32::from(raw.layout.ascent);
            for run in &raw.layout.runs {
                for glyph in &run.glyphs {
                    let byte = raw.start + glyph.index;
                    let local_x = f32::from(glyph.position.x)
                        + tracking_at(&shaped.tracking, byte) - tracking_at(&shaped.tracking, raw.start);
                    let owner = words.iter().enumerate().find(|(_, w)| w.line == li && w.start <= byte && byte < w.end);
                    let mut scale = 1.0f32;
                    let (mut dx, mut dy, mut glow, mut blur) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                    let active = light.mask.is_some();
                    if allow_emphasis && shaped.word_timed {
                        if let Some((_, word)) = owner {
                            let elapsed = elapsed_line;
                            if let Some(plan) = &shaped.emphasis[word.source] {
                                let relative = byte.saturating_sub(word.source_start);
                                let ci = shaped.char_starts[word.source].partition_point(|v| *v <= relative).saturating_sub(1);
                                if let Some(char) = plan.chars.get(ci) {
                                    let glow_t = (elapsed - char.word_delay) / plan.glow_duration.max(1.0);
                                    let float_t = (elapsed - plan.float_delay(char)) / plan.float_duration.max(1.0);
                                    let lift = if float_t > 0.0 {
                                        sample_float(&char.float, float_t.clamp(0.0, 1.0))
                                    } else {
                                        0.0
                                    };
                                    if glow_t > 0.0 {
                                        let g = sample_glow(&char.glow, glow_t.clamp(0.0, 1.0));
                                        let ramp = char.glow.first().map_or(1.0, |first| {
                                            if first.offset > 0.0 && glow_t < first.offset {
                                                (glow_t / first.offset).clamp(0.0, 1.0)
                                            } else {
                                                1.0
                                            }
                                        }) as f32;
                                        scale = 1.0 + (g.scale as f32 - 1.0) * ramp;
                                        dx = g.translate_x_em as f32 * font_size * ramp;
                                        dy = (g.translate_y_em * ramp as f64 + lift) as f32 * font_size;
                                        glow = g.shadow_alpha as f32 * ramp * if active { 1.0 } else { light.bright.max(light.dark).clamp(0.0, 1.0) };
                                        blur = g.shadow_blur_em as f32 * font_size;
                                    } else {
                                        dy = lift as f32 * font_size;
                                    }
                                }
                            } else if active {
                                let lift = PlainFloat::new(word.start_ms as f64, word.end_ms as f64, shaped.line_start_ms, shaped.is_bg);
                                dy = lift.lift_at((elapsed-lift.delay_ms)/lift.duration_ms) as f32 * font_size;
                            }
                        }
                    }
                    if shadow && (glow <= 0.002 || blur <= 0.0) { continue; }
                    let mut dark = light.base;
                    let mut bright = light.base;
                    let mut band = 0.0;
                    let mut fade_width = 1.0;
                    if !shadow {
                        if let (Some(masks), Some((wi, word))) = (&light.mask, owner) {
                            if shaped.word_timed {
                                let spec = MaskSpec::new(word.total_w, font_size, line_height, shaped.word_fade);
                                band = origin_x + word.x - word.fragment_offset - font_size
                                    + masks[wi] + spec.left_percent()/100.0 * spec.total_aspect()*spec.total_width();
                                fade_width = spec.fade_width;
                                dark = light.dark;
                                bright = light.bright;
                            } else {
                                dark = light.bright;
                                bright = light.bright;
                            }
                        }
                    } else { dark = glow; bright = glow; }
                    let x = origin_x + local_x;
                    let y = baseline + f32::from(glyph.position.y);
                    let cx = x + font_size * 0.25;
                    let cy = line_top + line_height * 0.5;
                    let total_scale = scale * line_scale * wrapper.scale;
                    let line_px = align.pivot_x(origin_x, line_width);
                    let line_py = origin_y + shaped.height * 0.5;
                    let tx = (1.0 - wrapper.scale) * wrapper.pivot_x
                        + wrapper.scale * ((1.0 - line_scale) * line_px + line_scale * ((1.0 - scale) * cx + dx));
                    let ty = (1.0 - wrapper.scale) * wrapper.pivot_y
                        + wrapper.scale * ((1.0 - line_scale) * line_py + line_scale * ((1.0 - scale) * cy + dy));
                    let transform = gpui_kit::TransformationMatrix {
                        rotation_scale: [[total_scale, 0.0], [0.0, total_scale]],
                        translation: [tx*scale_factor, ty*scale_factor],
                    };
                    let text_rgb = custom_rgb.unwrap_or_else(super::chrome::lyric_text_rgb);
                    let r_c = ((text_rgb >> 16) & 0xFF) as f32 / 255.0;
                    let g_c = ((text_rgb >> 8) & 0xFF) as f32 / 255.0;
                    let b_c = (text_rgb & 0xFF) as f32 / 255.0;
                    let text_luma = 0.2126 * r_c + 0.7152 * g_c + 0.0722 * b_c;
                    let dark_text = text_luma < 0.35;
                    let (dark_out, bright_out) = if super::chrome::is_light_theme() {
                        (
                            if dark > 0.01 { (dark * 1.45).clamp(0.32, 1.0) } else { dark },
                            if bright > 0.01 { (bright * 1.45).clamp(0.32, 1.0) } else { bright },
                        )
                    } else {
                        (dark, bright)
                    };
                    let mask = [1.0, (band*total_scale+tx)*scale_factor, fade_width*total_scale*scale_factor, dark_out];
                    let feather = [bright_out, feather_top*scale_factor, feather_bottom*scale_factor, feather_px*scale_factor];
                    #[cfg(target_os = "windows")]
                    {
                        if !shadow && drop_shadow > 0.001 {
                            let sa = if dark_text {
                                (drop_shadow * 0.22 * bright_out.max(dark_out)).clamp(0.0, 0.22)
                            } else {
                                (drop_shadow * bright_out.max(dark_out)).clamp(0.0, 0.85)
                            };
                            if sa > 0.01 {
                                let mut st = transform;
                                st.translation[1] += if dark_text { 1.0 * scale_factor } else { 1.5 * scale_factor };
                                let _ = window.paint_lyric_glyph(
                                    point(px(x), px(y)),
                                    run.font_id,
                                    glyph.id,
                                    px(font_size),
                                    Hsla::from(gpui_kit::rgb(0x000000)).opacity(sa),
                                    st,
                                    [0.0; 4],
                                    [0.0; 4],
                                    2,
                                );
                            }
                        }
                        if let Err(error) = window.paint_lyric_glyph(
                            point(px(x), px(y)), run.font_id, glyph.id, px(font_size),
                            Hsla::from(gpui_kit::rgb(text_rgb)), transform, mask, feather,
                            if shadow { (blur*scale_factor*1.5).ceil().clamp(1.0,64.0) as u16 } else { (row_blur*scale_factor*1.5).ceil().clamp(0.0,32.0) as u16 },
                        ) { log::warn!("lyric glyph: {error:#}"); }
                    }
                    #[cfg(not(target_os = "windows"))]
                    { let _ = window.paint_glyph(point(px(x+dx), px(y+dy)), run.font_id, glyph.id, px(font_size), Hsla::from(gpui_kit::rgb(text_rgb)).opacity(dark_out)); }
                }
            }
        }
    }
}

/// Builds per-frame `RowLight` (base/dark/bright + word-sweep mask + scaled glow)
/// for a shaped lyric row, shared by main window and desktop lyrics.
pub fn build_row_light(
    row: &super::model::Row,
    shaped: &RowLines,
    pos_ms: f64,
    is_active: bool,
    dim: f32,
    display: &super::model::Display,
) -> RowLight {
    let light = super::model::row_light(row, pos_ms, is_active, false, display);
    let progress = line_progress(pos_ms, row.start_ms, row.duration_ms());
    let mask = if is_active && light.sweep.is_some() {
        Some(
            shaped
                .masks
                .iter()
                .map(|frames| oly_render::mask::position_at(frames, progress))
                .collect(),
        )
    } else {
        None
    };
    let glow = if is_active {
        light.glow.map(|mut g| {
            for v in &mut g {
                *v *= dim;
            }
            g
        })
    } else {
        None
    };
    RowLight {
        base: (light.dark * dim).clamp(0.0, 1.0),
        dark: (light.dark * dim).clamp(0.0, 1.0),
        bright: (light.bright * dim).clamp(0.0, 1.0),
        mask,
        glow,
    }
}

/// Paints the 3-dot interlude indicator centred at `(cx_pos, cy_pos)`.
/// Shared by both the main player lyric column and horizontal/vertical desktop lyrics.
#[allow(clippy::too_many_arguments)]
pub fn paint_interlude_strip(
    window: &mut Window,
    cx_pos: f32,
    cy_pos: f32,
    d: f32,
    g: f32,
    dot_opacities: [f32; 3],
    opacity: f32,
    vertical: bool,
    color_rgb: Option<u32>,
    shadow_rgb: Option<(u32, f32)>,
) {
    if d <= 0.0 || opacity <= 0.001 {
        return;
    }
    let span = d * 3.0 + g * 2.0;
    for i in 0..3 {
        let a = (opacity * dot_opacities[i]).clamp(0.0, 1.0);
        if a <= 0.001 {
            continue;
        }
        let (dx, dy) = if vertical {
            (cx_pos - d * 0.5, cy_pos - span * 0.5 + i as f32 * (d + g))
        } else {
            (cx_pos - span * 0.5 + i as f32 * (d + g), cy_pos - d * 0.5)
        };
        if let Some((s_rgb, s_dy)) = shadow_rgb {
            super::chrome::fill_round_subpixel(
                window,
                dx,
                dy + s_dy,
                d,
                d,
                d * 0.5,
                Hsla::from(gpui_kit::rgb(s_rgb)).opacity(0.55 * a),
            );
        }
        let fill = match color_rgb {
            Some(rgb) => Hsla::from(gpui_kit::rgb(rgb)).opacity(a),
            None => super::chrome::text(a),
        };
        super::chrome::fill_round_subpixel(window, dx, dy, d, d, d * 0.5, fill);
    }
}

/// Translation uses the same pixel feather as the original, with its own opacity.
pub fn paint_translation(window: &mut Window, layout: &gpui_kit::LineLayout, x: f32, y: f32, font_size: f32, alpha: f32, clip: [f32; 3], line_scale: f32, pivot_x: f32, pivot_y: f32, wrapper: WrapperTransform, blur: f32) {
    let sf = window.scale_factor();
    let total_scale = line_scale * wrapper.scale;
    let tx = (1.0 - wrapper.scale) * wrapper.pivot_x + wrapper.scale * (1.0 - line_scale) * pivot_x;
    let ty = (1.0 - wrapper.scale) * wrapper.pivot_y + wrapper.scale * (1.0 - line_scale) * pivot_y;
    let text_rgb = super::chrome::lyric_text_rgb();
    let alpha_out = if super::chrome::is_light_theme() && alpha > 0.01 {
        (alpha * 1.40).clamp(0.30, 1.0)
    } else {
        alpha
    };
    for run in &layout.runs {
        for glyph in &run.glyphs {
            let origin = point(px(x + f32::from(glyph.position.x)), px(y + f32::from(layout.ascent) + f32::from(glyph.position.y)));
            #[cfg(target_os = "windows")]
            { let _ = window.paint_lyric_glyph(origin, run.font_id, glyph.id, px(font_size), Hsla::from(gpui_kit::rgb(text_rgb)),
                gpui_kit::TransformationMatrix { rotation_scale: [[total_scale,0.0],[0.0,total_scale]], translation: [tx*sf,ty*sf] }, [1.0,0.0,1.0,alpha_out], [alpha_out,clip[0]*sf,clip[1]*sf,clip[2]*sf], (blur*sf*1.5).ceil().clamp(0.0,32.0) as u16); }
            #[cfg(not(target_os = "windows"))]
            { let _ = window.paint_glyph(origin,run.font_id,glyph.id,px(font_size),Hsla::from(gpui_kit::rgb(text_rgb)).opacity(alpha_out)); }
        }
    }
}

/// The word boxes for one row, computed from the shaped pen table.
///
/// `oly_render::text::LineLayout` already carries per-word geometry, but its
/// rects are unioned across visual lines and expressed in the unwrapped line's
/// coordinates. The mask needs a box per *visual* line, so the boxes are
/// rebuilt here from the pens and the recorded line ranges — the same inputs the
/// reference measured its boxes from.
pub fn word_boxes(
    text: &str,
    words: &[(String, i64, i64)],
    pens: &[(usize, f32)],
    lines: &[(std::ops::Range<usize>, LayoutRect)],
    tracking: impl Fn(usize) -> f32,
) -> Vec<WordBox> {
    let mut out = Vec::with_capacity(words.len());
    let mut cursor = 0usize;
    for (source, (text_value, start_ms, end_ms)) in words.iter().enumerate() {
        let needle = text_value.trim();
        if needle.is_empty() || cursor > text.len() {
            continue;
        }
        let Some(found) = text[cursor..].find(needle) else {
            continue;
        };
        let start = cursor + found;
        let end = start + needle.len();
        cursor = end;

        let x0 = pen_at(pens, start) + tracking(start);
        let x1 = pen_at(pens, end) + tracking(end);
        for (line_index, (range, _)) in lines.iter().enumerate() {
            let lo = start.max(range.start);
            let hi = end.min(range.end);
            if lo >= hi { continue; }
            let line_start = pen_at(pens, range.start) + tracking(range.start);
            let left = pen_at(pens, lo) + tracking(lo);
            let right = pen_at(pens, hi) + tracking(hi);
            out.push(WordBox {
                line: line_index, source, source_start: start,
                fragment_offset: left - x0, total_w: (x1-x0).max(1.0),
                x: left - line_start, w: (right-left).max(1.0),
                start: lo, end: hi, start_ms: *start_ms, end_ms: *end_ms,
            });
        }
    }
    out
}

/// Pen position at a byte offset, clamped to the shaped range.
pub fn pen_at(pens: &[(usize, f32)], byte: usize) -> f32 {
    if pens.is_empty() {
        return 0.0;
    }
    let idx = pens.partition_point(|(at, _)| *at < byte);
    pens.get(idx).or_else(|| pens.last()).map(|(_, x)| *x).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_feather_is_flat_in_the_middle_and_zero_outside() {
        assert_eq!(feather(-1.0, 0.0, 100.0, 10.0), 0.0);
        assert_eq!(feather(5.0, 0.0, 100.0, 10.0), 0.5);
        assert_eq!(feather(50.0, 0.0, 100.0, 10.0), 1.0);
        assert_eq!(feather(95.0, 0.0, 100.0, 10.0), 0.5);
        assert_eq!(feather(101.0, 0.0, 100.0, 10.0), 0.0);
    }

    #[test]
    fn a_fully_lit_word_uses_the_bright_alpha_and_an_unlit_one_the_dark() {
        let word = WordBox {
            line: 0, source: 0, source_start: 0, fragment_offset: 0.0, total_w: 100.0,
            x: 0.0,
            w: 100.0,
            start: 0,
            end: 3,
            start_ms: 0,
            end_ms: 100,
        };
        // The band has not reached the word yet: the whole word is dark.
        let unlit = word_alpha(&word, -400.0, 50.0, 0.5, 1.0, 50.0, 60.0);
        assert!((unlit - 0.5).abs() < 1e-4, "{unlit}");
        // The band has swept past: the whole word is bright.
        let lit = word_alpha(&word, 400.0, 50.0, 0.5, 1.0, 50.0, 60.0);
        assert!((lit - 1.0).abs() < 1e-4, "{lit}");
    }

    #[test]
    fn word_boxes_are_relative_to_their_own_visual_line() {
        // Two words on one visual line, pens in the unwrapped line.
        let text = "All that";
        let pens = vec![(0usize, 0.0f32), (4, 30.0), (9, 60.0)];
        let lines = vec![(0usize..9usize, LayoutRect::new(0.0, 0.0, 60.0, 40.0))];
        let boxes = word_boxes(
            text,
            &[("All ".to_string(), 0, 100), ("that".to_string(), 100, 200)],
            &pens,
            &lines,
            |_| 0.0,
        );
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].x, 0.0);
        assert_eq!(boxes[1].x, 30.0);
        assert_eq!(boxes[1].w, 30.0);
    }
    #[test]
    fn karaoke_uses_normalized_time_not_elapsed_milliseconds() {
        assert_eq!(line_progress(17_500.0, 17_000.0, 2_000.0), 0.25);
        assert_eq!(line_progress(16_000.0, 17_000.0, 2_000.0), 0.0);
    }

    #[test]
    fn a_wrapped_word_keeps_one_timeline_across_both_fragments() {
        let pens = [(0,0.0),(2,20.0),(4,40.0)];
        let boxes = word_boxes("abcd", &[("abcd".into(), 100, 2000)], &pens,
            &[(0..2, LayoutRect::new(0.0,0.0,20.0,20.0)),(2..4, LayoutRect::new(0.0,20.0,20.0,20.0))], |_| 0.0);
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[1].fragment_offset, 20.0);
        assert_eq!(boxes[1].total_w, 40.0);
        assert_eq!(boxes[1].source, boxes[0].source);
    }

}
