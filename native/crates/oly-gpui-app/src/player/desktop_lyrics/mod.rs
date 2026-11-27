//! Borderless, transparent, always-on-top single-line Desktop Lyrics window.
//!
//! Features:
//! - Native AMLL animation pipeline (`rows::Shaper`, `paint::paint_row_with_shadow`,
//!   `oly_render::emphasis`, `oly_render::mask`, and `InterludeDots`).
//! - Horizontal & Vertical single-line lyrics + optional translation (enabled by default).
//! - Vertical lyrics stack CJK upright and rotate Latin/English runs 90° clockwise
//!   with constant font size, progress-driven smooth scrolling, and outer-edge main placement.
//! - Borderless 7-button elastic hover bar (`[Volume, Prev, Play/Pause, Next, Translate, Lock, More]`)
//!   with compact outer hover frame and slow smooth push-apart / return animation for lyrics.
//! - Independent Liquid Glass popovers reusing the app's native glass frame, `PanelMotion`,
//!   `chrome.rs` widgets, shared Volume renderer, shared Lyrics Version Picker renderer,
//!   curated color dropdown list, and GPU HSV custom color palette.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI8, Ordering},
};
use std::time::Instant;

use gpui_kit::*;
use oly_core::interlude::InterludeDots;
use oly_core::scroll::{Spring, SpringParams};
use oly_render::emphasis::{PlainFloat, sample_float, sample_glow};
use oly_render::glass::Rect as GlassRect;
use oly_render::mask::position_at;

use super::chrome::{self, SegmentedLens};
use super::elastic::{Elastic, PressGesture};
use super::icons::{self, Icon};
use super::interaction::{Feedback, state_icon_spring};
use super::model::{Box4, Display, Row, ThemeMode, row_light};
use super::paint::{self, LineAlign, RowLight, WrapperTransform};
use super::panel_motion::PanelMotion;
use super::rows::{RowLines, Shaper};
use super::view::{
    PickerEntry, VERSION_HEADER, VERSION_ROW, paint_lyrics_picker_contents,
    paint_sidecar_glass_frame, paint_volume_panel_contents, picker_import_rect,
    picker_refresh_rect, picker_smart_btn_rect,
};
use super::win32_window::{self, LockHotkeyListener};

pub const LONG_DEFAULT: f32 = 800.0;
pub const LONG_MIN: f32 = 560.0;
pub const LONG_MAX: f32 = 1600.0;
pub const SHORT_DEFAULT: f32 = 144.0;
pub const SHORT_MIN: f32 = 120.0;
pub const SHORT_MAX: f32 = 480.0;

pub const HORIZ_W: f32 = LONG_DEFAULT;
pub const HORIZ_H: f32 = SHORT_DEFAULT;
pub const HORIZ_MIN_W: f32 = LONG_MIN;
pub const HORIZ_MIN_H: f32 = SHORT_MIN;
pub const HORIZ_MAX_W: f32 = LONG_MAX;
pub const HORIZ_MAX_H: f32 = SHORT_MAX;
pub const VERT_W: f32 = SHORT_DEFAULT;
pub const VERT_H: f32 = LONG_DEFAULT;
pub const VERT_MIN_W: f32 = SHORT_MIN;
pub const VERT_MIN_H: f32 = LONG_MIN;
pub const VERT_MAX_W: f32 = SHORT_MAX;
pub const VERT_MAX_H: f32 = LONG_MAX;
pub const MIN_FONT: f32 = 20.0;
pub const MAX_FONT: f32 = 64.0;
const DESKTOP_DROP_SHADOW: f32 = 0.68;
const POPOVER_ELASTIC_PAD: f32 = 12.0;

/// Curated desktop lyrics text color presets (no black, no cluttered duplicates).
pub const COLOR_PRESETS: &[(u32, &str)] = &[
    (0xF4F6F8, "纯白"),
    (0xCBEAFF, "冰川蓝"),
    (0x7FD6C0, "薄荷青"),
    (0xFFE59E, "琥珀金"),
    (0xFFD1DC, "樱花粉"),
];

pub fn color_preset_label(rgb: u32) -> String {
    let rgb = rgb & 0x00FF_FFFF;
    if let Some((_, name)) = COLOR_PRESETS.iter().find(|(c, _)| *c == rgb) {
        (*name).to_string()
    } else {
        format!("#{:06X}", rgb)
    }
}

pub use oly_ui::color::{hsv_to_rgb, is_dark_color, rgb_to_hsv, smoothstep01};

mod bridge;
mod config;
mod popovers;
mod vertical;
mod view;

pub use bridge::*;
pub use config::*;
pub use vertical::*;
pub use view::*;
use popovers::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[::std::prelude::v1::test]
    fn vertical_tokenizer_keeps_cjk_upright_and_groups_latin_rotated_runs() {
        let row = Row {
            group: 0,
            text: "你好 Hi!".into(),
            words: vec![
                ("你好 ".into(), 0, 400),
                ("Hi!".into(), 400, 900),
            ],
            translation: String::new(),
            romanization: String::new(),
            start_ms: 0.0,
            end_ms: 900.0,
            word_timed: true,
            is_bg: false,
            is_duet: false,
        };
        let toks = tokenize_vertical_row(&row);
        assert_eq!(toks.len(), 4);
        assert!(matches!(toks[0], VerticalToken::UprightChar { ch: '你', word_source: 0, .. }));
        assert!(matches!(toks[1], VerticalToken::UprightChar { ch: '好', word_source: 0, .. }));
        assert!(matches!(toks[2], VerticalToken::Space));
        assert!(matches!(
            &toks[3],
            VerticalToken::RotatedRun { text, word_source: 1, .. } if text == "Hi!"
        ));
    }

    #[::std::prelude::v1::test]
    fn vertical_overflow_scroll_tracks_word_and_line_progress() {
        let toks: Vec<VerticalToken> = (0..10)
            .map(|i| VerticalToken::UprightChar {
                ch: '字',
                word_source: i,
                char_index: 0,
                start_ms: (i as f64) * 200.0,
                end_ms: ((i + 1) as f64) * 200.0,
            })
            .collect();
        let steps = vec![40.0_f32; 10];
        // Short line fits inside viewport -> zero scroll
        assert_eq!(
            compute_vertical_scroll_target(&toks, &steps, 400.0, 500.0, 1000.0, 0.0, 2000.0, true),
            0.0
        );
        // Long line overflows -> scrolls from 0 toward max_scroll (200.0)
        let s_start =
            compute_vertical_scroll_target(&toks, &steps, 400.0, 200.0, 0.0, 0.0, 2000.0, true);
        let s_mid =
            compute_vertical_scroll_target(&toks, &steps, 400.0, 200.0, 1200.0, 0.0, 2000.0, true);
        let s_end =
            compute_vertical_scroll_target(&toks, &steps, 400.0, 200.0, 2000.0, 0.0, 2000.0, true);
        assert_eq!(s_start, 0.0);
        assert!(s_mid > 0.0 && s_mid < 200.0);
        assert!((s_end - 200.0).abs() < 0.01);
    }

    #[::std::prelude::v1::test]
    fn desktop_config_roundtrips_json_patch() {
        let cfg = DesktopLyricsConfig {
            orientation: DesktopOrientation::Vertical,
            locked: true,
            font_size: 40.0,
            text_color: 0xCBEAFF,
            show_translation: false,
            auto_scroll: false,
            display_lines: 3,
            horizontal_pos: Some((120.0, 900.0)),
            vertical_pos: Some((1740.0, 180.0)),
            horizontal_size: Some((960.0, 180.0)),
            vertical_size: Some((190.0, 720.0)),
        };
        let parsed = DesktopLyricsConfig::from_config(&cfg.to_patch());
        assert_eq!(parsed, cfg);
    }

    #[::std::prelude::v1::test]
    fn hsv_rgb_roundtrip_preserves_colors() {
        for &(hex, _) in COLOR_PRESETS {
            let (h, s, v) = rgb_to_hsv(hex);
            let out = hsv_to_rgb(h, s, v);
            let dr = (((hex >> 16) & 0xFF) as i32 - ((out >> 16) & 0xFF) as i32).abs();
            let dg = (((hex >> 8) & 0xFF) as i32 - ((out >> 8) & 0xFF) as i32).abs();
            let db = ((hex & 0xFF) as i32 - (out & 0xFF) as i32).abs();
            assert!(dr <= 1 && dg <= 1 && db <= 1, "hex={hex:#06X} out={out:#06X}");
        }
    }

    #[::std::prelude::v1::test]
    fn snap_center_magnetizes_near_centerline_and_releases_outside_hysteresis() {
        let (x, sx) = snap_axis(492.0, 960.0, 0.0, 1920.0, false);
        assert!(sx);
        assert!((x - 480.0).abs() < 0.01);

        let (x2, sx2) = snap_axis(495.0, 960.0, 0.0, 1920.0, true);
        assert!(sx2);
        assert!((x2 - 480.0).abs() < 0.01);

        let (x3, sx3) = snap_axis(505.0, 960.0, 0.0, 1920.0, true);
        assert!(!sx3);
        assert!((x3 - 505.0).abs() < 0.01);
    }

    #[::std::prelude::v1::test]
    fn snap_edges_magnetize_gently_and_allow_overhang_past_threshold() {
        let (lx, ls) = snap_axis(9.0, 800.0, 0.0, 1920.0, false);
        assert!(ls);
        assert!((lx - 0.0).abs() < 0.01);

        let (lx_hold, ls_hold) = snap_axis(-14.0, 800.0, 0.0, 1920.0, true);
        assert!(ls_hold);
        assert!((lx_hold - 0.0).abs() < 0.01);

        let (lx_free, ls_free) = snap_axis(-28.0, 800.0, 0.0, 1920.0, true);
        assert!(!ls_free);
        assert!((lx_free - (-28.0)).abs() < 0.01);

        let (rx, rs) = snap_axis(1111.0, 800.0, 0.0, 1920.0, false);
        assert!(rs);
        assert!((rx - 1120.0).abs() < 0.01);

        let (rx_free, rs_free) = snap_axis(1145.0, 800.0, 0.0, 1920.0, true);
        assert!(!rs_free);
        assert!((rx_free - 1145.0).abs() < 0.01);
    }

    #[::std::prelude::v1::test]
    fn shared_desktop_bridge_read_snapshot_reuses_arc() {
        let bridge = SharedDesktopBridge::new();
        let mut snap = DesktopSnapshot::default();
        snap.track_title = "ZeroCopy".into();
        bridge.update_snapshot(snap);

        let a = bridge.read_snapshot();
        let b = bridge.read_snapshot();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.track_title, "ZeroCopy");
    }
}