//! Small paint helpers shared by the player's window chrome: text, vector
//! icons, buttons and sliders.
//!
//! Everything here draws into the window's own scene through the public paint
//! API, so the chrome shares one GPU device and one frame with the lyrics
//! instead of standing up a second surface.

use gpui_kit::*;

use std::sync::atomic::{AtomicBool, Ordering};

/// The body text colour (`--text` in the shipped stylesheet).
pub const TEXT_HEX: u32 = 0xF4F6_F8;
/// Light theme body text colour (soft charcoal slate for comfortable contrast on bright glass).
pub const LIGHT_TEXT_HEX: u32 = 0x1F24_2D;
/// The default UI accent colour (Liquid Blue).
pub const ACCENT_HEX: u32 = 0x0A84_FF;

static LIGHT_THEME: AtomicBool = AtomicBool::new(false);

/// Set active UI theme for the current paint pass.
pub fn set_light_theme(light: bool) {
    LIGHT_THEME.store(light, Ordering::Relaxed);
}

/// Whether the active paint pass is using the light theme.
pub fn is_light_theme() -> bool {
    LIGHT_THEME.load(Ordering::Relaxed)
}

/// Active lyric glyph RGB (`0xRRGGBB`) for the current theme.
pub fn lyric_text_rgb() -> u32 {
    if is_light_theme() { LIGHT_TEXT_HEX } else { TEXT_HEX }
}

/// Frosted panel surface colour (shallow dark in dark mode, bright snow-white in light mode).
pub fn themed_surface(alpha: f32) -> Hsla {
    let hex = if is_light_theme() { 0xFAFC_FF } else { 0x1618_1D };
    Hsla::from(rgb(hex)).opacity(alpha)
}

/// Subtle hairline border colour for panels and cards.
pub fn themed_border(alpha: f32) -> Hsla {
    let hex = if is_light_theme() { 0x0000_00 } else { TEXT_HEX };
    Hsla::from(rgb(hex)).opacity(alpha)
}

/// Text at `alpha` of `--text`.
pub fn text(alpha: f32) -> Hsla {
    let hex = if is_light_theme() { LIGHT_TEXT_HEX } else { TEXT_HEX };
    Hsla::from(rgb(hex)).opacity(alpha)
}

/// The default accent at `alpha`.
pub fn accent(alpha: f32) -> Hsla {
    Hsla::from(rgb(ACCENT_HEX)).opacity(alpha)
}

/// Any hex colour at `alpha`.
pub fn accent_rgb(hex: u32, alpha: f32) -> Hsla {
    Hsla::from(rgb(hex)).opacity(alpha)
}

/// High-contrast label for active segments: keep the active theme text colour in light mode
/// so bright or custom accents never swallow the selected label, and crisp white in dark/glass mode.
pub fn accent_contrast_text(_accent_hex: u32, alpha: f32) -> Hsla {
    if is_light_theme() {
        text(alpha.clamp(0.0, 1.0))
    } else {
        Hsla::from(rgb(0xFFFFFF)).opacity(alpha.clamp(0.0, 1.0))
    }
}

/// High-contrast label for crisp active segments on dark/tinted glass.
pub fn segmented_active_text(accent_hex: u32, alpha: f32) -> Hsla {
    accent_contrast_text(accent_hex, alpha)
}

/// Smooth overlapping crossfade factors `(rest_alpha, glass_alpha)` for segmented selectors.
pub fn segmented_crossfade(swell: f32) -> (f32, f32) {
    let smoothstep = |a: f32, b: f32, x: f32| -> f32 {
        let u = ((x - a) / (b - a)).clamp(0.0, 1.0);
        u * u * (3.0 - 2.0 * u)
    };
    let t = swell.clamp(0.0, 1.0);
    let glass_alpha = smoothstep(0.0, 0.38, t);
    let rest_alpha = 1.0 - smoothstep(0.04, 0.92, t);
    (rest_alpha, glass_alpha)
}

/// Smoothly keeps active segment labels crisp across rest and glass states.
pub fn segmented_active_text_blended(accent_hex: u32, _rest_alpha: f32, alpha: f32) -> Hsla {
    accent_contrast_text(accent_hex, alpha)
}

/// The interface font.
///
/// Pretendard carries the Latin and the Cyrillic; the CJK families come from
/// the platform through the fallback chain, because the shipped CJK family is
/// not packaged with the experiment.
pub fn ui_font(weight: f32) -> Font {
    let mut font = gpui_kit::font("Pretendard");
    font.weight = FontWeight(weight);
    font.fallbacks = Some(FontFallbacks::from_fonts(vec![
        "檎风黑体 CHS".to_string(),
        "Microsoft YaHei".to_string(),
        "SimHei".to_string(),
        "Deng".to_string(),
        "Segoe UI".to_string(),
    ]));
    font
}

pub fn text_run(len: usize, weight: f32, color: Hsla) -> TextRun {
    TextRun {
        len,
        font: ui_font(weight),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Shapes one line without drawing it. Returns `(width, ascent)`.
pub fn measure(window: &Window, s: &str, size: f32, weight: f32) -> (f32, f32) {
    if s.is_empty() {
        return (0.0, size);
    }
    let run = text_run(s.len(), weight, text(1.0));
    let layout = window.text_system().layout_line(
        &SharedString::from(s.to_string()),
        px(size),
        &[run],
        None,
    );
    (f32::from(layout.width), f32::from(layout.ascent))
}

/// Draws one line of chrome text with its top-left corner at `(x, top)`.
///
/// Returns the advance width, so callers can lay out a row of mixed pieces
/// without a second shaping pass.
pub fn paint_text(
    window: &mut Window,
    s: &str,
    x: f32,
    top: f32,
    size: f32,
    weight: f32,
    color: Hsla,
) -> f32 {
    paint_text_tracked(window, s, x, top, size, weight, color, 0.0)
}

/// [`paint_text`] with CSS `letter-spacing`, expressed in ems.
///
/// The reference sets `letter-spacing: -.02em` on the window cover title, so
/// the title is measurably tighter than the default advance. Tracking is added
/// after every glyph, which is what the CSS property does, and the returned
/// width includes the trailing gap.
#[allow(clippy::too_many_arguments)]
pub fn paint_text_tracked(
    window: &mut Window,
    s: &str,
    x: f32,
    top: f32,
    size: f32,
    weight: f32,
    color: Hsla,
    tracking_em: f32,
) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let run = text_run(s.len(), weight, color);
    let layout = window.text_system().layout_line(
        &SharedString::from(s.to_string()),
        px(size),
        &[run],
        None,
    );
    let baseline = top + f32::from(layout.ascent);
    let track = tracking_em * size;
    let mut advance = 0.0_f32;
    for shaped_run in &layout.runs {
        for glyph in &shaped_run.glyphs {
            let origin = point(px(x) + glyph.position.x + px(advance), px(baseline));
            let _ = window.paint_glyph(
                origin,
                shaped_run.font_id,
                glyph.id,
                px(size),
                color,
            );
        }
        // Tracking accumulates per glyph, not per run; `glyphs` is the
        // sequence the shaper produced for this run.
        advance += track * shaped_run.glyphs.len() as f32;
    }
    f32::from(layout.width) + track * layout.runs.iter().map(|r| r.glyphs.len()).sum::<usize>() as f32
}

/// One line of text, horizontally centred in `[x, x + width)`.
pub fn paint_text_centred(
    window: &mut Window,
    s: &str,
    x: f32,
    width: f32,
    top: f32,
    size: f32,
    weight: f32,
    color: Hsla,
) -> f32 {
    let (w, _) = measure(window, s, size, weight);
    paint_text(window, s, x + (width - w) * 0.5, top, size, weight, color);
    w
}

/// One line of text, optically centred both horizontally and vertically inside `r`.
pub fn paint_text_box_centred(
    window: &mut Window,
    s: &str,
    r: crate::geometry::Box4,
    size: f32,
    weight: f32,
    color: Hsla,
) -> f32 {
    let (w, ascent) = measure(window, s, size, weight);
    let x = r.x + (r.w - w) * 0.5;
    let top = r.y + (r.h - size) * 0.5 - (ascent - size * 0.78) * 0.35;
    paint_text(window, s, x, top, size, weight, color)
}

/// One line of text, left-aligned at `x` and optically centred vertically around `cy`.
pub fn paint_text_vcentred(
    window: &mut Window,
    s: &str,
    x: f32,
    cy: f32,
    size: f32,
    weight: f32,
    color: Hsla,
) -> f32 {
    let (_, ascent) = measure(window, s, size, weight);
    let top = cy - size * 0.5 - (ascent - size * 0.78) * 0.35;
    paint_text(window, s, x, top, size, weight, color)
}

/// Fitted text, left-aligned at `x` and optically centred vertically around `cy`.
pub fn paint_text_fitted_vcentred(
    window: &mut Window,
    text: &str,
    x: f32,
    cy: f32,
    width: f32,
    size: f32,
    weight: f32,
    color: Hsla,
) {
    let (_, ascent) = measure(window, text, size, weight);
    let top = cy - size * 0.5 - (ascent - size * 0.78) * 0.35;
    paint_text_fitted(window, text, x, top, width, size, weight, color);
}

/// A filled rounded rectangle.
pub fn fill_round(window: &mut Window, x: f32, y: f32, w: f32, h: f32, r: f32, bg: Hsla) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    window.paint_quad(quad(
        bounds(point(px(x), px(y)), size(px(w), px(h))),
        Corners::all(px(r)),
        bg,
        Edges::default(),
        Hsla::default(),
        BorderStyle::Solid,
    ));
}

/// A filled rounded rectangle without integer pixel edge snapping for silky dot scaling.
pub fn fill_round_subpixel(window: &mut Window, x: f32, y: f32, w: f32, h: f32, r: f32, bg: Hsla) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    window.paint_subpixel_quad(quad(
        bounds(point(px(x), px(y)), size(px(w), px(h))),
        Corners::all(px(r)),
        bg,
        Edges::default(),
        Hsla::default(),
        BorderStyle::Solid,
    ));
}

/// A rounded rectangle outline.
pub fn stroke_round(
    window: &mut Window,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    width: f32,
    color: Hsla,
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    window.paint_quad(quad(
        bounds(point(px(x), px(y)), size(px(w), px(h))),
        Corners::all(px(r)),
        Hsla::default(),
        Edges::all(px(width)),
        color,
        BorderStyle::Solid,
    ));
}

fn paint_polygon(window: &mut Window, points: Vec<Point<Pixels>>, color: Hsla) {
    if points.len() < 3 {
        return;
    }
    let mut builder = PathBuilder::fill();
    builder.add_polygon(&points, true);
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// A play triangle.
pub fn icon_play(window: &mut Window, x: f32, y: f32, size: f32, color: Hsla) {
    paint_polygon(
        window,
        vec![
            point(px(x + size * 0.16), px(y)),
            point(px(x + size), px(y + size * 0.5)),
            point(px(x + size * 0.16), px(y + size)),
        ],
        color,
    );
}

/// Two pause bars.
pub fn icon_pause(window: &mut Window, x: f32, y: f32, size: f32, color: Hsla) {
    let bar = size * 0.26;
    let gap = size * 0.32;
    fill_round(window, x + size * 0.08, y, bar, size, size * 0.08, color);
    fill_round(window, x + size * 0.08 + gap, y, bar, size, size * 0.08, color);
}

/// A skip triangle with its end bar; `forward` mirrors it.
pub fn icon_skip(window: &mut Window, x: f32, y: f32, size: f32, color: Hsla, forward: bool) {
    let bar = size * 0.14;
    if forward {
        paint_polygon(
            window,
            vec![
                point(px(x), px(y)),
                point(px(x + size - bar * 1.8), px(y + size * 0.5)),
                point(px(x), px(y + size)),
            ],
            color,
        );
        fill_round(
            window,
            x + size - bar,
            y,
            bar,
            size,
            bar * 0.4,
            color,
        );
    } else {
        paint_polygon(
            window,
            vec![
                point(px(x + size), px(y)),
                point(px(x + bar * 1.8), px(y + size * 0.5)),
                point(px(x + size), px(y + size)),
            ],
            color,
        );
        fill_round(window, x, y, bar, size, bar * 0.4, color);
    }
}

/// Three dots, the "more" affordance.
pub fn icon_more(window: &mut Window, x: f32, y: f32, size: f32, color: Hsla) {
    let d = size * 0.19;
    let cy = y + size * 0.5 - d * 0.5;
    for i in 0..3 {
        let cx = x + size * 0.5 + (i as f32 - 1.0) * size * 0.28 - d * 0.5;
        fill_round(window, cx, cy, d, d, d * 0.5, color);
    }
}

/// A speaker: a box plus a cone, with two sound arcs.
pub fn icon_volume(window: &mut Window, x: f32, y: f32, size: f32, color: Hsla) {
    let cy = y + size * 0.5;
    fill_round(
        window,
        x + size * 0.06,
        cy - size * 0.13,
        size * 0.2,
        size * 0.26,
        size * 0.04,
        color,
    );
    paint_polygon(
        window,
        vec![
            point(px(x + size * 0.22), px(cy - size * 0.14)),
            point(px(x + size * 0.46), px(cy - size * 0.34)),
            point(px(x + size * 0.46), px(cy + size * 0.34)),
            point(px(x + size * 0.22), px(cy + size * 0.14)),
        ],
        color,
    );
    for (i, scale) in [0.30_f32, 0.46].iter().enumerate() {
        let r = size * scale;
        let mut builder = PathBuilder::stroke(px(size * 0.055 * (1.0 + i as f32 * 0.0)));
        builder.move_to(point(px(x + size * 0.52), px(cy - r * 0.72)));
        builder.cubic_bezier_to(
            point(px(x + size * 0.52 + r * 0.42), px(cy)),
            point(px(x + size * 0.52 + r * 0.42), px(cy)),
            point(px(x + size * 0.52), px(cy + r * 0.72)),
        );
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }
}

/// Computes the live slider knob box including hover/press scale, squeeze, and pull.
pub fn slider_knob_rect(
    x: f32,
    y: f32,
    width: f32,
    fraction: f32,
    response: crate::interaction::Feedback,
) -> crate::geometry::Box4 {
    let fraction = fraction.clamp(0.0, 1.0);
    let knob_x = x + width * fraction + response.pull[0] * 0.35;
    let cy = y + response.pull[1] * 0.25;
    let scale = 1.0 + 0.08 * response.hover + 0.14 * response.press.max(0.0);
    let kw = 16.0 * scale * (1.0 + response.squeeze[0] * 0.65);
    let kh = 16.0 * scale * (1.0 + response.squeeze[1] * 0.65);
    crate::geometry::Box4::new(knob_x - kw * 0.5, cy - kh * 0.5, kw, kh)
}

/// A slider: track, filled portion, knob. Returns the knob centre x.
pub fn paint_slider(
    window: &mut Window,
    x: f32,
    y: f32,
    width: f32,
    fraction: f32,
    accent_color: Hsla,
    dim: Hsla,
    response: crate::interaction::Feedback,
    opacity: f32,
) -> f32 {
    let fraction = fraction.clamp(0.0, 1.0);
    let press = response.press.max(0.0);
    let track_h = 4.0 * (1.0 + 0.35 * press + 0.06 * response.hover);
    let cy = y;
    fill_round(
        window,
        x,
        cy - track_h * 0.5,
        width,
        track_h,
        track_h * 0.5,
        dim,
    );
    let filled = width * fraction;
    if filled > 0.5 {
        fill_round(
            window,
            x,
            cy - track_h * 0.5,
            filled,
            track_h,
            track_h * 0.5,
            accent_color,
        );
        let glow_w = 12.0_f32.min(filled);
        window.paint_quad(quad(
            bounds(
                point(px(x + filled - glow_w), px(cy - track_h * 0.5)),
                size(px(glow_w), px(track_h)),
            ),
            Corners::all(px(track_h * 0.5)),
            linear_gradient(
                90.0,
                linear_color_stop(text(0.0), 0.0),
                linear_color_stop(text((0.35 + 0.25 * press) * opacity), 1.0),
            ),
            Edges::default(),
            Hsla::default(),
            BorderStyle::Solid,
        ));
    }
    let kr = slider_knob_rect(x, y, width, fraction, response);
    let radius = kr.w.min(kr.h) * 0.5;
    elevation(window, kr, radius, (0.65 + 0.28 * press + 0.08 * response.hover) * opacity);
    // Quiet at rest, transient glass lens when held: as `press` rises, the opaque
    // white body yields to a translucent specular shell so the D3D11 lens pass
    // (or the track beneath) refracts through the knob.
    let body_alpha = (0.88 * (1.0 - press.clamp(0.0, 1.0) * 0.72) + 0.06 * response.flash) * opacity;
    let rim_alpha = (0.78 + 0.18 * press + 0.10 * response.hover) * opacity;
    window.paint_quad(quad(
        bounds(point(px(kr.x), px(kr.y)), size(px(kr.w), px(kr.h))),
        Corners::all(px(radius)),
        linear_gradient(
            145.0 + response.focus[0] * 35.0,
            linear_color_stop(text((body_alpha + 0.10).min(1.0)), 0.0),
            linear_color_stop(text(body_alpha * 0.86), 1.0),
        ),
        Edges::all(px(0.85 + 0.25 * press)),
        text(rim_alpha.min(1.0)),
        BorderStyle::Solid,
    ));
    x + filled
}

/// A two-state switch pill. `fraction` animates the knob between the ends.
pub fn paint_switch(
    window: &mut Window,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    on: bool,
    fraction: f32,
    opacity: f32,
) {
    paint_switch_animated(
        window,
        x,
        y,
        w,
        h,
        on,
        fraction,
        crate::interaction::Feedback::default(),
        opacity,
    );
}

/// A two-state switch pill with liquid droplet thumb stretch and track recoil.
#[allow(clippy::too_many_arguments)]
pub fn paint_switch_animated(
    window: &mut Window,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    on: bool,
    fraction: f32,
    f: crate::interaction::Feedback,
    opacity: f32,
) {
    paint_switch_with_accent(window, x, y, w, h, on, fraction, f, ACCENT_HEX, opacity);
}

/// A two-state switch pill with a liquid-transparent glass track and an accent-coloured liquid droplet knob.
#[allow(clippy::too_many_arguments)]
pub fn paint_switch_with_accent(
    window: &mut Window,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    _on: bool,
    fraction: f32,
    f: crate::interaction::Feedback,
    accent_hex: u32,
    opacity: f32,
) {
    let fraction = fraction.clamp(0.0, 1.0);
    let press = f.press.max(0.0);
    let track_scale = 1.0 + 0.03 * f.hover - 0.03 * press;
    let tw = w * track_scale;
    let th = h * track_scale;
    let tx = x + (w - tw) * 0.5 + f.pull[0] * 0.12;
    let ty = y + (h - th) * 0.5 + f.pull[1] * 0.08;
    // Recessed neutral liquid-transparent glass well.
    let well_color = if is_light_theme() {
        Hsla::from(rgb(0x14_18_1F)).opacity((0.12 - 0.03 * fraction) * opacity)
    } else {
        Hsla::from(rgb(0x0809_0C)).opacity((0.24 - 0.04 * fraction) * opacity)
    };
    fill_round(
        window,
        tx,
        ty,
        tw,
        th,
        th * 0.5,
        well_color,
    );
    window.paint_quad(quad(
        bounds(point(px(tx), px(ty)), size(px(tw), px(th))),
        Corners::all(px(th * 0.5)),
        linear_gradient(
            145.0,
            linear_color_stop(text((0.10 + 0.05 * f.hover) * opacity), 0.0),
            linear_color_stop(text((0.04 + 0.03 * f.hover) * opacity), 1.0),
        ),
        Edges::all(px(0.8)),
        text((0.16 + 0.14 * f.hover + 0.08 * press) * opacity),
        BorderStyle::Solid,
    ));
    let kr=crate::details::switch_knob(crate::geometry::Box4::new(tx,ty,tw,th),fraction,press);
    let kh=kr.h;
    elevation(window, kr, kh * 0.5, (0.48 + 0.24 * press) * opacity);
    let knob_alpha =
        (0.78 + 0.16 * fraction - 0.12 * press.clamp(0.0, 1.0) + 0.08 * f.flash).clamp(0.45, 0.98)
            * opacity;
    let rim_alpha = (0.48 + 0.36 * fraction + 0.15 * press).clamp(0.0, 0.96) * opacity;
    let r_on = ((accent_hex >> 16) & 0xff) as f32 / 255.0;
    let g_on = ((accent_hex >> 8) & 0xff) as f32 / 255.0;
    let b_on = (accent_hex & 0xff) as f32 / 255.0;
    let off_base = if is_light_theme() { 0.98 } else { 0.66 };
    let mix = |off: f32, on: f32| off + (on - off) * fraction;
    let top_stop = Hsla::from(Rgba {
        r: mix(off_base - 0.02, r_on),
        g: mix(off_base, g_on),
        b: mix((off_base + 0.02).min(1.0), b_on),
        a: (knob_alpha + 0.08 * opacity).min(1.0),
    });
    let bot_stop = Hsla::from(Rgba {
        r: mix(0.46, r_on),
        g: mix(0.48, g_on),
        b: mix(0.52, b_on),
        a: (knob_alpha * mix(0.92, 0.78)).min(1.0),
    });
    window.paint_quad(quad(
        bounds(point(px(kr.x), px(kr.y)), size(px(kr.w), px(kr.h))),
        Corners::all(px(kh * 0.5)),
        linear_gradient(
            145.0 + f.focus[0] * 30.0,
            linear_color_stop(top_stop, 0.0),
            linear_color_stop(bot_stop, 1.0),
        ),
        Edges::all(px(0.85)),
        text(rim_alpha),
        BorderStyle::Solid,
    ));
}

/// Formats a millisecond count as `m:ss`.
pub fn clock_text(ms: f64) -> String {
    let total = (ms.max(0.0) / 1000.0).floor() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

/// Soft elevation shares GPUI's existing shadow pass.
pub fn elevation(window: &mut Window, r: crate::geometry::Box4, radius: f32, strength: f32) {
    window.paint_drop_shadows(
        bounds(point(px(r.x), px(r.y)), size(px(r.w), px(r.h))),
        Corners::all(px(radius)),
        &[
            BoxShadow::new(px(0.0), px(10.0), Hsla::from(rgb(0)).opacity(0.25 * strength)).blur_radius(px(28.0)),
            BoxShadow::new(px(0.0), px(2.0), Hsla::from(rgb(0)).opacity(0.18 * strength)).blur_radius(px(5.0)),
        ],
    );
}

/// A plain glass icon button: quiet at rest and hover (only the icon bounces
/// gently on hover); a liquid glass circle with a specular rim blooms around
/// the button only while it is held or dragged.
pub fn button_surface(window: &mut Window, r: crate::geometry::Box4, f: crate::interaction::Feedback) {
    button_surface_alpha(window, r, f, 1.0);
}

/// [`button_surface`] modulated by a foreground fade factor `alpha`.
pub fn button_surface_alpha(
    window: &mut Window,
    r: crate::geometry::Box4,
    f: crate::interaction::Feedback,
    alpha: f32,
) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.005 {
        return;
    }
    let press = f.press.clamp(0.0, 1.0);
    if press <= 0.015 {
        return;
    }
    let scale = (0.90 + 0.14 * press).max(0.85);
    let w = r.w * scale * (1.0 + f.squeeze[0] * 0.72);
    let h = r.h * scale * (1.0 + f.squeeze[1] * 0.72);
    let x = r.x + (r.w - w) * 0.5 + f.pull[0] * 0.55;
    let y = r.y + (r.h - h) * 0.5 + f.pull[1] * 0.55;
    let radius = w.min(h) * 0.5;
    let angle = 140.0 + f.focus[0] * 42.0 - f.focus[1] * 24.0;
    let top_stop = text(((0.18 * press + 0.10 * f.flash * press) * alpha).clamp(0.0, 0.52));
    let bot_stop = Hsla::from(rgb(0x7676_80)).opacity((0.12 * press * alpha).clamp(0.0, 0.36));
    let rim_alpha = ((0.42 * press + 0.18 * f.flash * press) * alpha).clamp(0.0, 0.78);
    window.paint_quad(quad(
        bounds(point(px(x), px(y)), size(px(w), px(h))),
        Corners::all(px(radius)),
        linear_gradient(
            angle,
            linear_color_stop(top_stop, 0.0),
            linear_color_stop(bot_stop, 1.0),
        ),
        Edges::all(px(0.95)),
        text(rim_alpha),
        BorderStyle::Solid,
    ));
}

/// The window playbar rail: the reference's 6px pill with a white gradient on
/// a blue-grey bed, a soft glow where the fill ends, and no knob until the
/// slider is hovered or dragged.
pub fn progress_rail(window: &mut Window, progress: crate::geometry::Box4, fraction: f32) {
    progress_rail_interactive(window, progress, fraction, crate::interaction::Feedback::default());
}

/// Interactive playbar rail that thickens when held and reveals a liquid lens knob on hover/drag.
pub fn progress_rail_interactive(
    window: &mut Window,
    progress: crate::geometry::Box4,
    fraction: f32,
    f: crate::interaction::Feedback,
) {
    progress_rail_interactive_alpha(window, progress, fraction, f, 1.0);
}

/// [`progress_rail_interactive`] modulated by a foreground fade factor `alpha`.
pub fn progress_rail_interactive_alpha(
    window: &mut Window,
    progress: crate::geometry::Box4,
    fraction: f32,
    f: crate::interaction::Feedback,
    alpha: f32,
) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.005 {
        return;
    }
    let press = f.press.max(0.0);
    let compact_bar = progress.h <= 5.0;
    let base_h = if compact_bar { progress.h.max(2.0) } else { 6.0 };
    let h = base_h * (1.0 + 0.32 * press + 0.08 * f.hover);
    let center_y = if compact_bar {
        progress.y + progress.h * 0.5
    } else {
        progress.y + 6.0
    };
    let y = center_y - h * 0.5;
    let rail_bg = if is_light_theme() {
        Hsla::from(rgb(0x14_18_1F)).opacity((0.12 + 0.05 * f.hover) * alpha)
    } else {
        Hsla::from(rgb(0xCDD3_EB)).opacity((0.22 + 0.06 * f.hover) * alpha)
    };
    fill_round(
        window,
        progress.x,
        y,
        progress.w,
        h,
        h * 0.5,
        rail_bg,
    );
    let clamped = fraction.clamp(0.0, 1.0);
    let filled = progress.w * clamped;
    if filled > 0.5 {
        let (fill_start, fill_end) = if is_light_theme() {
            (text(0.45 * alpha), text(0.88 * alpha))
        } else {
            (text(0.42 * alpha), text(alpha))
        };
        window.paint_quad(quad(
            bounds(point(px(progress.x), px(y)), size(px(filled), px(h))),
            Corners::all(px(h * 0.5)),
            linear_gradient(
                90.0,
                linear_color_stop(fill_start, 0.0),
                linear_color_stop(fill_end, 1.0),
            ),
            Edges::default(),
            Hsla::default(),
            BorderStyle::Solid,
        ));
        let glow_w = 24.0_f32.min(filled);
        window.paint_quad(quad(
            bounds(
                point(px(progress.x + filled - glow_w), px(y)),
                size(px(glow_w), px(h)),
            ),
            Corners::all(px(h * 0.5)),
            linear_gradient(
                90.0,
                linear_color_stop(text(0.0), 0.0),
                linear_color_stop(text(0.7 * alpha), 1.0),
            ),
            Edges::default(),
            Hsla::default(),
            BorderStyle::Solid,
        ));
        window.paint_drop_shadows(
            bounds(
                point(px(progress.x + filled - glow_w), px(y)),
                size(px(glow_w), px(h)),
            ),
            Corners::all(px(h * 0.5)),
            &[BoxShadow::new(px(0.0), px(0.0), text(0.30 * alpha)).blur_radius(px(5.0))],
        );
    }
    let reveal = (f.hover + press).clamp(0.0, 1.0) * alpha;
    if reveal > 0.01 {
        let base_d = 11.0 + 5.0 * press;
        let kw = base_d * (1.0 + f.squeeze[0] * 0.5);
        let kh = base_d * (1.0 + f.squeeze[1] * 0.5);
        let kx = (progress.x + filled).clamp(progress.x + kw * 0.5, progress.right() - kw * 0.5);
        let ky = y + h * 0.5;
        let kr = crate::geometry::Box4::new(kx - kw * 0.5, ky - kh * 0.5, kw, kh);
        elevation(window, kr, kh * 0.5, 0.55 * reveal);
        let body_alpha = (0.92 * (1.0 - press * 0.65)) * reveal;
        window.paint_quad(quad(
            bounds(point(px(kr.x), px(kr.y)), size(px(kr.w), px(kr.h))),
            Corners::all(px(kh * 0.5)),
            linear_gradient(
                145.0,
                linear_color_stop(text((body_alpha + 0.08 * alpha).min(1.0)), 0.0),
                linear_color_stop(text(body_alpha * 0.85), 1.0),
            ),
            Edges::all(px(0.85)),
            text((0.85 * reveal).min(1.0)),
            BorderStyle::Solid,
        ));
    }
}

/// `artistLabel` from `src/shared/track-labels.ts`.
///
/// Presentation only: the provider's artist keeps the album in the same field
/// ("Birdy — Beautiful Lies (Deluxe)"), and the cover shows just the name.
/// Never use this string for SMTC identity or a lyric lookup.
pub fn artist_label(artist: &str, album: &str) -> String {
    let label = artist.trim();
    let album = album.trim();
    if !album.is_empty() {
        let lower_label = label.to_lowercase();
        let lower_album = album.to_lowercase();
        if lower_label.ends_with(&lower_album) {
            let cut = label.len().saturating_sub(album.len());
            if label.is_char_boundary(cut) {
                if let Some(name) = strip_trailing_separator(&label[..cut]) {
                    if !name.is_empty() {
                        return name;
                    }
                }
            }
        }
    }
    match label.split_once(" — ") {
        Some((head, _)) => {
            let head = head.trim();
            if head.is_empty() {
                label.to_string()
            } else {
                head.to_string()
            }
        }
        None => label.to_string(),
    }
}

/// `/(?:\s+[—–-]\s*|\s*[·•|]\s*)$/`, applied to the album prefix.
///
/// The two alternatives differ on purpose: a dash only counts as a separator
/// when whitespace precedes it, while the low dots and the pipe do not. A bare
/// hyphen inside a name ("Jay-Z") must therefore survive.
fn strip_trailing_separator(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim_end();
    for separator in ["—", "–", "-"] {
        if let Some(head) = trimmed.strip_suffix(separator) {
            if head.ends_with(char::is_whitespace) {
                return Some(head.trim().to_string());
            }
        }
    }
    for separator in ["·", "•", "|"] {
        if let Some(head) = trimmed.strip_suffix(separator) {
            return Some(head.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod track_label_tests {
    use super::artist_label;

    #[test]
    fn an_artist_that_ends_with_the_album_drops_the_album() {
        assert_eq!(
            artist_label("Birdy — Beautiful Lies (Deluxe)", "Beautiful Lies (Deluxe)"),
            "Birdy"
        );
        assert_eq!(artist_label("Birdy - Beautiful Lies", "Beautiful Lies"), "Birdy");
        assert_eq!(artist_label("Birdy · Beautiful Lies", "Beautiful Lies"), "Birdy");
    }

    #[test]
    fn a_hyphen_inside_a_name_is_not_a_separator() {
        assert_eq!(artist_label("Jay-Z", "Z"), "Jay-Z");
        assert_eq!(artist_label("Jay-Z", ""), "Jay-Z");
    }

    #[test]
    fn an_artist_album_pair_is_split_when_the_album_field_is_empty() {
        assert_eq!(artist_label("Birdy — Beautiful Lies", ""), "Birdy");
        assert_eq!(artist_label("  Birdy  ", ""), "Birdy");
        assert_eq!(artist_label("", ""), "");
    }

    #[test]
    fn a_prefix_that_is_only_a_separator_keeps_the_original_label() {
        assert_eq!(artist_label(" — Album", "Album"), "— Album");
    }

    #[test]
    fn segmented_lens_glides_between_slots_and_swells_while_held() {
        let mut lens = super::SegmentedLens::default();
        let base_well = crate::geometry::Box4::new(8.0, 6.0, 236.0, 32.0);
        lens.step(1.0, 56.0, false, 16.0);
        assert!(!lens.pending());
        assert_eq!(lens.well_rect(base_well).w, base_well.w);

        // Press and drag freely across the 4 slots.
        lens.update_drag(180.0, 10.0, 232.0, 4, 1.0);
        assert!(lens.is_dragging());
        lens.step(1.0, 56.0, true, 16.0);
        assert!(lens.pending());
        assert!(lens.swell() > 0.0);
        let expanded_well = lens.well_rect(base_well);
        assert!(expanded_well.w > base_well.w);
        assert!(expanded_well.h > base_well.h);
        let r_held = lens.rect(10.0, 8.0, 28.0, crate::interaction::Feedback::default());
        assert!(r_held.h > 28.0);
        assert!(r_held.w > 56.0);
        assert_eq!(lens.slot_at(10.0, 232.0, 4, 180.0), 2);
        assert_eq!(lens.slot_at(10.0, 232.0, 4, 235.0), 3);

        // Release onto slot 2 and let the underdamped jelly spring settle.
        lens.end_drag();
        assert!(!lens.is_dragging());
        for _ in 0..160 {
            lens.step(117.0, 56.0, false, 16.0);
        }
        assert!(lens.settled_near_target());
        assert!(!lens.pending());
        let r_settled = lens.rect(10.0, 8.0, 28.0, crate::interaction::Feedback::default());
        assert!((r_settled.x - 127.0).abs() < 0.5);
        assert!((r_settled.h - 28.0).abs() < 0.2);
    }

    #[test]
    fn segmented_crossfade_overlaps_smoothly_and_fling_projects_next_slot() {
        let (r0, g0) = super::segmented_crossfade(0.0);
        assert!((r0 - 1.0).abs() < 0.001 && g0 == 0.0);
        let (r_mid, g_mid) = super::segmented_crossfade(0.35);
        assert!(r_mid > 0.1 && r_mid < 0.9, "rest pill blooms during release: {r_mid}");
        assert!(g_mid > 0.6, "glass remains visible during overlap: {g_mid}");
        let (r1, g1) = super::segmented_crossfade(1.0);
        assert_eq!(r1, 0.0);
        assert!((g1 - 1.0).abs() < 0.001);

        let mut lens = super::SegmentedLens::default();
        lens.step(1.0, 56.0, false, 16.0);
        for step in 0..4 {
            lens.update_drag(30.0 + step as f32 * 22.0, 10.0, 232.0, 4, 1.0);
            lens.step(1.0, 56.0, true, 16.0);
        }
        // Cursor is still inside slot 1 (at x = 112 -> 102/58 = 1.75), but strong rightward velocity projects into slot 2.
        assert_eq!(lens.slot_at(10.0, 232.0, 4, 112.0), 1);
        assert_eq!(lens.slot_at_release(10.0, 232.0, 4, 112.0), 2);
    }
}

/// A tactile, quiet surface: hover lifts the rim, press compresses the fill.
pub fn control_surface(window: &mut Window, r: crate::geometry::Box4, f: crate::interaction::Feedback, selected: bool) {
    control_surface_shape(window, r, f, selected, 1.0, None);
}
pub fn control_surface_alpha(window: &mut Window, r: crate::geometry::Box4, f: crate::interaction::Feedback, selected: bool, opacity: f32) {
    control_surface_shape(window, r, f, selected, opacity, Some(10.0));
}
pub fn control_surface_alpha_radius(
    window: &mut Window,
    r: crate::geometry::Box4,
    f: crate::interaction::Feedback,
    selected: bool,
    opacity: f32,
    radius: f32,
) {
    control_surface_shape(window, r, f, selected, opacity, Some(radius));
}
fn control_surface_shape(
    window: &mut Window,
    r: crate::geometry::Box4,
    f: crate::interaction::Feedback,
    selected: bool,
    opacity: f32,
    fixed_radius: Option<f32>,
) {
    let factor=(64.0/r.w.max(64.0)).min(1.0);
    let press=f.press.max(0.0);
    let w=r.w*(1.0+f.squeeze[0]*factor);let h=r.h*(1.0+f.squeeze[1]*factor);
    let x=r.x+(r.w-w)*0.5+f.pull[0]*factor;let y=r.y+(r.h-h)*0.5+f.pull[1]*factor;
    let base=if selected {0.13} else {0.0};
    let alpha=(base+f.hover*0.09+press*0.045+f.flash*0.08)*opacity;
    let gleam=(f.hover*0.11+press*0.05+f.flash*0.10)*opacity;
    let rim=((if selected {0.20} else {0.0})+f.hover*0.22+press*0.12+f.flash*0.16)*opacity;
    if alpha <= 0.001 && rim <= 0.001 {
        return;
    }
    let radius = fixed_radius.map(|r| r.min(w.min(h) * 0.5)).unwrap_or_else(|| w.min(h) * 0.5);
    window.paint_quad(quad(
        bounds(point(px(x), px(y)), size(px(w), px(h))),
        Corners::all(px(radius)),
        linear_gradient(
            150.0 + f.focus[0] * 38.0,
            linear_color_stop(text((alpha + gleam).min(1.0)), 0.0),
            linear_color_stop(text((alpha * 0.62).min(1.0)), 1.0),
        ),
        Edges::all(px(0.8)),
        text(rim.min(1.0)),
        BorderStyle::Solid,
    ));
}

/// Shared sliding, free-dragging, and jelly-deforming selection capsule for `.lg-segmented` controls.
pub struct SegmentedLens {
    x: oly_core::scroll::Spring,
    tail_x: oly_core::scroll::Spring,
    width: oly_core::scroll::Spring,
    swell: oly_core::scroll::Spring,
    jelly: oly_core::scroll::Spring,
    drag_x: Option<f32>,
    prev_drag_target: Option<f32>,
    last_vx: f32,
    drag_vx: f32,
    initialized: bool,
}

impl Default for SegmentedLens {
    fn default() -> Self {
        let mut x = oly_core::scroll::Spring::new(0.0);
        let mut tail_x = oly_core::scroll::Spring::new(0.0);
        let mut width = oly_core::scroll::Spring::new(56.0);
        let mut swell = oly_core::scroll::Spring::new(0.0);
        let mut jelly = oly_core::scroll::Spring::new(0.0);
        x.update_params(oly_core::scroll::SpringParams::new(88.0, 16.5));
        tail_x.update_params(oly_core::scroll::SpringParams::new(54.0, 13.8));
        width.update_params(oly_core::scroll::SpringParams::new(105.0, 17.5));
        swell.update_params(oly_core::scroll::SpringParams::new(34.0, 11.8));
        jelly.update_params(oly_core::scroll::SpringParams::new(120.0, 17.2));
        Self {
            x,
            tail_x,
            width,
            swell,
            jelly,
            drag_x: None,
            prev_drag_target: None,
            last_vx: 0.0,
            drag_vx: 0.0,
            initialized: false,
        }
    }
}

impl SegmentedLens {
    pub fn update_drag(&mut self, cursor_x: f32, track_x: f32, track_w: f32, count: usize, pad: f32) {
        let cell_w = track_w / count.max(1) as f32;
        let slot_w = (cell_w - 2.0 * pad).max(16.0);
        let min_x = pad;
        let max_x = (track_w - slot_w - pad).max(min_x);
        let raw = cursor_x - track_x - slot_w * 0.5;
        let clamped = if raw < min_x {
            min_x - (min_x - raw).min(44.0) * 0.34
        } else if raw > max_x {
            max_x + (raw - max_x).min(44.0) * 0.34
        } else {
            raw
        };
        self.drag_x = Some(clamped);
    }

    pub fn end_drag(&mut self) {
        self.drag_x = None;
        self.prev_drag_target = None;
        self.drag_vx = 0.0;
    }

    pub fn is_dragging(&self) -> bool {
        self.drag_x.is_some()
    }

    pub fn slot_at(&self, track_x: f32, track_w: f32, count: usize, cursor_x: f32) -> usize {
        if count <= 1 {
            return 0;
        }
        let cell_w = (track_w / count as f32).max(1.0);
        let idx = ((cursor_x - track_x) / cell_w).floor() as isize;
        idx.clamp(0, count as isize - 1) as usize
    }

    /// Chooses the target slot on pointer release, factoring in flick velocity
    /// so throwing the capsule smoothly commits to the thrown-to segment.
    pub fn slot_at_release(&self, track_x: f32, track_w: f32, count: usize, cursor_x: f32) -> usize {
        if count <= 1 {
            return 0;
        }
        let cell_w = (track_w / count as f32).max(1.0);
        let fling_shift = if self.drag_vx.abs() > 160.0 {
            (self.drag_vx * 0.095).clamp(-cell_w * 0.92, cell_w * 0.92)
        } else {
            0.0
        };
        self.slot_at(track_x, track_w, count, cursor_x + fling_shift)
    }

    pub fn step(&mut self, slot_x: f32, slot_w: f32, held: bool, dt_ms: f64) {
        let target_x = self.drag_x.unwrap_or(slot_x);
        let desired_x_params = if self.drag_x.is_some() {
            oly_core::scroll::SpringParams::new(2200.0, 82.0)
        } else {
            oly_core::scroll::SpringParams::new(88.0, 16.5)
        };
        let desired_tail_params = if self.drag_x.is_some() {
            oly_core::scroll::SpringParams::new(150.0, 18.0)
        } else {
            oly_core::scroll::SpringParams::new(54.0, 13.8)
        };
        let desired_swell_params = if held || self.drag_x.is_some() {
            oly_core::scroll::SpringParams::new(190.0, 21.0)
        } else {
            oly_core::scroll::SpringParams::new(34.0, 11.8)
        };
        if !self.initialized {
            self.x.update_params(desired_x_params);
            self.tail_x.update_params(desired_tail_params);
            self.swell.update_params(desired_swell_params);
            self.x.set_position(target_x as f64);
            self.tail_x.set_position(target_x as f64);
            self.width.set_position(slot_w as f64);
            self.initialized = true;
        } else {
            if self.x.params() != desired_x_params {
                self.x.update_params(desired_x_params);
            }
            if self.tail_x.params() != desired_tail_params {
                self.tail_x.update_params(desired_tail_params);
            }
            if self.swell.params() != desired_swell_params {
                self.swell.update_params(desired_swell_params);
            }
            self.x.set_target_position(target_x as f64);
            self.tail_x.set_target_position(target_x as f64);
            self.width.set_target_position(slot_w as f64);
        }
        let travel_swell = if !held && self.drag_x.is_none() {
            let rem = (self.x.target() - self.x.position()).abs() as f32;
            (rem / 28.0).clamp(0.0, 0.48) as f64
        } else {
            0.0
        };
        self.swell.set_target_position(if held || self.drag_x.is_some() {
            1.0
        } else {
            travel_swell
        });
        let dt_s = (dt_ms.clamp(0.0, 100.0)) / 1000.0;
        let prev_x = self.x.position() as f32;
        self.x.update(dt_s);
        self.tail_x.update(dt_s);
        self.width.update(dt_s);
        self.swell.update(dt_s);
        let vx = if dt_s > 0.0001 {
            (self.x.position() as f32 - prev_x) / dt_s as f32
        } else {
            0.0
        };
        self.last_vx = vx;
        if let Some(dx) = self.drag_x {
            if let Some(prev) = self.prev_drag_target {
                if dt_s > 0.0001 {
                    let pointer_vx = (dx - prev) / dt_s as f32;
                    self.drag_vx = self.drag_vx * 0.42 + pointer_vx * 0.58;
                }
            }
            self.prev_drag_target = Some(dx);
        } else {
            self.prev_drag_target = None;
            self.drag_vx *= 0.85;
        }
        let stretch_target = (((vx.abs() / 440.0).min(1.0)).powf(1.35) * 0.24) as f64;
        self.jelly.set_target_position(stretch_target);
        self.jelly.update(dt_s);
    }

    /// Expands the groove well slightly along the active axis while dragging the liquid slider.
    pub fn well_rect_axis(&self, base: crate::geometry::Box4, vertical: bool) -> crate::geometry::Box4 {
        let s = (self.swell.position() as f32).clamp(-0.08, 1.25);
        let (dw, dh) = if vertical { (2.0 * s, 4.0 * s) } else { (4.0 * s, 2.5 * s) };
        crate::geometry::Box4::new(
            base.x - dw * 0.5,
            base.y - dh * 0.5,
            base.w + dw,
            base.h + dh,
        )
    }

    pub fn well_rect(&self, base: crate::geometry::Box4) -> crate::geometry::Box4 {
        self.well_rect_axis(base, false)
    }

    pub fn rect(
        &self,
        track_x: f32,
        track_y: f32,
        track_h: f32,
        f: crate::interaction::Feedback,
    ) -> crate::geometry::Box4 {
        let swell = (self.swell.position() as f32).clamp(-0.10, 1.30);
        let jelly = (self.jelly.position() as f32).clamp(-0.22, 0.30);
        let base_w = (self.width.position() as f32).max(16.0);
        let head = self.x.position() as f32;
        let tail = self.tail_x.position() as f32;
        let tail_delta = tail - head;
        let tail_left = (tail_delta * 0.55).min(0.0).max(-base_w * 0.42);
        let tail_right = (tail_delta * 0.55).max(0.0).min(base_w * 0.42);
        let base_x = track_x + head + tail_left;
        let tailed_w = base_w + (tail_right - tail_left);
        let tail_thin = ((tail_delta.abs() / base_w.max(1.0)) * 0.14).min(0.12);
        let sx = (1.0 + 0.30 * swell) * (1.0 + jelly * 0.65) * (1.0 + f.squeeze[0] * 0.35);
        let squash = (0.18 * jelly + 0.28 * tail_thin).min(0.30 * swell.max(0.0));
        let sy = (1.0 + 0.62 * swell) * (1.0 - squash) * (1.0 + f.squeeze[1] * 0.35);
        let w = tailed_w * sx;
        let h = track_h * sy;
        let x = base_x + (tailed_w - w) * 0.5 + f.pull[0] * 0.35;
        let y = track_y + (track_h - h) * 0.5 + f.pull[1] * 0.25;
        crate::geometry::Box4::new(x, y, w, h)
    }

    /// Computes a vertical sliding/jelly selection box for vertical segmented rails.
    pub fn v_rect(
        &self,
        list_x: f32,
        list_y: f32,
        list_w: f32,
        f: crate::interaction::Feedback,
    ) -> crate::geometry::Box4 {
        let swell = (self.swell.position() as f32).clamp(-0.10, 1.30);
        let jelly = (self.jelly.position() as f32).clamp(-0.22, 0.30);
        let base_h = (self.width.position() as f32).max(16.0);
        let head = self.x.position() as f32;
        let tail = self.tail_x.position() as f32;
        let tail_delta = tail - head;
        let tail_top = (tail_delta * 0.55).min(0.0).max(-base_h * 0.42);
        let tail_bot = (tail_delta * 0.55).max(0.0).min(base_h * 0.42);
        let base_y = list_y + head + tail_top;
        let tailed_h = base_h + (tail_bot - tail_top);
        let tail_thin = ((tail_delta.abs() / base_h.max(1.0)) * 0.12).min(0.10);
        let sy = (1.0 + 0.40 * swell) * (1.0 + jelly * 0.6) * (1.0 + f.squeeze[1] * 0.35);
        let sx = (1.0 + 0.10 * swell) * (1.0 - 0.30 * jelly - tail_thin) * (1.0 + f.squeeze[0] * 0.35);
        let w = list_w * sx;
        let h = tailed_h * sy;
        let x = list_x + (list_w - w) * 0.5 + f.pull[0] * 0.25;
        let y = base_y + (tailed_h - h) * 0.5 + f.pull[1] * 0.35;
        crate::geometry::Box4::new(x, y, w, h)
    }

    pub fn swell(&self) -> f32 {
        self.swell.position() as f32
    }

    pub fn settled_near_target(&self) -> bool {
        self.initialized
            && self.drag_x.is_none()
            && (self.x.position() - self.x.target()).abs() < 0.8
            && self.last_vx.abs() < 12.0
            && self.swell.position().abs() < 0.12
    }

    pub fn pending(&self) -> bool {
        self.initialized
            && (self.drag_x.is_some()
                || !self.x.arrived()
                || !self.tail_x.arrived()
                || !self.width.arrived()
                || !self.swell.arrived()
                || !self.jelly.arrived())
    }

    pub fn reset(&mut self) {
        self.drag_x = None;
        self.prev_drag_target = None;
        self.last_vx = 0.0;
        self.drag_vx = 0.0;
        self.initialized = false;
    }
}

/// Paints the translucent rounded glass bed behind a segmented selector.
pub fn paint_segmented_well(window: &mut Window, r: crate::geometry::Box4, radius: f32, opacity: f32) {
    let radius = radius.min(r.w.min(r.h) * 0.5);
    let well_bg = if is_light_theme() {
        Hsla::from(rgb(0x14_18_1F)).opacity(0.07 * opacity)
    } else {
        Hsla::from(rgb(0x0A0C_10)).opacity(0.14 * opacity)
    };
    fill_round(
        window,
        r.x,
        r.y,
        r.w,
        r.h,
        radius,
        well_bg,
    );
    window.paint_quad(quad(
        bounds(point(px(r.x), px(r.y)), size(px(r.w), px(r.h))),
        Corners::all(px(radius)),
        linear_gradient(
            180.0,
            linear_color_stop(text((0.085 * opacity).min(1.0)), 0.0),
            linear_color_stop(text((0.035 * opacity).min(1.0)), 1.0),
        ),
        Edges::all(px(0.85)),
        text((0.15 * opacity).min(1.0)),
        BorderStyle::Solid,
    ));
}

/// Paints the quiet, soft accent-tinted selection pill shown when a segmented selector is at rest.
pub fn paint_segmented_rest_pill(
    window: &mut Window,
    r: crate::geometry::Box4,
    radius: f32,
    accent_hex: u32,
    rest_alpha: f32,
    opacity: f32,
) {
    let a = (rest_alpha * opacity).clamp(0.0, 1.0);
    if a <= 0.01 {
        return;
    }
    let r_pill = radius.min(r.w.min(r.h) * 0.5);
    let fill_alpha = if is_light_theme() { 0.48 } else { 0.34 };
    fill_round_subpixel(
        window,
        r.x,
        r.y,
        r.w,
        r.h,
        r_pill,
        accent_rgb(accent_hex, fill_alpha * a),
    );
    if is_light_theme() {
        stroke_round(
            window,
            r.x,
            r.y,
            r.w,
            r.h,
            r_pill,
            0.9,
            accent_rgb(accent_hex, 0.78 * a),
        );
    }
}

/// Paints the subtle specular shell of the `.lg-selection-lens` droplet only while sliding/dragging.
pub fn paint_segmented_lens(
    window: &mut Window,
    r: crate::geometry::Box4,
    _radius: f32,
    swell: f32,
    f: crate::interaction::Feedback,
    opacity: f32,
) {
    let swell = swell.clamp(0.0, 1.25);
    if swell <= 0.001 {
        return;
    }
    let (_, strength) = segmented_crossfade(swell);
    let r_pill = r.h * 0.5;
    let elev = if is_light_theme() { 0.34 } else { 0.22 };
    elevation(window, r, r_pill, (elev * swell * strength) * opacity);
    let top_alpha = (0.03 * swell + 0.018 * f.hover) * strength * opacity;
    let bot_alpha = (0.008 * swell + 0.006 * f.hover) * strength * opacity;
    window.paint_subpixel_quad(quad(
        bounds(point(px(r.x), px(r.y)), size(px(r.w), px(r.h))),
        Corners::all(px(r_pill)),
        linear_gradient(
            150.0 + f.focus[0] * 35.0,
            linear_color_stop(text(top_alpha.min(1.0)), 0.0),
            linear_color_stop(text(bot_alpha.min(1.0)), 1.0),
        ),
        Edges::default(),
        gpui_kit::transparent_black(),
        BorderStyle::Solid,
    ));
}

/// Paints an integrated `.lg-stepper` capsule well and its left/right `-` / `+` buttons.
#[allow(clippy::too_many_arguments)]
pub fn paint_stepper(
    window: &mut Window,
    minus_rect: crate::geometry::Box4,
    plus_rect: crate::geometry::Box4,
    minus_feedback: crate::interaction::Feedback,
    plus_feedback: crate::interaction::Feedback,
    label: &str,
    enabled: bool,
    opacity: f32,
) {
    let well = crate::geometry::Box4::new(
        minus_rect.x - 2.0,
        minus_rect.y - 1.0,
        (plus_rect.right() - minus_rect.x) + 4.0,
        minus_rect.h + 2.0,
    );
    let r = well.h * 0.5;
    let step_bg = if is_light_theme() {
        Hsla::from(rgb(0x14_18_1F)).opacity(0.08 * opacity)
    } else {
        Hsla::from(rgb(0x090A_0D)).opacity(0.28 * opacity)
    };
    fill_round(
        window,
        well.x,
        well.y,
        well.w,
        well.h,
        r,
        step_bg,
    );
    fill_round(window, well.x, well.y, well.w, well.h, r, text(0.07 * opacity));
    stroke_round(window, well.x, well.y, well.w, well.h, r, 0.75, text(0.14 * opacity));
    for (b, f, sym) in [(minus_rect, minus_feedback, "−"), (plus_rect, plus_feedback, "+")] {
        let press = f.press.max(0.0);
        let scale = 1.0 - 0.08 * press + 0.03 * f.hover;
        let bw = b.w * scale;
        let bh = b.h * scale;
        let bx = b.x + (b.w - bw) * 0.5 + f.pull[0] * 0.25;
        let by = b.y + (b.h - bh) * 0.5 + f.pull[1] * 0.25;
        let fill_a = (0.06 + 0.12 * f.hover + 0.08 * press + 0.14 * f.flash) * opacity;
        fill_round(window, bx, by, bw, bh, bh * 0.5, text(fill_a));
        if f.hover > 0.01 || press > 0.01 {
            stroke_round(
                window,
                bx,
                by,
                bw,
                bh,
                bh * 0.5,
                0.75,
                text((0.22 * f.hover + 0.28 * press) * opacity),
            );
        }
        paint_text_box_centred(
            window,
            sym,
            b,
            15.0,
            500.0,
            text(if enabled { 0.92 } else { 0.28 } * opacity),
        );
    }
    let mid_x = minus_rect.right() + 2.0;
    let mid_w = (plus_rect.x - mid_x - 2.0).max(20.0);
    paint_text_box_centred(
        window,
        label,
        crate::geometry::Box4::new(mid_x, minus_rect.y, mid_w, minus_rect.h),
        10.5,
        550.0,
        text(if enabled { 0.90 } else { 0.32 } * opacity),
    );
}

/// Paints a tactile liquid glass choice pill (`Control::Choice`) with specular rim and animated chevron.
pub fn paint_choice_pill(
    window: &mut Window,
    r: crate::geometry::Box4,
    label: &str,
    f: crate::interaction::Feedback,
    opacity: f32,
) {
    paint_choice_pill_state(window, r, label, false, ACCENT_HEX, f, opacity);
}

/// Arrow direction for `paint_choice_pill_with_arrow`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceArrow {
    Down,
    Up,
    Right,
    Left,
}

/// Paints a tactile liquid glass choice pill (`Control::Choice`) with open/closed dropdown state.
pub fn paint_choice_pill_state(
    window: &mut Window,
    r: crate::geometry::Box4,
    label: &str,
    open: bool,
    accent_hex: u32,
    f: crate::interaction::Feedback,
    opacity: f32,
) {
    let arrow = if open { ChoiceArrow::Up } else { ChoiceArrow::Down };
    paint_choice_pill_with_arrow(window, r, label, open, arrow, accent_hex, f, opacity);
}

/// Paints a tactile liquid glass choice pill with an explicit arrow direction.
#[allow(clippy::too_many_arguments)]
pub fn paint_choice_pill_with_arrow(
    window: &mut Window,
    r: crate::geometry::Box4,
    label: &str,
    open: bool,
    arrow: ChoiceArrow,
    accent_hex: u32,
    f: crate::interaction::Feedback,
    opacity: f32,
) {
    let press = f.press.max(0.0);
    let factor = (64.0 / r.w.max(64.0)).min(1.0);
    let w = r.w * (1.0 + f.squeeze[0] * factor - 0.015 * press);
    let h = r.h * (1.0 + f.squeeze[1] * factor - 0.02 * press);
    let x = r.x + (r.w - w) * 0.5 + f.pull[0] * factor;
    let y = r.y + (r.h - h) * 0.5 + f.pull[1] * factor;
    let top_a = (0.09 + 0.08 * f.hover + 0.05 * press + 0.09 * f.flash + if open { 0.05 } else { 0.0 }) * opacity;
    let bot_a = (0.05 + 0.04 * f.hover + 0.03 * press) * opacity;
    let border_col = if open {
        accent_rgb(accent_hex, 0.55 * opacity)
    } else {
        let rim_a = (0.14 + 0.22 * f.hover + 0.16 * press + 0.14 * f.flash) * opacity;
        text(rim_a.min(1.0))
    };
    window.paint_quad(quad(
        bounds(point(px(x), px(y)), size(px(w), px(h))),
        Corners::all(px(9.5)),
        linear_gradient(
            150.0 + f.focus[0] * 35.0,
            linear_color_stop(text(top_a.min(1.0)), 0.0),
            linear_color_stop(text(bot_a.min(1.0)), 1.0),
        ),
        Edges::all(px(0.85)),
        border_col,
        BorderStyle::Solid,
    ));
    paint_text_fitted(
        window,
        label,
        x + 11.0,
        y + 6.0,
        (w - 34.0).max(20.0),
        13.0,
        550.0,
        text((0.92 + 0.06 * f.hover).min(1.0) * opacity),
    );
    let arrow_col = text((0.56 + 0.32 * f.hover) * opacity);
    match arrow {
        ChoiceArrow::Down => chevron_vertical(window, x + w - 18.0, y + 11.5 + 1.2 * press, false, arrow_col),
        ChoiceArrow::Up => chevron_vertical(window, x + w - 18.0, y + 11.5 + 1.2 * press, true, arrow_col),
        ChoiceArrow::Right => chevron_horizontal(window, x + w - 16.0, y + 10.0 + 1.2 * press, true, arrow_col),
        ChoiceArrow::Left => chevron_horizontal(window, x + w - 16.0, y + 10.0 + 1.2 * press, false, arrow_col),
    }
}

/// Paints a single liquid glass option row inside a dropdown list or version picker.
#[allow(clippy::too_many_arguments)]
pub fn paint_list_item(
    window: &mut Window,
    r: crate::geometry::Box4,
    label: &str,
    selected: bool,
    swatch_hex: Option<u32>,
    accent_hex: u32,
    f: crate::interaction::Feedback,
    opacity: f32,
) {
    let _=accent_hex;
    let x=r.x+38.;
    if selected {checkmark(window,r.x+12.,r.y+(r.h-8.)/2.,text(0.94*opacity));}
    if let Some(hex)=swatch_hex {
        fill_round(window,x,r.y+(r.h-10.)/2.,10.,10.,5.,accent_rgb(hex,opacity));
    }
    let tx=x+if swatch_hex.is_some(){18.}else{0.};
    let (title,subtitle)=label.split_once('\n').unwrap_or((label,""));
    let title_y=r.y+if subtitle.is_empty(){(r.h-18.)/2.}else{8.};
    paint_text_fitted(window,title,tx,title_y,(r.right()-tx-14.).max(1.),15.,500.,text((0.9+0.08*f.hover)*opacity));
    if !subtitle.is_empty(){paint_text_fitted(window,subtitle,tx,r.y+33.,(r.right()-tx-14.).max(1.),12.,400.,text(0.43*opacity));}

}

pub fn chevron_vertical(window: &mut Window, x: f32, y: f32, up: bool, color: Hsla) {
    let mut p = PathBuilder::stroke(px(1.5));
    if up {
        p.move_to(point(px(x), px(y + 5.0)));
        p.line_to(point(px(x + 4.0), px(y + 1.0)));
        p.line_to(point(px(x + 8.0), px(y + 5.0)));
    } else {
        p.move_to(point(px(x), px(y + 1.0)));
        p.line_to(point(px(x + 4.0), px(y + 5.0)));
        p.line_to(point(px(x + 8.0), px(y + 1.0)));
    }
    if let Ok(path) = p.build() {
        window.paint_path(path, color);
    }
}

pub fn chevron_horizontal(window: &mut Window, x: f32, y: f32, right: bool, color: Hsla) {
    let mut p = PathBuilder::stroke(px(1.5));
    if right {
        p.move_to(point(px(x + 1.0), px(y)));
        p.line_to(point(px(x + 5.0), px(y + 4.0)));
        p.line_to(point(px(x + 1.0), px(y + 8.0)));
    } else {
        p.move_to(point(px(x + 5.0), px(y)));
        p.line_to(point(px(x + 1.0), px(y + 4.0)));
        p.line_to(point(px(x + 5.0), px(y + 8.0)));
    }
    if let Ok(path) = p.build() {
        window.paint_path(path, color);
    }
}

pub fn checkmark(window: &mut Window, x: f32, y: f32, color: Hsla) {
    let mut p = PathBuilder::stroke(px(1.75));
    p.move_to(point(px(x), px(y + 4.0)));
    p.line_to(point(px(x + 3.2), px(y + 7.2)));
    p.line_to(point(px(x + 9.0), px(y + 0.8)));
    if let Ok(path) = p.build() {
        window.paint_path(path, color);
    }
}

/// The reference's `.cover-blank`: a soft diagonal highlight over a light
/// wash, shown while a track exists but the provider sent no artwork.
///
/// `radial-gradient(120% 120% at 20% 15%, rgb(255 255 255 / .14), transparent 60%),
/// rgb(255 255 255 / .06)` on a 16px rounded box. gpui only has two-stop linear
/// gradients, so the radial falloff is approximated by a centre-weighted
/// horizontal band that fades out at both edges.

pub fn chevron(window: &mut Window, x: f32, y: f32, color: Hsla) {
    let mut p = PathBuilder::stroke(px(1.5));
    p.move_to(point(px(x), px(y)));
    p.line_to(point(px(x+4.0), px(y+4.0)));
    p.line_to(point(px(x), px(y+8.0)));
    if let Ok(path) = p.build() { window.paint_path(path, color); }
}

/// Keep provider titles inside their row, including mixed CJK/Latin labels.
pub fn paint_text_fitted(window: &mut Window, text: &str, x: f32, y: f32, width: f32, size: f32, weight: f32, color: Hsla) {
    if measure(window, text, size, weight).0 <= width {
        paint_text(window, text, x, y, size, weight, color);
        return;
    }
    let char_ends: Vec<usize> = text
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .collect();
    let mut lo = 0usize;
    let mut hi = char_ends.len();
    let mut probe = String::with_capacity(text.len() + 4);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        probe.clear();
        probe.push_str(&text[..char_ends[mid - 1]]);
        probe.push('…');
        if measure(window, &probe, size, weight).0 <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    probe.clear();
    if lo > 0 {
        probe.push_str(&text[..char_ends[lo - 1]]);
    }
    probe.push('…');
    paint_text(window, &probe, x, y, size, weight, color);
}

/// Paints a tactile liquid-glass button that adapts across Light, Dark, and LiquidGlass themes.
#[allow(clippy::too_many_arguments)]
pub fn paint_glass_button(
    window: &mut Window,
    r: crate::geometry::Box4,
    label: &str,
    accent_hex: u32,
    primary: bool,
    danger: bool,
    f: crate::interaction::Feedback,
    opacity: f32,
) {
    let press = f.press.max(0.0);
    let factor = (56.0 / r.w.max(56.0)).min(1.0);
    let w = r.w * (1.0 + f.squeeze[0] * factor - 0.018 * press);
    let h = r.h * (1.0 + f.squeeze[1] * factor - 0.024 * press);
    let x = r.x + (r.w - w) * 0.5 + f.pull[0] * factor;
    let y = r.y + (r.h - h) * 0.5 + f.pull[1] * factor;
    let radius = (h * 0.34).clamp(7.0, 10.0);
    let light = is_light_theme();

    let (top_col, bot_col, border_col, label_col) = if danger {
        let red = Hsla::from(rgb(0xFF_45_3A));
        let top_a = (if light { 0.14 } else { 0.22 } + 0.08 * f.hover + 0.06 * press) * opacity;
        let bot_a = (if light { 0.08 } else { 0.12 } + 0.04 * f.hover) * opacity;
        let rim = red.opacity((0.36 + 0.24 * f.hover + 0.14 * f.flash) * opacity);
        let fg = Hsla::from(rgb(if light { 0xC9_2A_22 } else { 0xFF_69_61 })).opacity(0.96 * opacity);
        (red.opacity(top_a), red.opacity(bot_a), rim, fg)
    } else if primary {
        let base_a = if light { 0.90 } else { 0.72 };
        let top = accent_rgb(accent_hex, (base_a + 0.08 * f.hover + 0.06 * f.flash).clamp(0.0, 1.0) * opacity);
        let bot = accent_rgb(accent_hex, (base_a - 0.12 + 0.06 * f.hover).clamp(0.0, 1.0) * opacity);
        let rim = Hsla::from(rgb(0xFF_FF_FF)).opacity((if light { 0.32 } else { 0.26 } + 0.16 * f.hover) * opacity);
        let fg = accent_contrast_text(accent_hex, 0.98 * opacity);
        (top, bot, rim, fg)
    } else if light {
        let top = Hsla::from(rgb(0xFF_FF_FF)).opacity((0.82 + 0.12 * f.hover + 0.06 * press) * opacity);
        let bot = Hsla::from(rgb(0xE8_EE_F7)).opacity((0.68 + 0.10 * f.hover) * opacity);
        let rim = themed_border((0.14 + 0.14 * f.hover + 0.12 * press + 0.10 * f.flash) * opacity);
        let fg = text((0.88 + 0.10 * f.hover).min(1.0) * opacity);
        (top, bot, rim, fg)
    } else {
        let top = text((0.12 + 0.08 * f.hover + 0.06 * press + 0.08 * f.flash).min(1.0) * opacity);
        let bot = text((0.05 + 0.05 * f.hover + 0.03 * press).min(1.0) * opacity);
        let rim = text((0.18 + 0.20 * f.hover + 0.14 * press + 0.14 * f.flash).min(1.0) * opacity);
        let fg = text((0.90 + 0.08 * f.hover).min(1.0) * opacity);
        (top, bot, rim, fg)
    };

    window.paint_quad(quad(
        bounds(point(px(x), px(y)), size(px(w), px(h))),
        Corners::all(px(radius)),
        linear_gradient(
            150.0 + f.focus[0] * 35.0,
            linear_color_stop(top_col, 0.0),
            linear_color_stop(bot_col, 1.0),
        ),
        Edges::all(px(0.85)),
        border_col,
        BorderStyle::Solid,
    ));
    paint_text_box_centred(window, label, crate::geometry::Box4::new(x, y, w, h), 12.0, 580.0, label_col);
}

/// Paints a recessed liquid-glass text input field with centred text, scrolling overflow, and caret.
#[allow(clippy::too_many_arguments)]
pub fn paint_glass_input(
    window: &mut Window,
    r: crate::geometry::Box4,
    placeholder: &str,
    raw: &str,
    secret: bool,
    active: bool,
    caret_visible: bool,
    accent_hex: u32,
    opacity: f32,
) {
    let light = is_light_theme();
    let radius = (r.h * 0.28).clamp(7.0, 9.5);
    let (top_col, bot_col) = if light {
        let base = if active { 0.88 } else { 0.68 };
        (
            Hsla::from(rgb(0xFF_FF_FF)).opacity(base * opacity),
            Hsla::from(rgb(0xF0_F4_FA)).opacity(base * opacity),
        )
    } else {
        let a_top = if active { 0.16 } else { 0.09 };
        let a_bot = if active { 0.10 } else { 0.05 };
        (
            Hsla::from(rgb(0x0A_0C_10)).opacity(a_top * 2.2 * opacity),
            text(a_bot * opacity),
        )
    };
    let border_col = if active {
        accent_rgb(accent_hex, 0.92 * opacity)
    } else {
        themed_border((if light { 0.14 } else { 0.18 }) * opacity)
    };
    window.paint_quad(quad(
        bounds(point(px(r.x), px(r.y)), size(px(r.w), px(r.h))),
        Corners::all(px(radius)),
        linear_gradient(
            180.0,
            linear_color_stop(top_col, 0.0),
            linear_color_stop(bot_col, 1.0),
        ),
        Edges::all(px(if active { 1.25 } else { 0.85 })),
        border_col,
        BorderStyle::Solid,
    ));

    let shown = if secret && !raw.is_empty() {
        "•".repeat(raw.chars().count().min(32))
    } else if raw.is_empty() {
        placeholder.to_owned()
    } else {
        raw.to_owned()
    };
    let text_x = r.x + 10.0;
    let text_max_w = (r.w - 22.0).max(14.0);
    let text_w = measure(window, &shown, 12.0, 500.0).0;
    let draw_x = if !raw.is_empty() && text_w > text_max_w {
        text_x - (text_w - text_max_w)
    } else {
        text_x
    };
    let text_top = r.y + (r.h - 15.0) * 0.5;
    let mask = ContentMask {
        bounds: bounds(point(px(text_x), px(r.y + 1.0)), size(px(text_max_w), px((r.h - 2.0).max(1.0)))),
    };
    window.with_content_mask(Some(mask), |window| {
        paint_text(
            window,
            &shown,
            draw_x,
            text_top,
            12.0,
            500.0,
            text((if raw.is_empty() { 0.42 } else { 0.94 }) * opacity),
        );
    });
    if active && caret_visible {
        let caret_x = if raw.is_empty() {
            text_x + 1.5
        } else {
            text_x + text_w.min(text_max_w) + 1.5
        };
        let caret_h = 15.0_f32.min((r.h - 10.0).max(10.0));
        fill_round(
            window,
            caret_x,
            r.y + (r.h - caret_h) * 0.5,
            1.5,
            caret_h,
            0.75,
            accent_rgb(accent_hex, 0.95 * opacity),
        );
    }
}
