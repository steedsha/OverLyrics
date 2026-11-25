//! SVG geometry reused from OverLyricsLite/src/windows/lyrics/icons.tsx.
use super::model::Box4;
use gpui_kit::*;
#[derive(Clone, Copy)]
pub enum Icon {
    Play,
    Pause,
    Prev,
    Next,
    Volume,
    More,
    List,
    Sync,
    Settings,
    Drawer,
    Close,
    Lyrics,
    Refresh,
    MotionCover,
    Minimize,
    Fullscreen,
    FullscreenExit,
    Plus,
    ArrowLeft,
    CoverExpand,
    CoverCollapse,
    MusicNote,
}
pub fn paint(window: &mut Window, cx: &App, icon: Icon, r: Box4, color: Hsla) {
    paint_transformed(window, cx, icon, r, color, TransformationMatrix::unit());
}
fn paint_transformed(
    window: &mut Window,
    cx: &App,
    icon: Icon,
    r: Box4,
    color: Hsla,
    transform: TransformationMatrix,
) {
    let (name, data): (&str, &[u8]) = match icon {
        Icon::Play => (
            "legacy/play.svg",
            include_bytes!("../../assets/icons/play.svg"),
        ),
        Icon::Pause => (
            "legacy/pause.svg",
            include_bytes!("../../assets/icons/pause.svg"),
        ),
        Icon::Prev => (
            "legacy/prev.svg",
            include_bytes!("../../assets/icons/prev.svg"),
        ),
        Icon::Next => (
            "legacy/next.svg",
            include_bytes!("../../assets/icons/next.svg"),
        ),
        Icon::Volume => (
            "legacy/volume.svg",
            include_bytes!("../../assets/icons/volume.svg"),
        ),
        Icon::List => (
            "legacy/list.svg",
            include_bytes!("../../assets/icons/list.svg"),
        ),
        Icon::Sync => (
            "legacy/sync.svg",
            include_bytes!("../../assets/icons/sync.svg"),
        ),
        Icon::Settings => (
            "legacy/settings.svg",
            include_bytes!("../../assets/icons/settings.svg"),
        ),
        Icon::Drawer => (
            "legacy/drawer.svg",
            include_bytes!("../../assets/icons/drawer.svg"),
        ),
        Icon::More => (
            "legacy/more.svg",
            include_bytes!("../../assets/icons/more.svg"),
        ),
        Icon::Close => (
            "legacy/close.svg",
            include_bytes!("../../assets/icons/close.svg"),
        ),
        Icon::Lyrics => (
            "legacy/lyrics.svg",
            include_bytes!("../../assets/icons/lyrics.svg"),
        ),
        Icon::Refresh => (
            "legacy/refresh.svg",
            include_bytes!("../../assets/icons/refresh.svg"),
        ),
        Icon::MotionCover => (
            "legacy/motion_cover.svg",
            include_bytes!("../../assets/icons/motion_cover.svg"),
        ),
        Icon::Minimize => (
            "legacy/minimize.svg",
            include_bytes!("../../assets/icons/minimize.svg"),
        ),
        Icon::Fullscreen => (
            "legacy/fullscreen.svg",
            include_bytes!("../../assets/icons/fullscreen.svg"),
        ),
        Icon::FullscreenExit => (
            "legacy/fullscreen_exit.svg",
            include_bytes!("../../assets/icons/fullscreen_exit.svg"),
        ),
        Icon::Plus => (
            "legacy/plus.svg",
            include_bytes!("../../assets/icons/plus.svg"),
        ),
        Icon::ArrowLeft => (
            "legacy/arrow_left.svg",
            include_bytes!("../../assets/icons/arrow_left.svg"),
        ),
        Icon::CoverExpand => (
            "legacy/cover_expand.svg",
            include_bytes!("../../assets/icons/cover_expand.svg"),
        ),
        Icon::CoverCollapse => (
            "legacy/cover_collapse.svg",
            include_bytes!("../../assets/icons/cover_collapse.svg"),
        ),
        Icon::MusicNote => (
            "legacy/music_note.svg",
            include_bytes!("../../assets/icons/music_note.svg"),
        ),
    };
    if let Err(error) = window.paint_svg(
        bounds(point(px(r.x), px(r.y)), size(px(r.w), px(r.h))),
        name.into(),
        Some(data),
        transform,
        color,
        cx,
    ) {
        log::warn!("legacy icon {name}: {error:#}");
    }
}

fn affine_around(
    r: Box4,
    sx: f32,
    sy: f32,
    theta: f32,
    offset: [f32; 2],
    scale: f32,
) -> TransformationMatrix {
    let (cos, sin) = (theta.cos(), theta.sin());
    let a = sx * cos;
    let b = -sy * sin;
    let c = sx * sin;
    let d = sy * cos;
    let cx_d = (r.x + r.w * 0.5) * scale;
    let cy_d = (r.y + r.h * 0.5) * scale;
    TransformationMatrix {
        rotation_scale: [[a, b], [c, d]],
        translation: [
            cx_d - (a * cx_d + b * cy_d) + offset[0] * scale,
            cy_d - (c * cx_d + d * cy_d) + offset[1] * scale,
        ],
    }
}

pub fn paint_elastic(
    window: &mut Window,
    cx: &App,
    icon: Icon,
    r: Box4,
    color: Hsla,
    f: super::interaction::Feedback,
) {
    paint_elastic_rotated(window, cx, icon, r, color, f, 0.0);
}

pub fn paint_elastic_rotated(
    window: &mut Window,
    cx: &App,
    icon: Icon,
    r: Box4,
    color: Hsla,
    f: super::interaction::Feedback,
    theta_rad: f32,
) {
    // Deform the sprite, keeping SVG raster dimensions and atlas entries stable.
    let hop = f.hop.clamp(-0.08, 0.16);
    let sx = (1.0 + f.squeeze[0] * 0.35 + hop * 0.72).max(0.5);
    let sy = (1.0 + f.squeeze[1] * 0.35 + hop * 0.72).max(0.5);
    let lift_y = -hop * (r.h * 0.11).clamp(1.4, 3.6);
    let scale = window.scale_factor();
    let transform = affine_around(
        r,
        sx,
        sy,
        theta_rad,
        [f.pull[0] * 0.65, f.pull[1] * 0.65 + lift_y],
        scale,
    );
    paint_transformed(window, cx, icon, r, color, transform);
}

/// Gentle crossfade transition between two state icons (e.g., Play <-> Pause).
pub fn paint_toggle(
    window: &mut Window,
    cx: &App,
    off_icon: Icon,
    on_icon: Icon,
    r: Box4,
    color: Hsla,
    f: super::interaction::Feedback,
    morph: f32,
) {
    // State changes should not multiply the ordinary hover hop.
    let f = super::interaction::Feedback { hop: f.hop * 0.35, ..f };
    if (morph - 0.0).abs() < 0.003 {
        paint_elastic(window, cx, off_icon, r, color, f);
        return;
    }
    if (morph - 1.0).abs() < 0.003 {
        paint_elastic(window, cx, on_icon, r, color, f);
        return;
    }
    let p = morph.clamp(0.0, 1.0);
    let over = (morph - p).clamp(-0.03, 0.03);
    let hop = f.hop.clamp(-0.08, 0.16);
    let lift_y = -hop * (r.h * 0.11).clamp(1.4, 3.6);
    let scale = window.scale_factor();

    let off_a = 1.0 - p;
    if off_a > 0.01 {
        let bounce = if over < 0.0 { -over * 0.2 } else { 0.0 };
        let base = (0.94 + 0.06 * (1.0 - p) + bounce + hop * 0.72).max(0.42);
        let sx = (base * (1.0 + f.squeeze[0] * 0.35) + over.abs() * 0.12).max(0.4);
        let sy = (base * (1.0 + f.squeeze[1] * 0.35) - over.abs() * 0.08).max(0.4);
        let theta = -0.08 * p - over * 0.1;
        let transform = affine_around(
            r,
            sx,
            sy,
            theta,
            [f.pull[0] * 0.65, f.pull[1] * 0.65 + lift_y],
            scale,
        );
        let c = Hsla {
            a: color.a * off_a,
            ..color
        };
        paint_transformed(window, cx, off_icon, r, c, transform);
    }

    let on_a = p;
    if on_a > 0.01 {
        let bounce = if over > 0.0 { over * 0.2 } else { 0.0 };
        let base = (0.94 + 0.06 * p + bounce + hop * 0.72).max(0.42);
        let sx = (base * (1.0 + f.squeeze[0] * 0.35) - over.abs() * 0.08).max(0.4);
        let sy = (base * (1.0 + f.squeeze[1] * 0.35) + over.abs() * 0.12).max(0.4);
        let theta = 0.08 * (1.0 - p) - over * 0.1;
        let transform = affine_around(
            r,
            sx,
            sy,
            theta,
            [f.pull[0] * 0.65, f.pull[1] * 0.65 + lift_y],
            scale,
        );
        let c = Hsla {
            a: color.a * on_a,
            ..color
        };
        paint_transformed(window, cx, on_icon, r, c, transform);
    }
}
