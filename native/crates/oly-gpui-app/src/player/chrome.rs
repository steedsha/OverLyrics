//! Generic chrome primitives are re-exported from `oly-ui`; player-specific helpers live here.

use gpui_kit::*;

pub use oly_ui::chrome::*;

/// Default monochrome cover placeholder with a subtle neutral card and centered
/// music-note icon when no track is playing or no artwork was provided.
pub fn cover_blank(
    window: &mut Window,
    cx: &App,
    r: super::model::Box4,
    radius: f32,
    alpha: f32,
) {
    let a = alpha.clamp(0.0, 1.0);
    if a <= 0.005 {
        return;
    }
    let light = is_light_theme();
    let (base, sheen, border, note) = if light {
        (
            Hsla::from(rgb(0xE8_EA_EE)).opacity(0.95 * a),
            Hsla::from(rgb(0xFF_FF_FF)).opacity(0.45 * a),
            themed_border(0.10 * a),
            Hsla::from(rgb(0x1C_1E_22)).opacity(0.30 * a),
        )
    } else {
        (
            Hsla::from(rgb(0x1E_1F_22)).opacity(0.92 * a),
            Hsla::from(rgb(0xFF_FF_FF)).opacity(0.10 * a),
            text(0.10 * a),
            Hsla::from(rgb(0xFF_FF_FF)).opacity(0.32 * a),
        )
    };
    fill_round(window, r.x, r.y, r.w, r.h, radius, base);
    window.paint_quad(quad(
        bounds(point(px(r.x), px(r.y)), size(px(r.w), px(r.h))),
        Corners::all(px(radius)),
        linear_gradient(
            135.0,
            linear_color_stop(sheen, 0.0),
            linear_color_stop(Hsla::from(rgb(0x00_00_00)).opacity(0.0), 0.65),
        ),
        Edges::all(px(1.0)),
        border,
        BorderStyle::Solid,
    ));
    let icon_sz = (r.w.min(r.h) * 0.36).clamp(16.0, 96.0);
    let (cx_r, cy_r) = r.centre();
    super::icons::paint(
        window,
        cx,
        super::icons::Icon::MusicNote,
        super::model::Box4::new(cx_r - icon_sz * 0.5, cy_r - icon_sz * 0.5, icon_sz, icon_sz),
        note,
    );
}
