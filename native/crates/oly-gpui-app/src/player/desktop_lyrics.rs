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
pub const SHORT_MAX: f32 = 320.0;

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

fn is_dark_color(rgb: u32) -> bool {
    let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
    let b = (rgb & 0xFF) as f32 / 255.0;
    (0.2126 * r + 0.7152 * g + 0.0722 * b) < 0.35
}

pub fn rgb_to_hsv(rgb: u32) -> (f32, f32, f32) {
    let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
    let b = (rgb & 0xFF) as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta <= 1e-6 {
        0.0
    } else if (max - r).abs() <= 1e-6 {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if (max - g).abs() <= 1e-6 {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    let s = if max <= 1e-6 { 0.0 } else { delta / max };
    (h.clamp(0.0, 360.0), s.clamp(0.0, 1.0), max.clamp(0.0, 1.0))
}

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> u32 {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let r = ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    let g = ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    let b = ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    (r << 16) | (g << 8) | b
}

fn smoothstep01(t: f32) -> f32 {
    let s = t.clamp(0.0, 1.0);
    s * s * (3.0 - 2.0 * s)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopOrientation {
    Horizontal,
    Vertical,
}

impl DesktopOrientation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("vertical") {
            Self::Vertical
        } else {
            Self::Horizontal
        }
    }

    pub fn window_size(self) -> (f32, f32) {
        match self {
            Self::Horizontal => (HORIZ_W, HORIZ_H),
            Self::Vertical => (VERT_W, VERT_H),
        }
    }

    pub fn min_window_size(self) -> (f32, f32) {
        match self {
            Self::Horizontal => (HORIZ_MIN_W, HORIZ_MIN_H),
            Self::Vertical => (VERT_MIN_W, VERT_MIN_H),
        }
    }

    pub fn max_window_size(self) -> (f32, f32) {
        match self {
            Self::Horizontal => (HORIZ_MAX_W, HORIZ_MAX_H),
            Self::Vertical => (VERT_MAX_W, VERT_MAX_H),
        }
    }

    pub fn max_window_size_for_display(self, disp_w: f32, disp_h: f32) -> (f32, f32) {
        let (min_w, min_h) = self.min_window_size();
        let (max_w, max_h) = self.max_window_size();
        let lim_w = (disp_w * 0.92).max(min_w).min(max_w);
        let lim_h = (disp_h * 0.92).max(min_h).min(max_h);
        (lim_w, lim_h)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopLyricsConfig {
    pub orientation: DesktopOrientation,
    pub locked: bool,
    pub font_size: f32,
    pub text_color: u32,
    pub show_translation: bool,
    pub auto_scroll: bool,
    pub horizontal_pos: Option<(f32, f32)>,
    pub vertical_pos: Option<(f32, f32)>,
    pub horizontal_size: Option<(f32, f32)>,
    pub vertical_size: Option<(f32, f32)>,
}

impl Default for DesktopLyricsConfig {
    fn default() -> Self {
        Self {
            orientation: DesktopOrientation::Horizontal,
            locked: false,
            font_size: 36.0,
            text_color: COLOR_PRESETS[0].0,
            show_translation: true,
            auto_scroll: true,
            horizontal_pos: None,
            vertical_pos: None,
            horizontal_size: None,
            vertical_size: None,
        }
    }
}

impl DesktopLyricsConfig {
    pub fn from_config(root: &serde_json::Value) -> Self {
        let mut cfg = Self::default();
        let Some(obj) = root.get("desktopLyrics").and_then(|v| v.as_object()) else {
            return cfg;
        };
        if let Some(o) = obj.get("orientation").and_then(|v| v.as_str()) {
            cfg.orientation = DesktopOrientation::parse(o);
        }
        if let Some(l) = obj.get("locked").and_then(|v| v.as_bool()) {
            cfg.locked = l;
        }
        if let Some(fs) = obj.get("fontSize").and_then(|v| v.as_f64()) {
            if fs.is_finite() {
                cfg.font_size = (fs as f32).clamp(MIN_FONT, MAX_FONT);
            }
        }
        if let Some(hex) = obj.get("textColor").and_then(|v| v.as_str()) {
            let trimmed = hex.trim().trim_start_matches('#');
            if let Ok(parsed) = u32::from_str_radix(trimmed, 16) {
                cfg.text_color = parsed & 0x00FF_FFFF;
            }
        }
        if let Some(st) = obj.get("showTranslation").and_then(|v| v.as_bool()) {
            cfg.show_translation = st;
        }
        if let Some(ascr) = obj.get("autoScroll").and_then(|v| v.as_bool()) {
            cfg.auto_scroll = ascr;
        }
        let read_pos = |key: &str| -> Option<(f32, f32)> {
            let p = obj.get(key)?.as_object()?;
            let x = p.get("x")?.as_f64()? as f32;
            let y = p.get("y")?.as_f64()? as f32;
            (x.is_finite() && y.is_finite()).then_some((x, y))
        };
        let read_size = |key: &str, orient: DesktopOrientation| -> Option<(f32, f32)> {
            let p = obj.get(key)?.as_object()?;
            let w = p.get("w")?.as_f64()? as f32;
            let h = p.get("h")?.as_f64()? as f32;
            if !w.is_finite() || !h.is_finite() {
                return None;
            }
            let (min_w, min_h) = orient.min_window_size();
            let (max_w, max_h) = orient.max_window_size();
            Some((w.clamp(min_w, max_w), h.clamp(min_h, max_h)))
        };
        cfg.horizontal_pos = read_pos("horizontalPos");
        cfg.vertical_pos = read_pos("verticalPos");
        cfg.horizontal_size = read_size("horizontalSize", DesktopOrientation::Horizontal);
        cfg.vertical_size = read_size("verticalSize", DesktopOrientation::Vertical);
        cfg
    }

    pub fn to_patch(&self) -> serde_json::Value {
        let pos_json = |p: Option<(f32, f32)>| match p {
            Some((x, y)) => serde_json::json!({ "x": x.round(), "y": y.round() }),
            None => serde_json::Value::Null,
        };
        let size_json = |s: Option<(f32, f32)>| match s {
            Some((w, h)) => serde_json::json!({ "w": w.round(), "h": h.round() }),
            None => serde_json::Value::Null,
        };
        serde_json::json!({
            "desktopLyrics": {
                "orientation": self.orientation.as_str(),
                "locked": self.locked,
                "fontSize": self.font_size.round(),
                "textColor": format!("#{:06X}", self.text_color & 0x00FF_FFFF),
                "showTranslation": self.show_translation,
                "autoScroll": self.auto_scroll,
                "horizontalPos": pos_json(self.horizontal_pos),
                "verticalPos": pos_json(self.vertical_pos),
                "horizontalSize": size_json(self.horizontal_size),
                "verticalSize": size_json(self.vertical_size),
            }
        })
    }

    pub fn window_size(&self) -> (f32, f32) {
        let (def_w, def_h) = self.orientation.window_size();
        let (min_w, min_h) = self.orientation.min_window_size();
        let (max_w, max_h) = self.orientation.max_window_size();
        let saved = match self.orientation {
            DesktopOrientation::Horizontal => self
                .horizontal_size
                .or_else(|| self.vertical_size.map(|(vw, vh)| (vh, vw))),
            DesktopOrientation::Vertical => self
                .vertical_size
                .or_else(|| self.horizontal_size.map(|(hw, hh)| (hh, hw))),
        };
        match saved {
            Some((w, h)) => (w.clamp(min_w, max_w), h.clamp(min_h, max_h)),
            None => (def_w, def_h),
        }
    }

    pub fn set_size(&mut self, w: f32, h: f32) {
        let (min_w, min_h) = self.orientation.min_window_size();
        let (max_w, max_h) = self.orientation.max_window_size();
        let clamped = (w.clamp(min_w, max_w), h.clamp(min_h, max_h));
        // Keep horizontal and vertical sizes unified by mirroring (long, short) across orientations.
        match self.orientation {
            DesktopOrientation::Horizontal => {
                self.horizontal_size = Some(clamped);
                self.vertical_size = Some((
                    clamped.1.clamp(VERT_MIN_W, VERT_MAX_W),
                    clamped.0.clamp(VERT_MIN_H, VERT_MAX_H),
                ));
            }
            DesktopOrientation::Vertical => {
                self.vertical_size = Some(clamped);
                self.horizontal_size = Some((
                    clamped.1.clamp(HORIZ_MIN_W, HORIZ_MAX_W),
                    clamped.0.clamp(HORIZ_MIN_H, HORIZ_MAX_H),
                ));
            }
        }
    }

    pub fn saved_pos(&self) -> Option<(f32, f32)> {
        match self.orientation {
            DesktopOrientation::Horizontal => self.horizontal_pos,
            DesktopOrientation::Vertical => self.vertical_pos,
        }
    }

    pub fn set_pos(&mut self, x: f32, y: f32) {
        match self.orientation {
            DesktopOrientation::Horizontal => self.horizontal_pos = Some((x, y)),
            DesktopOrientation::Vertical => self.vertical_pos = Some((x, y)),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct DesktopSnapshot {
    pub playing: bool,
    pub track_title: String,
    pub track_artist: String,
    pub display: Display,
    pub position_ms: f64,
    pub active_row: Option<Row>,
    pub active_row_index: Option<usize>,
    pub interlude_range: Option<(f64, f64)>,
    pub sync_offset_ms: i64,
    pub volume_pct: f64,
    pub volume_enabled: bool,
    pub volume_status: String,
    pub volume_label: String,
    pub candidates: Arc<Vec<PickerEntry>>,
    pub refresh_searching: bool,
}

impl Default for DesktopSnapshot {
    fn default() -> Self {
        Self {
            playing: false,
            track_title: String::new(),
            track_artist: String::new(),
            display: Display::default(),
            position_ms: 0.0,
            active_row: None,
            active_row_index: None,
            interlude_range: None,
            sync_offset_ms: 0,
            volume_pct: 100.0,
            volume_enabled: true,
            volume_status: "100%".into(),
            volume_label: "播放器内部音量".into(),
            candidates: Arc::new(Vec::new()),
            refresh_searching: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum DesktopCommand {
    Prev,
    PlayPause,
    Next,
    SyncNudge(i64),
    OpenVolume,
    SetVolumePct(f64),
    PickCandidate(usize),
    ToggleSmartTranslation(usize),
    RefreshLyrics,
    ImportLyrics,
    ReturnToMain,
    Close,
    SaveConfig(DesktopLyricsConfig),
}

#[derive(Clone, Debug)]
pub enum PopoverAction {
    SetOrientation(DesktopOrientation),
    FontDelta(f32),
    SetColor(u32),
    ToggleAutoScroll,
    OpenColorPopover(Box4, f32),
    CloseColorPopover,
    OpenLyricsPicker,
    ReturnToMain,
}

#[derive(Default)]
pub struct SharedDesktopBridge {
    snapshot: Mutex<Arc<DesktopSnapshot>>,
    commands: Mutex<Vec<DesktopCommand>>,
    cfg: Mutex<DesktopLyricsConfig>,
    popover_actions: Mutex<Vec<PopoverAction>>,
    pub color_popover_open: AtomicBool,
    /// `0` = closed, `1` = opened on right side, `-1` = opened on left side.
    pub color_popover_side: AtomicI8,
    volume_closed_at: Mutex<Option<Instant>>,
    more_closed_at: Mutex<Option<Instant>>,
    color_closed_at: Mutex<Option<Instant>>,
}

impl SharedDesktopBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn mark_closed(slot: &Mutex<Option<Instant>>) {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    fn take_recently_closed(slot: &Mutex<Option<Instant>>, window_ms: u128) -> bool {
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        let recent = g.is_some_and(|t| t.elapsed().as_millis() <= window_ms);
        if recent {
            *g = None;
        }
        recent
    }

    pub fn mark_volume_closed(&self) {
        Self::mark_closed(&self.volume_closed_at);
    }

    pub fn mark_more_closed(&self) {
        Self::mark_closed(&self.more_closed_at);
    }

    pub fn mark_color_closed(&self) {
        self.color_popover_open.store(false, Ordering::SeqCst);
        self.color_popover_side.store(0, Ordering::SeqCst);
        Self::mark_closed(&self.color_closed_at);
    }

    pub fn was_volume_recently_closed(&self) -> bool {
        Self::take_recently_closed(&self.volume_closed_at, 260)
    }

    pub fn was_more_recently_closed(&self) -> bool {
        Self::take_recently_closed(&self.more_closed_at, 260)
    }

    pub fn was_color_recently_closed(&self) -> bool {
        Self::take_recently_closed(&self.color_closed_at, 260)
    }

    pub fn push(&self, cmd: DesktopCommand) {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(cmd);
    }

    pub fn drain_commands(&self) -> Vec<DesktopCommand> {
        let mut q = self.commands.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    }

    pub fn push_popover(&self, act: PopoverAction) {
        self.popover_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(act);
    }

    pub fn drain_popover(&self) -> Vec<PopoverAction> {
        let mut q = self
            .popover_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    }

    pub fn set_cfg(&self, cfg: DesktopLyricsConfig) {
        *self.cfg.lock().unwrap_or_else(|e| e.into_inner()) = cfg;
    }

    pub fn read_cfg(&self) -> DesktopLyricsConfig {
        self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(super) fn update_snapshot(&self, snap: DesktopSnapshot) {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(snap);
    }

    pub(super) fn read_snapshot(&self) -> Arc<DesktopSnapshot> {
        Arc::clone(&self.snapshot.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Vertical layout token: upright CJK/symbol glyph, 90° CW rotated Latin run, or space.
#[derive(Clone, Debug, PartialEq)]
pub enum VerticalToken {
    UprightChar {
        ch: char,
        word_source: usize,
        char_index: usize,
        start_ms: f64,
        end_ms: f64,
    },
    RotatedRun {
        text: String,
        word_source: usize,
        start_ms: f64,
        end_ms: f64,
    },
    Space,
}

fn is_latin_vertical_char(ch: char) -> bool {
    ch.is_ascii_graphic() || (ch.is_alphabetic() && matches!(ch as u32, 0x00C0..=0x024F))
}

fn map_vertical_punct(ch: char) -> char {
    match ch {
        '(' | '（' => '︵',
        ')' | '）' => '︶',
        '「' => '﹁',
        '」' => '﹂',
        '『' => '﹃',
        '』' => '﹄',
        '【' => '︻',
        '】' => '︼',
        '《' => '︽',
        '》' => '︾',
        '〈' => '︿',
        '〉' => '﹀',
        '—' | '–' => '｜',
        '…' => '⋮',
        '~' | '～' => '︱',
        other => other,
    }
}

/// Tokenizes a lyric row into upright CJK chars and 90° clockwise rotated Latin/English runs.
pub fn tokenize_vertical_row(row: &Row) -> Vec<VerticalToken> {
    let mut tokens = Vec::new();
    let words: Vec<(usize, &str, f64, f64)> = if row.words.is_empty() {
        vec![(0, row.text.as_str(), row.start_ms, row.end_ms)]
    } else {
        row.words
            .iter()
            .enumerate()
            .map(|(idx, (t, s, e))| (idx, t.as_str(), *s as f64, *e as f64))
            .collect()
    };

    for (word_source, text, w_start, w_end) in words {
        let chars: Vec<char> = text.chars().collect();
        let total = chars.len().max(1) as f64;
        let span = (w_end - w_start).max(0.0);
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            if ch.is_whitespace() {
                if !matches!(tokens.last(), Some(VerticalToken::Space)) {
                    tokens.push(VerticalToken::Space);
                }
                i += 1;
            } else if is_latin_vertical_char(ch) {
                let start_i = i;
                let mut run = String::new();
                while i < chars.len() && is_latin_vertical_char(chars[i]) {
                    run.push(chars[i]);
                    i += 1;
                }
                let s = w_start + span * (start_i as f64 / total);
                let e = w_start + span * (i as f64 / total);
                tokens.push(VerticalToken::RotatedRun {
                    text: run,
                    word_source,
                    start_ms: s,
                    end_ms: e,
                });
            } else {
                let mapped = map_vertical_punct(ch);
                let s = w_start + span * (i as f64 / total);
                let e = w_start + span * ((i + 1) as f64 / total);
                tokens.push(VerticalToken::UprightChar {
                    ch: mapped,
                    word_source,
                    char_index: i,
                    start_ms: s,
                    end_ms: e,
                });
                i += 1;
            }
        }
    }
    while matches!(tokens.first(), Some(VerticalToken::Space)) {
        tokens.remove(0);
    }
    while matches!(tokens.last(), Some(VerticalToken::Space)) {
        tokens.pop();
    }
    tokens
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Btn {
    Lock,
    Volume,
    Prev,
    Play,
    Next,
    Translate,
    More,
}

impl Btn {
    const COUNT: usize = 7;

    fn index(self) -> usize {
        match self {
            Btn::Lock => 0,
            Btn::Volume => 1,
            Btn::Prev => 2,
            Btn::Play => 3,
            Btn::Next => 4,
            Btn::Translate => 5,
            Btn::More => 6,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DragState {
    start_cursor_phys: (i32, i32),
    start_origin_logical: (f32, f32),
    snapped_x: bool,
    snapped_y: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResizeEdges {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl ResizeEdges {
    fn any(self) -> bool {
        self.left || self.right || self.top || self.bottom
    }

    fn cursor_style(self) -> Option<CursorStyle> {
        match (self.left, self.right, self.top, self.bottom) {
            (true, false, true, false) | (false, true, false, true) => {
                Some(CursorStyle::ResizeUpLeftDownRight)
            }
            (false, true, true, false) | (true, false, false, true) => {
                Some(CursorStyle::ResizeUpRightDownLeft)
            }
            (true, false, false, false) | (false, true, false, false) => {
                Some(CursorStyle::ResizeLeftRight)
            }
            (false, false, true, false) | (false, false, false, true) => {
                Some(CursorStyle::ResizeUpDown)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ResizeState {
    edges: ResizeEdges,
    start_cursor_phys: (i32, i32),
    start_rect_phys: (i32, i32, i32, i32),
    last_applied_phys: (i32, i32, i32, i32),
}

struct VertMeasureCache {
    text: String,
    font_bits: u32,
    weight_bits: u32,
    tokens: Vec<VerticalToken>,
    steps: Vec<f32>,
    total: f32,
}

impl VertMeasureCache {
    fn matches(&self, text: &str, font_size: f32, weight: f32) -> bool {
        self.text == text
            && self.font_bits == font_size.to_bits()
            && self.weight_bits == weight.to_bits()
    }
}

pub struct DesktopLyricsView {
    bridge: Arc<SharedDesktopBridge>,
    parent: WeakEntity<super::view::PlayerView>,
    cfg: DesktopLyricsConfig,
    shaper: Shaper,
    interlude_dots: InterludeDots,
    last_interlude: Option<(f64, f64)>,
    hotkey: LockHotkeyListener,
    last_hotkey_count: u64,
    configured_hwnd: bool,
    applied_click_through: Option<bool>,
    cursor: Option<(f32, f32)>,
    pressed_btn: Option<Btn>,
    btn_feedback: [Feedback; Btn::COUNT],
    btn_elastic: [Elastic; Btn::COUNT],
    press_gesture: PressGesture,
    play_morph: Spring,
    drag: Option<DragState>,
    resize: Option<ResizeState>,
    hover_alpha: f32,
    bar_side_blend: f32,
    bar_side_inited: bool,
    trans_right_blend: f32,
    trans_side_inited: bool,
    vert_main_scroll: f32,
    vert_trans_scroll: f32,
    horiz_main_scroll: f32,
    horiz_trans_scroll: f32,
    vert_main_cache: Option<VertMeasureCache>,
    vert_trans_cache: Option<VertMeasureCache>,
    pending_rect_phys: Arc<Mutex<Option<(isize, i32, i32, Option<(i32, i32)>)>>>,
    rect_apply_scheduled: Arc<AtomicBool>,
    leave_at_ms: Option<f64>,
    toast: Option<(String, f64)>,
    last_row_idx: Option<usize>,
    line_fade_start_ms: f64,
    last_origin: Option<(f32, f32)>,
    last_size: Option<(f32, f32)>,
    started: Instant,
    last_ms: f64,
    buttons: Vec<(Btn, Box4)>,
    volume_popover: Option<AnyWindowHandle>,
    more_popover: Option<AnyWindowHandle>,
    color_popover: Option<AnyWindowHandle>,
    picker_popover: Option<AnyWindowHandle>,
    last_more_anchor: Box4,
    bar_rect: Box4,
    idle_poll_pending: bool,
}

impl DesktopLyricsView {
    pub fn new(
        bridge: Arc<SharedDesktopBridge>,
        parent: WeakEntity<super::view::PlayerView>,
        cfg: DesktopLyricsConfig,
        text_system: Arc<TextSystem>,
    ) -> Self {
        bridge.set_cfg(cfg.clone());
        let hotkey = LockHotkeyListener::start();
        let last_hotkey_count = hotkey.counter.load(Ordering::SeqCst);
        Self {
            bridge,
            parent,
            cfg,
            shaper: Shaper::new(text_system, 32),
            interlude_dots: InterludeDots::new(),
            last_interlude: None,
            hotkey,
            last_hotkey_count,
            configured_hwnd: false,
            applied_click_through: None,
            cursor: None,
            pressed_btn: None,
            btn_feedback: [Feedback::default(); Btn::COUNT],
            btn_elastic: std::array::from_fn(|_| Elastic::default()),
            press_gesture: PressGesture::default(),
            play_morph: state_icon_spring(0.0),
            drag: None,
            resize: None,
            hover_alpha: 0.0,
            bar_side_blend: 0.0,
            bar_side_inited: false,
            trans_right_blend: 0.0,
            trans_side_inited: false,
            vert_main_scroll: 0.0,
            vert_trans_scroll: 0.0,
            horiz_main_scroll: 0.0,
            horiz_trans_scroll: 0.0,
            vert_main_cache: None,
            vert_trans_cache: None,
            pending_rect_phys: Arc::new(Mutex::new(None)),
            rect_apply_scheduled: Arc::new(AtomicBool::new(false)),
            leave_at_ms: None,
            toast: None,
            last_row_idx: None,
            line_fade_start_ms: 0.0,
            last_origin: None,
            last_size: None,
            started: Instant::now(),
            last_ms: 0.0,
            buttons: Vec::new(),
            volume_popover: None,
            more_popover: None,
            color_popover: None,
            picker_popover: None,
            last_more_anchor: Box4::default(),
            bar_rect: Box4::default(),
            idle_poll_pending: false,
        }
    }

    fn now_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    /// Coalesces window move/resize onto the next foreground executor tick so
    /// Win32 `SetWindowPos` -> `WM_SIZE`/`WM_MOVE` never re-enters `Window::draw`.
    fn schedule_window_rect_phys(
        &self,
        hwnd: isize,
        x: i32,
        y: i32,
        size: Option<(i32, i32)>,
        cx: &mut Context<Self>,
    ) {
        *self
            .pending_rect_phys
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((hwnd, x, y, size));
        if self.rect_apply_scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let pending = self.pending_rect_phys.clone();
        let scheduled = self.rect_apply_scheduled.clone();
        cx.foreground_executor()
            .spawn(async move {
                scheduled.store(false, Ordering::SeqCst);
                let target = pending.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some((h, px_x, px_y, sz)) = target {
                    if let Some((pw, ph)) = sz {
                        win32_window::set_window_rect_physical(h, px_x, px_y, pw, ph);
                    } else {
                        win32_window::move_window_physical(h, px_x, px_y);
                    }
                }
            })
            .detach();
    }

    fn close_color_popover(&mut self, cx: &mut App) {
        self.bridge.color_popover_open.store(false, Ordering::SeqCst);
        self.bridge.color_popover_side.store(0, Ordering::SeqCst);
        if let Some(h) = self.color_popover.take() {
            let _ = h.update(cx, |_, w, _| w.remove_window());
        }
    }

    fn close_popovers(&mut self, cx: &mut App) {
        self.close_color_popover(cx);
        if let Some(h) = self.volume_popover.take() {
            let _ = h.update(cx, |_, w, _| w.remove_window());
        }
        if let Some(h) = self.more_popover.take() {
            let _ = h.update(cx, |_, w, _| w.remove_window());
        }
        if let Some(h) = self.picker_popover.take() {
            let _ = h.update(cx, |_, w, _| w.remove_window());
        }
    }

    fn set_locked(&mut self, locked: bool, now: f64, cx: &mut App) {
        if locked {
            self.close_popovers(cx);
        }
        self.cfg.locked = locked;
        self.bridge.set_cfg(self.cfg.clone());
        self.toast = Some((
            if locked {
                "已锁定穿透 · Ctrl+L 解锁".into()
            } else {
                "已解锁桌面歌词".into()
            },
            now + 2000.0,
        ));
        self.bridge
            .push(DesktopCommand::SaveConfig(self.cfg.clone()));
    }

    fn set_orientation(
        &mut self,
        orient: DesktopOrientation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cfg.orientation == orient {
            return;
        }
        let b = window.bounds();
        self.cfg
            .set_pos(f32::from(b.origin.x), f32::from(b.origin.y));
        self.cfg.orientation = orient;
        self.bridge.set_cfg(self.cfg.clone());
        let (raw_w, raw_h) = self.cfg.window_size();
        let (disp_w, disp_h) = window
            .display(cx)
            .map(|d| {
                let b = d.visible_bounds();
                (f32::from(b.size.width), f32::from(b.size.height))
            })
            .unwrap_or((1920.0, 1080.0));
        let (min_w, min_h) = orient.min_window_size();
        let (max_w, max_h) = orient.max_window_size_for_display(disp_w, disp_h);
        let (w, h) = (raw_w.clamp(min_w, max_w), raw_h.clamp(min_h, max_h));
        let (nx, ny) = self
            .cfg
            .saved_pos()
            .unwrap_or_else(|| default_origin_for(orient, window, cx));
        let clamped = clamp_origin(nx, ny, w, h, window, cx);
        window.set_bounds(gpui_kit::bounds(
            point(px(clamped.0), px(clamped.1)),
            size(px(w), px(h)),
        ));
        self.last_origin = Some(clamped);
        self.last_size = Some((w, h));
        self.trans_side_inited = false;
        self.bar_side_inited = false;
        self.cfg.set_pos(clamped.0, clamped.1);
        self.bridge.set_cfg(self.cfg.clone());
        self.bridge
            .push(DesktopCommand::SaveConfig(self.cfg.clone()));
    }

    fn open_popover_rect(
        &self,
        anchor: Box4,
        pop_w: f32,
        pop_h: f32,
        window: &Window,
        cx: &App,
    ) -> (f32, f32) {
        let wb = window.bounds();
        let wx = f32::from(wb.origin.x);
        let wy = f32::from(wb.origin.y);
        let ax = wx + anchor.x;
        let ay = wy + anchor.y;
        let (dx, dy, dw, dh) = window
            .display(cx)
            .map(|d| {
                let b = d.visible_bounds();
                (
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                )
            })
            .unwrap_or((0.0, 0.0, 1920.0, 1080.0));
        let gap = 10.0;
        let (raw_x, raw_y) = match self.cfg.orientation {
            DesktopOrientation::Horizontal => {
                let cx_a = ax + anchor.w * 0.5 - pop_w * 0.5;
                let above_y = wy + self.bar_rect.y - pop_h - gap;
                let below_y = wy + self.bar_rect.bottom() + gap;
                let py = if above_y >= dy + 8.0 { above_y } else { below_y };
                (cx_a, py)
            }
            DesktopOrientation::Vertical => {
                let cy_a = ay + anchor.h * 0.5 - pop_h * 0.5;
                let left_x = wx + self.bar_rect.x - pop_w - gap;
                let right_x = wx + self.bar_rect.right() + gap;
                let px_v = if left_x >= dx + 8.0 { left_x } else { right_x };
                (px_v, cy_a)
            }
        };
        (
            raw_x.clamp(dx + 8.0, (dx + dw - pop_w - 8.0).max(dx + 8.0)),
            raw_y.clamp(dy + 8.0, (dy + dh - pop_h - 8.0).max(dy + 8.0)),
        )
    }

    fn spawn_glass_popover<V: 'static + Render>(
        title: &'static str,
        pos: (f32, f32),
        size_px: (f32, f32),
        window: &Window,
        cx: &mut App,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
    ) -> Option<AnyWindowHandle> {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(gpui_kit::bounds(
                point(px(pos.0), px(pos.1)),
                size(px(size_px.0), px(size_px.1)),
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            window_background: WindowBackgroundAppearance::Transparent,
            is_resizable: false,
            is_minimizable: false,
            focus: true,
            display_id: window.display(cx).map(|d| d.id()),
            ..Default::default()
        };
        if let Ok((child, _)) =
            gpui_kit::open_window(options, cx, move |w, cx| cx.new(|cx| build(w, cx)))
        {
            if let Some(root) = child.downcast::<gpui_kit::base::Root>() {
                let _ = root.update(cx, |r, w, cx| {
                    r.style().background = Some(gpui_kit::transparent_black().into());
                    if let Some(h) = win32_window::hwnd_from_window(w) {
                        win32_window::configure_desktop_overlay(h);
                    }
                    cx.notify();
                });
            }
            Some(child)
        } else {
            None
        }
    }

    fn toggle_volume_popover(&mut self, anchor: Box4, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(h) = self.volume_popover.take() {
            if h.update(cx, |_, w, _| w.remove_window()).is_ok() {
                return;
            }
        }
        if self.bridge.was_volume_recently_closed() {
            return;
        }
        self.close_popovers(cx);
        self.bridge.push(DesktopCommand::OpenVolume);
        let (pw, ph) = (240.0_f32, 96.0_f32);
        let pos = self.open_popover_rect(anchor, pw, ph, window, cx);
        let bridge = self.bridge.clone();
        self.volume_popover = Self::spawn_glass_popover(
            "OverLyrics Desktop Volume",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopVolumePopover::new(bridge),
        );
    }

    fn toggle_more_popover(&mut self, anchor: Box4, window: &mut Window, cx: &mut Context<Self>) {
        self.last_more_anchor = anchor;
        if let Some(h) = self.more_popover.take() {
            self.close_color_popover(cx);
            if h.update(cx, |_, w, _| w.remove_window()).is_ok() {
                return;
            }
        }
        if self.bridge.was_more_recently_closed() {
            self.close_color_popover(cx);
            return;
        }
        self.close_popovers(cx);
        let (pw, ph) = (252.0_f32 + POPOVER_ELASTIC_PAD * 2.0, 272.0_f32 + POPOVER_ELASTIC_PAD * 2.0);
        let pos = self.open_popover_rect(anchor, pw, ph, window, cx);
        let bridge = self.bridge.clone();
        self.more_popover = Self::spawn_glass_popover(
            "OverLyrics Desktop Settings",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopMorePopover::new(bridge),
        );
    }

    fn open_color_popover(
        &mut self,
        more_card_screen: Box4,
        color_row_center_y_screen: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(h) = self.color_popover.take() {
            self.bridge.color_popover_open.store(false, Ordering::SeqCst);
            self.bridge.color_popover_side.store(0, Ordering::SeqCst);
            if h.update(cx, |_, w, _| w.remove_window()).is_ok() {
                return;
            }
        }
        if self.bridge.was_color_recently_closed() {
            return;
        }
        let (dx, dy, dw, dh) = window
            .display(cx)
            .map(|d| {
                let b = d.visible_bounds();
                (
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                )
            })
            .unwrap_or((0.0, 0.0, 1920.0, 1080.0));
        let card_w = 240.0_f32;
        let card_h = 214.0_f32;
        let pw = card_w + POPOVER_ELASTIC_PAD * 2.0;
        let ph = card_h + POPOVER_ELASTIC_PAD * 2.0;
        let gap = 8.0_f32;
        let right_x = more_card_screen.right() + gap - POPOVER_ELASTIC_PAD;
        let left_x = more_card_screen.x - gap - card_w - POPOVER_ELASTIC_PAD;
        let (px_c, side): (f32, i8) = if right_x + pw <= dx + dw - 6.0 {
            (right_x, 1)
        } else {
            (left_x.max(dx + 6.0), -1)
        };
        let list_top_in_card = 8.0_f32;
        let item_h = 32.0_f32;
        let selected_row_center_in_card = if let Some(idx) = COLOR_PRESETS
            .iter()
            .position(|&(rgb_v, _)| rgb_v == self.cfg.text_color)
        {
            list_top_in_card + idx as f32 * item_h + item_h * 0.5
        } else {
            list_top_in_card + COLOR_PRESETS.len() as f32 * item_h + 6.0 + item_h * 0.5
        };
        let py_c = (color_row_center_y_screen - selected_row_center_in_card - POPOVER_ELASTIC_PAD)
            .clamp(dy + 6.0, (dy + dh - ph - 6.0).max(dy + 6.0));
        self.bridge.color_popover_open.store(true, Ordering::SeqCst);
        self.bridge.color_popover_side.store(side, Ordering::SeqCst);
        let bridge = self.bridge.clone();
        self.color_popover = Self::spawn_glass_popover(
            "OverLyrics Desktop Color",
            (px_c, py_c),
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopColorPopover::new(bridge),
        );
    }

    fn open_lyrics_picker_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popovers(cx);
        let snap = self.bridge.read_snapshot();
        let visible_rows = snap.candidates.len().clamp(1, 4);
        let pw = 420.0_f32;
        let ph = VERSION_HEADER + visible_rows as f32 * VERSION_ROW + 8.0;
        let anchor = if self.last_more_anchor.w > 0.0 {
            self.last_more_anchor
        } else {
            self.bar_rect
        };
        let pos = self.open_popover_rect(anchor, pw, ph, window, cx);
        let bridge = self.bridge.clone();
        self.picker_popover = Self::spawn_glass_popover(
            "OverLyrics Desktop Lyrics Picker",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopLyricsPickerPopover::new(bridge),
        );
    }

    fn hit_button(&self, x: f32, y: f32) -> Option<(Btn, Box4)> {
        self.buttons
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .copied()
    }

    fn handle_button(&mut self, btn: Btn, rect: Box4, window: &mut Window, cx: &mut Context<Self>) {
        self.btn_feedback[btn.index()].trigger_hop(1.5);
        match btn {
            Btn::Volume => self.toggle_volume_popover(rect, window, cx),
            Btn::Prev => self.bridge.push(DesktopCommand::Prev),
            Btn::Play => self.bridge.push(DesktopCommand::PlayPause),
            Btn::Next => self.bridge.push(DesktopCommand::Next),
            Btn::Translate => {
                self.cfg.show_translation = !self.cfg.show_translation;
                self.bridge.set_cfg(self.cfg.clone());
                self.bridge
                    .push(DesktopCommand::SaveConfig(self.cfg.clone()));
            }
            Btn::Lock => {
                let now = self.now_ms();
                self.set_locked(true, now, cx);
            }
            Btn::More => self.toggle_more_popover(rect, window, cx),
        }
    }

    fn hit_resize_edges(x: f32, y: f32, win_w: f32, win_h: f32) -> ResizeEdges {
        let m = 10.0_f32;
        ResizeEdges {
            left: x <= m,
            right: x >= win_w - m,
            top: y <= m,
            bottom: y >= win_h - m,
        }
    }

    fn shape_single_line(
        &mut self,
        row: &Row,
        display: &Display,
        base_font: f32,
        max_w: f32,
        auto_scroll: bool,
    ) -> Option<Arc<RowLines>> {
        let mut d = *display;
        d.show_translation = false;
        d.show_romanization = false;
        d.balanced = false;

        if auto_scroll {
            let mut m = super::model::metrics(HORIZ_W, HORIZ_H, (1920.0, 1080.0), false, &d);
            m.font = base_font;
            m.line_height = base_font * d.line_height_em.max(1.15);
            return self.shaper.row(row, &m, &d, 4096.0, 0xD100).ok();
        }

        let mut fs = base_font;
        for _ in 0..3 {
            let mut m = super::model::metrics(HORIZ_W, HORIZ_H, (1920.0, 1080.0), false, &d);
            m.font = fs;
            m.line_height = fs * d.line_height_em.max(1.15);
            let probe = self.shaper.row(row, &m, &d, 4096.0, 0xD100).ok()?;
            let natural_w = probe
                .lines
                .first()
                .map(|l| f32::from(l.layout.width))
                .unwrap_or(0.0);
            if natural_w <= max_w || fs <= 14.0 {
                return Some(probe);
            }
            fs = (fs * (max_w / natural_w) * 0.96).clamp(14.0, fs - 1.0);
        }
        let mut m = super::model::metrics(HORIZ_W, HORIZ_H, (1920.0, 1080.0), false, &d);
        m.font = fs;
        m.line_height = fs * d.line_height_em.max(1.15);
        self.shaper.row(row, &m, &d, max_w.max(200.0), 0xD101).ok()
    }
}

pub fn default_origin_for(
    orientation: DesktopOrientation,
    window: &Window,
    cx: &App,
) -> (f32, f32) {
    let (w, h) = orientation.window_size();
    let (dx, dy, dw, dh) = window
        .display(cx)
        .map(|d| {
            let b = d.bounds();
            (
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height),
            )
        })
        .unwrap_or((0.0, 0.0, 1920.0, 1080.0));
    match orientation {
        DesktopOrientation::Horizontal => (dx + (dw - w) * 0.5, dy + dh - h - 72.0),
        DesktopOrientation::Vertical => (dx + dw - w - 28.0, dy + (dh - h) * 0.5),
    }
}

pub fn clamp_origin(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    window: &Window,
    cx: &App,
) -> (f32, f32) {
    let Some(d) = window.display(cx) else {
        return (x, y);
    };
    let b = d.bounds();
    let min_x = f32::from(b.origin.x);
    let min_y = f32::from(b.origin.y);
    let max_x = (min_x + f32::from(b.size.width) - w).max(min_x);
    let max_y = (min_y + f32::from(b.size.height) - h).max(min_y);
    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

fn word_progress(pos_ms: f64, start_ms: f64, end_ms: f64) -> f32 {
    if end_ms <= start_ms {
        return if pos_ms >= start_ms { 1.0 } else { 0.0 };
    }
    ((pos_ms - start_ms) / (end_ms - start_ms)).clamp(0.0, 1.0) as f32
}

fn paint_interlude_dots(
    window: &mut Window,
    orient: DesktopOrientation,
    cx_pos: f32,
    cy_pos: f32,
    font_size: f32,
    dot_scale: f32,
    text_rgb: u32,
    snap: &oly_core::interlude::InterludeSnapshot,
) {
    let base_d = (font_size * 0.28 * dot_scale).clamp(7.0, 18.0);
    let base_g = base_d * 0.72;
    let d = base_d * snap.scale as f32;
    let g = base_g * snap.scale as f32;
    let opacity = snap.opacity as f32;
    if d <= 0.0 || opacity <= 0.005 {
        return;
    }
    let dark_text = is_dark_color(text_rgb);
    let shadow_rgb = if dark_text { 0xFFFFFF } else { 0x000000 };
    let span = d * 3.0 + g * 2.0;
    for i in 0..3 {
        let a = (opacity * snap.dot_opacities[i] as f32).clamp(0.0, 1.0);
        if a <= 0.005 {
            continue;
        }
        let (dx, dy) = match orient {
            DesktopOrientation::Horizontal => {
                (cx_pos - span * 0.5 + i as f32 * (d + g), cy_pos - d * 0.5)
            }
            DesktopOrientation::Vertical => {
                (cx_pos - d * 0.5, cy_pos - span * 0.5 + i as f32 * (d + g))
            }
        };
        chrome::fill_round_subpixel(
            window,
            dx,
            dy + if dark_text { 0.0 } else { 1.5 },
            d,
            d,
            d * 0.5,
            Hsla::from(gpui_kit::rgb(shadow_rgb)).opacity(0.55 * a),
        );
        chrome::fill_round_subpixel(
            window,
            dx,
            dy,
            d,
            d,
            d * 0.5,
            Hsla::from(gpui_kit::rgb(text_rgb)).opacity(a),
        );
    }
}

fn measure_vertical_steps(
    window: &Window,
    tokens: &[VerticalToken],
    fs: f32,
    weight: f32,
) -> (Vec<f32>, f32) {
    let mut steps = Vec::with_capacity(tokens.len());
    let mut total = 0.0_f32;
    let run_fs = fs * 0.92;
    for tok in tokens {
        let step = match tok {
            VerticalToken::Space => fs * 0.42,
            VerticalToken::UprightChar { .. } => fs * 1.06,
            VerticalToken::RotatedRun { text, .. } => {
                let run = chrome::text_run(text.len(), weight, Hsla::white());
                let layout = window.text_system().layout_line(
                    &SharedString::from(text.clone()),
                    px(run_fs),
                    &[run],
                    None,
                );
                let gw = f32::from(layout.width);
                gw.max(fs * 0.45) + fs * 0.08
            }
        };
        steps.push(step);
        total += step;
    }
    (steps, total)
}

pub fn compute_vertical_scroll_target(
    tokens: &[VerticalToken],
    steps: &[f32],
    total_h: f32,
    vis_h: f32,
    playhead_ms: f64,
    row_start_ms: f64,
    row_dur_ms: f64,
    word_timed: bool,
) -> f32 {
    if total_h <= vis_h || tokens.is_empty() {
        return 0.0;
    }
    let max_scroll = (total_h - vis_h).max(0.0);
    if word_timed {
        let mut cur_y = 0.0_f32;
        let mut focus_y: Option<f32> = None;
        let mut first_start = f64::INFINITY;
        for (tok, &step) in tokens.iter().zip(steps.iter()) {
            let timing = match tok {
                VerticalToken::UprightChar { start_ms, end_ms, .. }
                | VerticalToken::RotatedRun { start_ms, end_ms, .. } => Some((*start_ms, *end_ms)),
                VerticalToken::Space => None,
            };
            if let Some((s, e)) = timing {
                if first_start.is_infinite() {
                    first_start = s;
                }
                if playhead_ms < e {
                    let p = word_progress(playhead_ms, s, e);
                    focus_y = Some(cur_y + step * p);
                    break;
                }
            }
            cur_y += step;
        }
        let fy = if let Some(y) = focus_y {
            y
        } else if playhead_ms <= first_start {
            0.0
        } else {
            total_h
        };
        (fy - vis_h * 0.44).clamp(0.0, max_scroll)
    } else {
        let prog = paint::line_progress(playhead_ms, row_start_ms, row_dur_ms).clamp(0.0, 1.0);
        let adj = ((prog - 0.10) / 0.78).clamp(0.0, 1.0);
        max_scroll * smoothstep01(adj)
    }
}

/// Paints a single-column vertical lyric line at constant font size:
/// CJK upright, Latin/English rotated 90° clockwise, short lines centered,
/// overflowing lines smoothly scrolled with soft top/bottom edge fade.
#[allow(clippy::too_many_arguments)]
fn paint_vertical_text_column(
    window: &mut Window,
    col_cx: f32,
    win_h: f32,
    row: &Row,
    shaped: Option<&RowLines>,
    display: &Display,
    playhead_ms: f64,
    base_font: f32,
    text_rgb: u32,
    base_alpha: f32,
    fade: f32,
    scroll_y: f32,
) {
    let tokens = tokenize_vertical_row(row);
    if tokens.is_empty() {
        return;
    }

    let fs = base_font;
    let vis_top = 18.0_f32;
    let vis_bottom = (win_h - 18.0).max(vis_top + 60.0);
    let vis_h = (vis_bottom - vis_top).max(60.0);
    let (steps, total_h) = measure_vertical_steps(window, &tokens, fs, display.weight);
    let overflowing = total_h > vis_h;
    let mut cur_y = if overflowing {
        vis_top - scroll_y.clamp(0.0, (total_h - vis_h).max(0.0))
    } else {
        vis_top + (vis_h - total_h) * 0.5
    };

    let sf = window.scale_factor();
    let dark_text = is_dark_color(text_rgb);
    let shadow_alpha_scale = if dark_text { 0.18 } else { DESKTOP_DROP_SHADOW };
    let shadow_rgb = 0x000000_u32;
    let glow_rgb = text_rgb;
    let elapsed_line = shaped.map(|s| playhead_ms - s.line_start_ms).unwrap_or(0.0);
    let word_timed = shaped.is_some_and(|s| s.word_timed);
    let dark_a = (display.unplayed * base_alpha * fade).clamp(0.0, 1.0);
    let bright_a = (display.bright * base_alpha * fade).clamp(0.0, 1.0);
    let fade_band = 36.0_f32;
    let edge_fade = |y_pos: f32| -> f32 {
        if !overflowing {
            return 1.0;
        }
        let top_a = ((y_pos - vis_top) / fade_band).clamp(0.0, 1.0);
        let bot_a = ((vis_bottom - y_pos) / fade_band).clamp(0.0, 1.0);
        smoothstep01(top_a.min(bot_a))
    };

    let sample_amll_motion =
        |word_source: usize, char_index: usize, start_ms: f64, end_ms: f64| -> (f32, f32, f32, f32) {
            let Some(s) = shaped else {
                return (1.0, 0.0, 0.0, 0.0);
            };
            if !s.word_timed || elapsed_line < 0.0 {
                return (1.0, 0.0, 0.0, 0.0);
            }
            if let Some(Some(plan)) = s.emphasis.get(word_source) {
                if let Some(ch) = plan.chars.get(char_index) {
                    let glow_t = (elapsed_line - ch.word_delay) / plan.glow_duration.max(1.0);
                    let float_t =
                        (elapsed_line - plan.float_delay(ch)) / plan.float_duration.max(1.0);
                    let lift = if float_t > 0.0 {
                        sample_float(&ch.float, float_t.clamp(0.0, 1.0)) as f32
                    } else {
                        0.0
                    };
                    if glow_t > 0.0 {
                        let g = sample_glow(&ch.glow, glow_t.clamp(0.0, 1.0));
                        let ramp = ch.glow.first().map_or(1.0, |first| {
                            if first.offset > 0.0 && glow_t < first.offset {
                                (glow_t / first.offset).clamp(0.0, 1.0)
                            } else {
                                1.0
                            }
                        }) as f32;
                        let sc = 1.0 + (g.scale as f32 - 1.0) * ramp;
                        let glow = g.shadow_alpha as f32 * ramp * fade * base_alpha;
                        let blur = g.shadow_blur_em as f32 * fs;
                        return (sc, lift * fs, glow, blur);
                    }
                    return (1.0, lift * fs, 0.0, 0.0);
                }
            }
            let lift = PlainFloat::new(start_ms, end_ms, s.line_start_ms, s.is_bg);
            let dy = lift.lift_at((elapsed_line - lift.delay_ms) / lift.duration_ms) as f32 * fs;
            (1.0, dy, 0.0, 0.0)
        };

    for (tok, &step_h) in tokens.iter().zip(steps.iter()) {
        if overflowing && (cur_y + step_h < vis_top - 12.0 || cur_y > vis_bottom + 12.0) {
            cur_y += step_h;
            continue;
        }
        match tok {
            VerticalToken::Space => {}
            VerticalToken::UprightChar {
                ch,
                word_source,
                char_index,
                start_ms,
                end_ms,
            } => {
                let ea = edge_fade(cur_y + step_h * 0.5);
                if ea <= 0.005 {
                    cur_y += step_h;
                    continue;
                }
                let s = ch.to_string();
                let p = if word_timed {
                    word_progress(playhead_ms, *start_ms, *end_ms)
                } else {
                    1.0
                };
                let alpha = (dark_a + (bright_a - dark_a) * p) * ea;
                let (sc, lift_px, glow, blur) =
                    sample_amll_motion(*word_source, *char_index, *start_ms, *end_ms);
                let color = Hsla::from(gpui_kit::rgb(text_rgb)).opacity(alpha);
                let run = chrome::text_run(s.len(), display.weight, color);
                let layout = window.text_system().layout_line(
                    &SharedString::from(s),
                    px(fs),
                    &[run],
                    None,
                );
                let gw = f32::from(layout.width);
                let ascent = f32::from(layout.ascent);
                let descent = f32::from(layout.descent);
                let gx = col_cx - gw * 0.5;
                let baseline = cur_y + (step_h + ascent - descent) * 0.5;
                let cx = col_cx;
                let cy = cur_y + step_h * 0.5;
                let tx = (1.0 - sc) * cx;
                let ty = (1.0 - sc) * cy + lift_px * 0.55;
                let transform = TransformationMatrix {
                    rotation_scale: [[sc, 0.0], [0.0, sc]],
                    translation: [tx * sf, ty * sf],
                };
                for r in &layout.runs {
                    for g in &r.glyphs {
                        let origin = point(px(gx) + g.position.x, px(baseline) + g.position.y);
                        #[cfg(target_os = "windows")]
                        {
                            let mut st = transform;
                            st.translation[1] += if dark_text { 1.0 * sf } else { 1.5 * sf };
                            let _ = window.paint_lyric_glyph(
                                origin,
                                r.font_id,
                                g.id,
                                px(fs),
                                Hsla::from(gpui_kit::rgb(shadow_rgb))
                                    .opacity((shadow_alpha_scale * alpha).clamp(0.0, 0.85)),
                                st,
                                [0.0; 4],
                                [0.0; 4],
                                2,
                            );
                            if glow * ea > 0.002 && blur > 0.0 {
                                let _ = window.paint_lyric_glyph(
                                    origin,
                                    r.font_id,
                                    g.id,
                                    px(fs),
                                    Hsla::from(gpui_kit::rgb(glow_rgb)).opacity(glow * ea),
                                    transform,
                                    [0.0; 4],
                                    [0.0; 4],
                                    (blur * sf * 1.5).ceil().clamp(1.0, 64.0) as u16,
                                );
                            }
                            let _ = window.paint_lyric_glyph(
                                origin,
                                r.font_id,
                                g.id,
                                px(fs),
                                color,
                                transform,
                                [0.0; 4],
                                [0.0; 4],
                                0,
                            );
                        }
                        #[cfg(not(target_os = "windows"))]
                        {
                            let _ = window.paint_glyph(origin, r.font_id, g.id, px(fs), color);
                        }
                    }
                }
            }
            VerticalToken::RotatedRun {
                text,
                word_source,
                start_ms,
                end_ms,
            } => {
                let run_fs = fs * 0.92;
                let (sc, lift_px, glow, blur) =
                    sample_amll_motion(*word_source, 0, *start_ms, *end_ms);
                let run = chrome::text_run(text.len(), display.weight, Hsla::white());
                let layout = window.text_system().layout_line(
                    &SharedString::from(text.clone()),
                    px(run_fs),
                    &[run],
                    None,
                );
                let gw = f32::from(layout.width);
                let ascent = f32::from(layout.ascent);
                let descent = f32::from(layout.descent);
                let cx = col_cx;
                let cy = cur_y + step_h * 0.5;
                let gx = cx - gw * 0.5;
                let baseline = cy + (ascent - descent) * 0.5;
                let px_d = cx * sf;
                let py_d = cy * sf;
                // 90° clockwise rotation in HLSL column-major float2x2 (tops face right, reads top-to-bottom)
                let transform = TransformationMatrix {
                    rotation_scale: [[0.0, -sc], [sc, 0.0]],
                    translation: [px_d + sc * py_d, py_d - sc * px_d + lift_px * 0.45 * sf],
                };
                let span = (*end_ms - *start_ms).max(1.0);
                for r in &layout.runs {
                    for g in &r.glyphs {
                        let gx_pos = f32::from(g.position.x);
                        let rot_y = cy + (gx + gx_pos - cx) * sc;
                        let ea = edge_fade(rot_y);
                        if ea <= 0.005 {
                            continue;
                        }
                        let gp = if word_timed {
                            let frac = if gw > 1.0 {
                                (gx_pos / gw).clamp(0.0, 1.0) as f64
                            } else {
                                0.0
                            };
                            let gs = *start_ms + span * frac;
                            let ge = (gs + span * 0.25).min(*end_ms).max(gs + 1.0);
                            word_progress(playhead_ms, gs, ge)
                        } else {
                            1.0
                        };
                        let alpha = (dark_a + (bright_a - dark_a) * gp) * ea;
                        let color = Hsla::from(gpui_kit::rgb(text_rgb)).opacity(alpha);
                        let origin = point(px(gx) + g.position.x, px(baseline) + g.position.y);
                        #[cfg(target_os = "windows")]
                        {
                            let mut st = transform;
                            st.translation[1] += if dark_text { 1.0 * sf } else { 1.5 * sf };
                            let _ = window.paint_lyric_glyph(
                                origin,
                                r.font_id,
                                g.id,
                                px(run_fs),
                                Hsla::from(gpui_kit::rgb(shadow_rgb))
                                    .opacity((shadow_alpha_scale * alpha).clamp(0.0, 0.85)),
                                st,
                                [0.0; 4],
                                [0.0; 4],
                                2,
                            );
                            if glow * ea > 0.002 && blur > 0.0 {
                                let _ = window.paint_lyric_glyph(
                                    origin,
                                    r.font_id,
                                    g.id,
                                    px(run_fs),
                                    Hsla::from(gpui_kit::rgb(glow_rgb)).opacity(glow * ea),
                                    transform,
                                    [0.0; 4],
                                    [0.0; 4],
                                    (blur * sf * 1.5).ceil().clamp(1.0, 64.0) as u16,
                                );
                            }
                            let _ = window.paint_lyric_glyph(
                                origin,
                                r.font_id,
                                g.id,
                                px(run_fs),
                                color,
                                transform,
                                [0.0; 4],
                                [0.0; 4],
                                0,
                            );
                        }
                        #[cfg(not(target_os = "windows"))]
                        {
                            let _ = window.paint_glyph(origin, r.font_id, g.id, px(run_fs), color);
                        }
                    }
                }
            }
        }
        cur_y += step_h;
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_horiz_translation(
    window: &mut Window,
    text: &str,
    win_w: f32,
    y_top: f32,
    font_size: f32,
    text_rgb: u32,
    alpha: f32,
    auto_scroll: bool,
    scroll_x: f32,
) {
    let trimmed = text.trim();
    if trimmed.is_empty() || alpha <= 0.01 {
        return;
    }
    let pad_x = 24.0_f32;
    let max_w = (win_w - pad_x * 2.0).max(200.0);
    let (raw_w, _) = chrome::measure(window, trimmed, font_size, 500.0);
    let fs = if !auto_scroll && raw_w > max_w && raw_w > 1.0 {
        (font_size * (max_w / raw_w)).clamp(11.0, font_size)
    } else {
        font_size
    };
    let (tw, _) = chrome::measure(window, trimmed, fs, 500.0);
    let overflowing = auto_scroll && tw > max_w;
    let x = if overflowing {
        pad_x - scroll_x.clamp(0.0, (tw - max_w).max(0.0))
    } else {
        ((win_w - tw) * 0.5).max(pad_x)
    };
    let dark_text = is_dark_color(text_rgb);
    let shadow_a = if dark_text {
        (0.18 * alpha).clamp(0.0, 0.22)
    } else {
        (0.62 * alpha).clamp(0.0, 0.85)
    };
    let draw = |window: &mut Window| {
        chrome::paint_text(
            window,
            trimmed,
            x,
            y_top + if dark_text { 0.8 } else { 1.2 },
            fs,
            500.0,
            Hsla::from(gpui_kit::rgb(0x000000)).opacity(shadow_a),
        );
        chrome::paint_text(
            window,
            trimmed,
            x,
            y_top,
            fs,
            500.0,
            Hsla::from(gpui_kit::rgb(text_rgb)).opacity(alpha),
        );
    };
    if overflowing {
        window.with_content_mask(
            Some(ContentMask {
                bounds: gpui_kit::bounds(
                    point(px(pad_x), px((y_top - 4.0).max(0.0))),
                    gpui_kit::size(px(max_w), px(fs * 1.6 + 8.0)),
                ),
            }),
            draw,
        );
    } else {
        draw(window);
    }
}

impl Render for DesktopLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let parent = self.parent.clone();
        window.defer(cx, move |_, cx| {
            let _ = parent.update(cx, |pv, _| {
                pv.tick_live_for_desktop();
            });
        });

        let now = self.now_ms();
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;

        for act in self.bridge.drain_popover() {
            match act {
                PopoverAction::SetOrientation(orient) => {
                    self.close_popovers(cx);
                    self.set_orientation(orient, window, cx);
                }
                PopoverAction::FontDelta(delta) => {
                    self.cfg.font_size = (self.cfg.font_size + delta).clamp(MIN_FONT, MAX_FONT);
                    self.bridge.set_cfg(self.cfg.clone());
                    self.bridge
                        .push(DesktopCommand::SaveConfig(self.cfg.clone()));
                }
                PopoverAction::SetColor(rgb) => {
                    self.cfg.text_color = rgb & 0x00FF_FFFF;
                    self.bridge.set_cfg(self.cfg.clone());
                    self.bridge
                        .push(DesktopCommand::SaveConfig(self.cfg.clone()));
                }
                PopoverAction::ToggleAutoScroll => {
                    self.cfg.auto_scroll = !self.cfg.auto_scroll;
                    self.bridge.set_cfg(self.cfg.clone());
                    self.bridge
                        .push(DesktopCommand::SaveConfig(self.cfg.clone()));
                }
                PopoverAction::OpenColorPopover(more_card_screen, color_row_center_y) => {
                    self.open_color_popover(more_card_screen, color_row_center_y, window, cx);
                }
                PopoverAction::CloseColorPopover => {
                    self.close_color_popover(cx);
                }
                PopoverAction::OpenLyricsPicker => {
                    self.open_lyrics_picker_popover(window, cx);
                }
                PopoverAction::ReturnToMain => {
                    self.close_popovers(cx);
                    let parent = self.parent.clone();
                    let _ = parent.update(cx, |pv, _| {
                        pv.restore_from_desktop_lyrics();
                    });
                    window.remove_window();
                    return div().into_any_element();
                }
            }
        }

        let mut os_cursor_inside = self.cursor.is_some();
        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if !self.configured_hwnd {
                self.configured_hwnd = true;
                window.defer(cx, move |_, _| {
                    win32_window::configure_desktop_overlay(hwnd);
                });
            }
            os_cursor_inside = win32_window::cursor_in_window(hwnd);
            if !os_cursor_inside && self.pressed_btn.is_none() {
                self.cursor = None;
            }
            let hc = self.hotkey.counter.load(Ordering::SeqCst);
            if hc != self.last_hotkey_count {
                self.last_hotkey_count = hc;
                self.set_locked(!self.cfg.locked, now, cx);
            }
            if self.applied_click_through != Some(self.cfg.locked) {
                let want_locked = self.cfg.locked;
                self.applied_click_through = Some(want_locked);
                window.defer(cx, move |_, _| {
                    win32_window::set_click_through(hwnd, want_locked);
                });
            }
        }
        if self.pressed_btn.is_some() && !win32_window::is_left_mouse_down() {
            self.pressed_btn = None;
            self.press_gesture.cancel();
        }

        let sf = window.scale_factor();
        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let win_h = f32::from(vp.height).max(1.0);

        let mut snap_guide_x = false;
        let mut snap_guide_y = false;
        if let Some(mut rs) = self.resize {
            if !self.cfg.locked && win32_window::is_left_mouse_down() {
                if let (Some(hwnd), Some((cx_p, cy_p))) = (
                    win32_window::hwnd_from_window(window),
                    win32_window::cursor_screen_pos(),
                ) {
                    let (disp_w, disp_h) = window
                        .display(cx)
                        .map(|d| {
                            let b = d.visible_bounds();
                            (f32::from(b.size.width), f32::from(b.size.height))
                        })
                        .unwrap_or((1920.0, 1080.0));
                    let (min_w, min_h) = self.cfg.orientation.min_window_size();
                    let (max_w, max_h) = self.cfg.orientation.max_window_size_for_display(disp_w, disp_h);
                    let min_w_p = (min_w * sf).round() as i32;
                    let min_h_p = (min_h * sf).round() as i32;
                    let max_w_p = (max_w * sf).round().max(min_w_p as f32) as i32;
                    let max_h_p = (max_h * sf).round().max(min_h_p as f32) as i32;

                    let dx_p = cx_p - rs.start_cursor_phys.0;
                    let dy_p = cy_p - rs.start_cursor_phys.1;
                    let (sx_p, sy_p, sw_p, sh_p) = rs.start_rect_phys;
                    let mut nx_p = sx_p;
                    let mut ny_p = sy_p;
                    let mut nw_p = sw_p;
                    let mut nh_p = sh_p;
                    if rs.edges.right {
                        nw_p = (sw_p + dx_p).clamp(min_w_p, max_w_p);
                    } else if rs.edges.left {
                        let right_p = sx_p + sw_p;
                        nw_p = (sw_p - dx_p).clamp(min_w_p, max_w_p);
                        nx_p = right_p - nw_p;
                    }
                    if rs.edges.bottom {
                        nh_p = (sh_p + dy_p).clamp(min_h_p, max_h_p);
                    } else if rs.edges.top {
                        let bottom_p = sy_p + sh_p;
                        nh_p = (sh_p - dy_p).clamp(min_h_p, max_h_p);
                        ny_p = bottom_p - nh_p;
                    }
                    let target_phys = (nx_p, ny_p, nw_p, nh_p);
                    self.last_origin = Some((nx_p as f32 / sf, ny_p as f32 / sf));
                    self.last_size = Some((nw_p as f32 / sf, nh_p as f32 / sf));
                    if target_phys != rs.last_applied_phys {
                        rs.last_applied_phys = target_phys;
                        self.resize = Some(rs);
                        self.schedule_window_rect_phys(
                            hwnd,
                            nx_p,
                            ny_p,
                            Some((nw_p, nh_p)),
                            cx,
                        );
                    }
                }
            } else {
                self.resize = None;
                if let Some((ox, oy)) = self.last_origin {
                    self.cfg.set_pos(ox, oy);
                }
                if let Some((rw, rh)) = self.last_size {
                    self.cfg.set_size(rw, rh);
                }
                self.bridge.set_cfg(self.cfg.clone());
                self.bridge
                    .push(DesktopCommand::SaveConfig(self.cfg.clone()));
            }
        } else if let Some(mut drag) = self.drag {
            if !self.cfg.locked && win32_window::is_left_mouse_down() {
                if let (Some(hwnd), Some((cx_p, cy_p))) = (
                    win32_window::hwnd_from_window(window),
                    win32_window::cursor_screen_pos(),
                ) {
                    let dx = (cx_p - drag.start_cursor_phys.0) as f32 / sf;
                    let dy = (cy_p - drag.start_cursor_phys.1) as f32 / sf;
                    let raw_x = drag.start_origin_logical.0 + dx;
                    let raw_y = drag.start_origin_logical.1 + dy;
                    let (nx, ny, sx, sy) = snap_and_clamp_drag(
                        raw_x,
                        raw_y,
                        win_w,
                        win_h,
                        drag.snapped_x,
                        drag.snapped_y,
                        window,
                        cx,
                    );
                    drag.snapped_x = sx;
                    drag.snapped_y = sy;
                    snap_guide_x = sx;
                    snap_guide_y = sy;
                    self.drag = Some(drag);
                    self.last_origin = Some((nx, ny));
                    self.schedule_window_rect_phys(
                        hwnd,
                        (nx * sf).round() as i32,
                        (ny * sf).round() as i32,
                        None,
                        cx,
                    );
                }
            } else {
                self.drag = None;
                if let Some((ox, oy)) = self.last_origin {
                    self.cfg.set_pos(ox, oy);
                    self.bridge.set_cfg(self.cfg.clone());
                    self.bridge
                        .push(DesktopCommand::SaveConfig(self.cfg.clone()));
                }
            }
        } else {
            let wb = window.bounds();
            let cur_origin = (f32::from(wb.origin.x), f32::from(wb.origin.y));
            if self.last_origin.is_none() {
                self.last_origin = Some(cur_origin);
            }
        }

        let snap = self.bridge.read_snapshot();
        if snap.active_row_index != self.last_row_idx {
            let first = self.last_row_idx.is_none();
            self.last_row_idx = snap.active_row_index;
            self.line_fade_start_ms = if first { now - 1000.0 } else { now };
            self.vert_main_scroll = 0.0;
            self.vert_trans_scroll = 0.0;
            self.horiz_main_scroll = 0.0;
            self.horiz_trans_scroll = 0.0;
        }
        let line_fade = ((now - self.line_fade_start_ms) / 180.0).clamp(0.0, 1.0) as f32;

        self.play_morph
            .set_target_position(if snap.playing { 1.0 } else { 0.0 });
        self.play_morph.update(dt / 1000.0);
        let play_morph = self.play_morph.position().clamp(0.0, 1.0) as f32;

        self.interlude_dots.set_playing(snap.playing);
        self.interlude_dots.sync_clock(snap.position_ms);
        if snap.active_row.is_none() {
            if let Some((s, e)) = snap.interlude_range {
                if self.last_interlude != Some((s, e)) {
                    self.last_interlude = Some((s, e));
                    self.interlude_dots
                        .set_interlude(s, e, snap.position_ms, false, 0);
                }
            } else if self.last_interlude.is_some() {
                self.last_interlude = None;
                self.interlude_dots.clear_interlude(false);
            }
        } else if self.last_interlude.is_some() {
            self.last_interlude = None;
            self.interlude_dots.clear_interlude(false);
        }
        self.interlude_dots.update(dt);
        let interlude_snap = self.interlude_dots.visible_snapshot().map(|(s, _, _)| s);

        // Prune closed popover handles so hover state never sticks.
        let open_wins = cx.windows();
        if let Some(h) = self.volume_popover {
            if !open_wins.contains(&h) {
                self.volume_popover = None;
            }
        }
        if let Some(h) = self.more_popover {
            if !open_wins.contains(&h) {
                self.more_popover = None;
                self.close_color_popover(cx);
            }
        }
        if let Some(h) = self.color_popover {
            if !open_wins.contains(&h) {
                self.color_popover = None;
                self.bridge.color_popover_open.store(false, Ordering::SeqCst);
                self.bridge.color_popover_side.store(0, Ordering::SeqCst);
            }
        }
        if let Some(h) = self.picker_popover {
            if !open_wins.contains(&h) {
                self.picker_popover = None;
            }
        }
        let any_popover_open = self.volume_popover.is_some()
            || self.more_popover.is_some()
            || self.color_popover.is_some()
            || self.picker_popover.is_some();
        let hovering = !self.cfg.locked
            && (os_cursor_inside
                || self.drag.is_some()
                || self.resize.is_some()
                || self.pressed_btn.is_some()
                || any_popover_open);
        if hovering {
            self.leave_at_ms = None;
            self.hover_alpha = (self.hover_alpha + (dt as f32) / 220.0).clamp(0.0, 1.0);
        } else {
            let leave_at = *self.leave_at_ms.get_or_insert(now);
            if self.cfg.locked || now - leave_at >= 220.0 {
                self.hover_alpha = (self.hover_alpha - (dt as f32) / 260.0).clamp(0.0, 1.0);
            }
        }

        let orient = self.cfg.orientation;
        let font_size = self.cfg.font_size;
        let text_color = self.cfg.text_color;
        let show_trans = self.cfg.show_translation;
        let auto_scroll = self.cfg.auto_scroll;
        let hover_alpha = smoothstep01(self.hover_alpha);

        // Screen bounds for smooth screen-half bar positioning & vertical main/trans placement.
        let wb = window.bounds();
        let (disp_x, disp_y, disp_w, disp_h) = window
            .display(cx)
            .map(|d| {
                let b = d.visible_bounds();
                (
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                )
            })
            .unwrap_or((0.0, 0.0, 1920.0, 1080.0));
        let win_x = f32::from(wb.origin.x);
        let win_y = f32::from(wb.origin.y);
        let in_top_half = (win_y + win_h * 0.5) < (disp_y + disp_h * 0.5);
        let in_right_half = (win_x + win_w * 0.5) >= (disp_x + disp_w * 0.5);
        // Horizontal: top half -> bar at bottom (1.0), bottom half -> bar at top (0.0).
        // Vertical: left half -> bar at right (1.0), right half -> bar at left (0.0).
        let target_bar_side = match orient {
            DesktopOrientation::Horizontal => if in_top_half { 1.0_f32 } else { 0.0_f32 },
            DesktopOrientation::Vertical => if !in_right_half { 1.0_f32 } else { 0.0_f32 },
        };
        if !self.bar_side_inited {
            self.bar_side_inited = true;
            self.bar_side_blend = target_bar_side;
        } else {
            let step = (dt as f32) / 340.0;
            if self.bar_side_blend < target_bar_side {
                self.bar_side_blend = (self.bar_side_blend + step).min(target_bar_side);
            } else if self.bar_side_blend > target_bar_side {
                self.bar_side_blend = (self.bar_side_blend - step).max(target_bar_side);
            }
        }
        let bar_side_t = smoothstep01(self.bar_side_blend);

        let target_trans_right = if in_right_half { 1.0_f32 } else { 0.0_f32 };
        if !self.trans_side_inited {
            self.trans_side_inited = true;
            self.trans_right_blend = target_trans_right;
        } else {
            let step = (dt as f32) / 320.0;
            if self.trans_right_blend < target_trans_right {
                self.trans_right_blend = (self.trans_right_blend + step).min(target_trans_right);
            } else if self.trans_right_blend > target_trans_right {
                self.trans_right_blend = (self.trans_right_blend - step).max(target_trans_right);
            }
        }
        let trans_right_t = smoothstep01(self.trans_right_blend);

        // Symmetric 7-button order: Lock on far left, Play/Pause always dead-center!
        let btn_specs: [(Btn, Icon); Btn::COUNT] = [
            (Btn::Lock, Icon::Lock),
            (Btn::Volume, Icon::Volume),
            (Btn::Prev, Icon::Prev),
            (Btn::Play, Icon::Play),
            (Btn::Next, Icon::Next),
            (
                Btn::Translate,
                if show_trans {
                    Icon::TranslateOn
                } else {
                    Icon::TranslateOff
                },
            ),
            (Btn::More, Icon::More),
        ];

        let mut buttons = Vec::with_capacity(btn_specs.len());
        let bar_rect = match orient {
            DesktopOrientation::Horizontal => {
                let btn_s = 28.0_f32;
                let gap = 6.0_f32;
                let pad_x = 6.0_f32;
                let total_w =
                    btn_specs.len() as f32 * btn_s + (btn_specs.len() - 1) as f32 * gap + pad_x * 2.0;
                let bx = (win_w - total_w) * 0.5;
                let top_y = 6.0_f32;
                let bot_y = (win_h - 38.0).max(top_y);
                let by = top_y + (bot_y - top_y) * bar_side_t;
                for (i, (btn, _)) in btn_specs.iter().enumerate() {
                    let rx = bx + pad_x + i as f32 * (btn_s + gap);
                    buttons.push((*btn, Box4::new(rx, by + 2.0, btn_s, btn_s)));
                }
                Box4::new(bx, by, total_w, 32.0)
            }
            DesktopOrientation::Vertical => {
                let btn_s = 28.0_f32;
                let gap = 6.0_f32;
                let pad_y = 6.0_f32;
                let total_h =
                    btn_specs.len() as f32 * btn_s + (btn_specs.len() - 1) as f32 * gap + pad_y * 2.0;
                let left_x = 6.0_f32;
                let right_x = (win_w - 38.0).max(left_x);
                let bx = left_x + (right_x - left_x) * bar_side_t;
                let by = (win_h - total_h) * 0.5;
                for (i, (btn, _)) in btn_specs.iter().enumerate() {
                    let ry = by + pad_y + i as f32 * (btn_s + gap);
                    buttons.push((*btn, Box4::new(bx + 2.0, ry, btn_s, btn_s)));
                }
                Box4::new(bx, by, 32.0, total_h)
            }
        };
        self.bar_rect = bar_rect;
        self.buttons = buttons.clone();

        let raw_delta = self.press_gesture.delta();
        let mut btn_animating = false;
        for (btn, rect) in &buttons {
            let idx = btn.index();
            let hovered = self.cursor.is_some_and(|(mx, my)| rect.contains(mx, my));
            let held = self.pressed_btn == Some(*btn);
            btn_animating |= self.btn_feedback[idx].step(hovered, held, dt);
            self.btn_elastic[idx].step(raw_delta, held, dt);
            btn_animating |= self.btn_elastic[idx].pending();
            let el = &self.btn_elastic[idx];
            let fb = &mut self.btn_feedback[idx];
            fb.pull = el.pull();
            fb.squeeze = el.squeeze();
            fb.press = el.pressure().clamp(0.0, 1.0);
            if let Some((mx, my)) = self.cursor {
                fb.focus = [
                    ((mx - rect.x) / rect.w.max(1.0) * 2.0 - 1.0).clamp(-1.5, 1.5),
                    ((my - rect.y) / rect.h.max(1.0) * 2.0 - 1.0).clamp(-1.5, 1.5),
                ];
            }
        }
        let btn_feedback = self.btn_feedback;

        let horiz_avail_w = (win_w - 48.0).max(200.0);
        let shaped_active = snap.active_row.as_ref().and_then(|row| {
            self.shape_single_line(row, &snap.display, font_size, horiz_avail_w, auto_scroll)
        });

        // Smooth progress-driven scrolling (or fit-by-font-size when auto_scroll is off) for both orientations.
        let mut vert_main_scroll = self.vert_main_scroll;
        let mut vert_trans_scroll = self.vert_trans_scroll;
        let mut vert_eff_main_fs = font_size;
        let mut vert_eff_trans_fs = (font_size * 0.62).clamp(12.0, 40.0);
        let mut horiz_main_scroll = self.horiz_main_scroll;
        let mut horiz_trans_scroll = self.horiz_trans_scroll;
        let scroll_alpha = (1.0 - (-dt / 180.0).exp()) as f32;

        if orient == DesktopOrientation::Horizontal {
            if let (Some(row), Some(shaped)) = (snap.active_row.as_ref(), shaped_active.as_ref()) {
                if auto_scroll {
                    let natural_w = shaped
                        .lines
                        .first()
                        .map(|l| f32::from(l.layout.width))
                        .unwrap_or(0.0);
                    let max_sx = (natural_w - horiz_avail_w).max(0.0);
                    let prog = paint::line_progress(snap.position_ms, row.start_ms, row.duration_ms())
                        .clamp(0.0, 1.0);
                    let adj = ((prog - 0.10) / 0.78).clamp(0.0, 1.0);
                    let target_sx = max_sx * smoothstep01(adj);
                    self.horiz_main_scroll += (target_sx - self.horiz_main_scroll) * scroll_alpha;
                    if (self.horiz_main_scroll - target_sx).abs() < 0.2 {
                        self.horiz_main_scroll = target_sx;
                    }
                    horiz_main_scroll = self.horiz_main_scroll;

                    if show_trans && !row.translation.trim().is_empty() {
                        let trans_fs = (shaped.font_size * 0.62).clamp(12.0, 40.0);
                        let (tw, _) = chrome::measure(window, row.translation.trim(), trans_fs, 500.0);
                        let max_tx = (tw - horiz_avail_w).max(0.0);
                        let target_tx = max_tx * smoothstep01(adj);
                        self.horiz_trans_scroll +=
                            (target_tx - self.horiz_trans_scroll) * scroll_alpha;
                        if (self.horiz_trans_scroll - target_tx).abs() < 0.2 {
                            self.horiz_trans_scroll = target_tx;
                        }
                        horiz_trans_scroll = self.horiz_trans_scroll;
                    } else {
                        self.horiz_trans_scroll = 0.0;
                        horiz_trans_scroll = 0.0;
                    }
                } else {
                    self.horiz_main_scroll = 0.0;
                    self.horiz_trans_scroll = 0.0;
                    horiz_main_scroll = 0.0;
                    horiz_trans_scroll = 0.0;
                }
            } else {
                self.horiz_main_scroll = 0.0;
                self.horiz_trans_scroll = 0.0;
                horiz_main_scroll = 0.0;
                horiz_trans_scroll = 0.0;
            }
        } else if let Some(row) = snap.active_row.as_ref() {
            let vis_h = (win_h - 36.0).max(60.0);
            if !self
                .vert_main_cache
                .as_ref()
                .is_some_and(|c| c.matches(&row.text, font_size, snap.display.weight))
            {
                let tokens = tokenize_vertical_row(row);
                let (steps, total) =
                    measure_vertical_steps(window, &tokens, font_size, snap.display.weight);
                self.vert_main_cache = Some(VertMeasureCache {
                    text: row.text.clone(),
                    font_bits: font_size.to_bits(),
                    weight_bits: snap.display.weight.to_bits(),
                    tokens,
                    steps,
                    total,
                });
            }
            let main_cache = self.vert_main_cache.as_ref().unwrap();
            let main_total = main_cache.total;
            if auto_scroll {
                let main_wt = shaped_active.as_ref().is_some_and(|s| s.word_timed);
                let main_target = compute_vertical_scroll_target(
                    &main_cache.tokens,
                    &main_cache.steps,
                    main_total,
                    vis_h,
                    snap.position_ms,
                    row.start_ms,
                    row.duration_ms(),
                    main_wt,
                );
                self.vert_main_scroll += (main_target - self.vert_main_scroll) * scroll_alpha;
                if (self.vert_main_scroll - main_target).abs() < 0.2 {
                    self.vert_main_scroll = main_target;
                }
                vert_main_scroll = self.vert_main_scroll;
            } else {
                self.vert_main_scroll = 0.0;
                vert_main_scroll = 0.0;
                if main_total > vis_h && main_total > 1.0 {
                    vert_eff_main_fs = (font_size * (vis_h / main_total) * 0.96).clamp(12.0, font_size);
                }
            }

            if show_trans && !row.translation.trim().is_empty() {
                let max_trans_fs = (vert_eff_main_fs * 0.62).clamp(10.0, 40.0);
                if !self
                    .vert_trans_cache
                    .as_ref()
                    .is_some_and(|c| c.matches(&row.translation, max_trans_fs, snap.display.weight))
                {
                    let trans_row = Row {
                        group: row.group,
                        text: row.translation.clone(),
                        words: Vec::new(),
                        translation: String::new(),
                        romanization: String::new(),
                        start_ms: row.start_ms,
                        end_ms: row.end_ms,
                        word_timed: false,
                        is_bg: false,
                        is_duet: false,
                    };
                    let tokens = tokenize_vertical_row(&trans_row);
                    let (steps, total) =
                        measure_vertical_steps(window, &tokens, max_trans_fs, snap.display.weight);
                    self.vert_trans_cache = Some(VertMeasureCache {
                        text: row.translation.clone(),
                        font_bits: max_trans_fs.to_bits(),
                        weight_bits: snap.display.weight.to_bits(),
                        tokens,
                        steps,
                        total,
                    });
                }
                let trans_cache = self.vert_trans_cache.as_ref().unwrap();
                let trans_total = trans_cache.total;
                if auto_scroll {
                    vert_eff_trans_fs = max_trans_fs;
                    let trans_target = compute_vertical_scroll_target(
                        &trans_cache.tokens,
                        &trans_cache.steps,
                        trans_total,
                        vis_h,
                        snap.position_ms,
                        row.start_ms,
                        row.duration_ms(),
                        false,
                    );
                    self.vert_trans_scroll += (trans_target - self.vert_trans_scroll) * scroll_alpha;
                    if (self.vert_trans_scroll - trans_target).abs() < 0.2 {
                        self.vert_trans_scroll = trans_target;
                    }
                    vert_trans_scroll = self.vert_trans_scroll;
                } else {
                    self.vert_trans_scroll = 0.0;
                    vert_trans_scroll = 0.0;
                    vert_eff_trans_fs = if trans_total > vis_h && trans_total > 1.0 {
                        (max_trans_fs * (vis_h / trans_total) * 0.96).clamp(10.0, max_trans_fs)
                    } else {
                        max_trans_fs
                    };
                }
            } else {
                self.vert_trans_scroll = 0.0;
                vert_trans_scroll = 0.0;
            }
        } else {
            self.vert_main_scroll = 0.0;
            self.vert_trans_scroll = 0.0;
            vert_main_scroll = 0.0;
            vert_trans_scroll = 0.0;
        }

        let toast_info = self.toast.as_ref().and_then(|(msg, until)| {
            if now >= *until {
                None
            } else {
                let a = ((*until - now) / 300.0).clamp(0.0, 1.0) as f32;
                Some((msg.clone(), a))
            }
        });
        if toast_info.is_none() {
            self.toast = None;
        }
        let hover_resize_edges = self
            .cursor
            .map(|(mx, my)| Self::hit_resize_edges(mx, my, win_w, win_h));
        let active_cursor = if self.cfg.locked {
            CursorStyle::Arrow
        } else {
            self.resize
                .map(|r| r.edges)
                .or(hover_resize_edges)
                .and_then(ResizeEdges::cursor_style)
                .unwrap_or(CursorStyle::Arrow)
        };

        let play_target = if snap.playing { 1.0 } else { 0.0 };
        let animating = snap.playing
            || hovering
            || self.hover_alpha > 0.001
            || (self.bar_side_blend - target_bar_side).abs() > 0.001
            || (self.trans_right_blend - target_trans_right).abs() > 0.001
            || line_fade < 0.999
            || (self.play_morph.position() - play_target).abs() > 0.002
            || interlude_snap.is_some()
            || toast_info.is_some()
            || btn_animating
            || !self.configured_hwnd;
        if animating && window.is_visible() {
            window.request_animation_frame();
        } else if !self.idle_poll_pending {
            self.idle_poll_pending = true;
            let timer = cx
                .background_executor()
                .timer(std::time::Duration::from_millis(120));
            cx.spawn(async move |view, cx| {
                timer.await;
                let _ = view.update(cx, |v, cx| {
                    v.idle_poll_pending = false;
                    cx.notify();
                });
            })
            .detach();
        }

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .cursor(active_cursor)
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.pressed_btn.is_some() {
                    this.press_gesture.update(x, y);
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    if this.cfg.locked {
                        return;
                    }
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if let Some((btn, _rect)) = this.hit_button(x, y) {
                        this.pressed_btn = Some(btn);
                        this.press_gesture.begin(x, y);
                    } else if let Some(cpos) = win32_window::cursor_screen_pos() {
                        this.close_popovers(cx);
                        let wb = window.bounds();
                        let origin = (f32::from(wb.origin.x), f32::from(wb.origin.y));
                        let vp = window.viewport_size();
                        let (vw, vh) = (f32::from(vp.width), f32::from(vp.height));
                        let edges = Self::hit_resize_edges(x, y, vw, vh);
                        this.last_origin = Some(origin);
                        this.last_size = Some((vw, vh));
                        if edges.any() {
                            let sf = window.scale_factor().max(0.5);
                            let start_rect_phys = win32_window::hwnd_from_window(window)
                                .and_then(win32_window::window_rect_physical)
                                .unwrap_or((
                                    (origin.0 * sf).round() as i32,
                                    (origin.1 * sf).round() as i32,
                                    (vw * sf).round() as i32,
                                    (vh * sf).round() as i32,
                                ));
                            this.resize = Some(ResizeState {
                                edges,
                                start_cursor_phys: cpos,
                                start_rect_phys,
                                last_applied_phys: start_rect_phys,
                            });
                        } else {
                            this.drag = Some(DragState {
                                start_cursor_phys: cpos,
                                start_origin_logical: origin,
                                snapped_x: false,
                                snapped_y: false,
                            });
                        }
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, cx| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    this.press_gesture.update(x, y);
                    if let Some(btn) = this.pressed_btn.take() {
                        let hit = this.hit_button(x, y);
                        let same = hit.is_some_and(|(b, _)| b == btn);
                        if this.press_gesture.release(same) {
                            if let Some((_, rect)) = hit {
                                this.handle_button(btn, rect, window, cx);
                            }
                        }
                    } else {
                        this.press_gesture.cancel();
                    }
                    cx.notify();
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        chrome::set_light_theme(false);

                        // 0. Subtle compact outer hover/drag/resize frame around the lyrics window
                        if hover_alpha > 0.01 {
                            let fr = Box4::new(
                                2.0,
                                2.0,
                                (win_w - 4.0).max(1.0),
                                (win_h - 4.0).max(1.0),
                            );
                            chrome::fill_round(
                                window,
                                fr.x,
                                fr.y,
                                fr.w,
                                fr.h,
                                15.0,
                                Hsla::from(gpui_kit::rgb(0x0E1118)).opacity(0.16 * hover_alpha),
                            );
                            chrome::stroke_round(
                                window,
                                fr.x - 0.5,
                                fr.y - 0.5,
                                fr.w + 1.0,
                                fr.h + 1.0,
                                15.5,
                                1.0,
                                Hsla::from(gpui_kit::rgb(0x000000)).opacity(0.10 * hover_alpha),
                            );
                            let border_a = if hover_resize_edges.is_some_and(|e| e.any()) {
                                0.44
                            } else {
                                0.16
                            };
                            chrome::stroke_round(
                                window,
                                fr.x,
                                fr.y,
                                fr.w,
                                fr.h,
                                15.0,
                                1.0,
                                Hsla::from(gpui_kit::rgb(0xFFFFFF)).opacity(border_a * hover_alpha),
                            );
                        }

                        // 1. Borderless 7-button elastic hover bar (smooth screen-half position, no lyric push!)
                        if hover_alpha > 0.01 {
                            for ((btn, icon), (_, rect)) in btn_specs.iter().zip(buttons.iter()) {
                                let fb = btn_feedback[btn.index()];
                                let swell = (fb.press * 0.95 + fb.hover * 0.22).clamp(0.0, 1.15);
                                if swell > 0.01 {
                                    let sx = (1.0 + fb.squeeze[0] * 0.45).max(0.75);
                                    let sy = (1.0 + fb.squeeze[1] * 0.45).max(0.75);
                                    let lw = rect.w * sx;
                                    let lh = rect.h * sy;
                                    let (rcx, rcy) = rect.centre();
                                    let lens_r = Box4::new(
                                        rcx - lw * 0.5 + fb.pull[0] * 0.65,
                                        rcy - lh * 0.5 + fb.pull[1] * 0.65,
                                        lw,
                                        lh,
                                    );
                                    chrome::paint_segmented_lens(
                                        window,
                                        lens_r,
                                        lens_r.w.min(lens_r.h) * 0.5,
                                        swell,
                                        fb,
                                        hover_alpha,
                                    );
                                }
                                let (bcx, bcy) = rect.centre();
                                let d = if *btn == Btn::Play { 16.0 } else { 15.0 };
                                let icon_box = Box4::new(bcx - d * 0.5, bcy - d * 0.5, d, d);
                                let base_op = if *btn == Btn::Translate && !show_trans {
                                    0.58
                                } else {
                                    0.84
                                };
                                let icon_col = Hsla::from(gpui_kit::rgb(0xFFFFFF))
                                    .opacity(((base_op + 0.16 * fb.hover).min(1.0)) * hover_alpha);
                                let shadow_box =
                                    Box4::new(icon_box.x, icon_box.y + 1.0, icon_box.w, icon_box.h);
                                let shadow_col = Hsla::from(gpui_kit::rgb(0x000000))
                                    .opacity(0.45 * hover_alpha);
                                if *btn == Btn::Play {
                                    icons::paint_toggle(
                                        window,
                                        cx,
                                        Icon::Play,
                                        Icon::Pause,
                                        shadow_box,
                                        shadow_col,
                                        fb,
                                        play_morph,
                                    );
                                    icons::paint_toggle(
                                        window,
                                        cx,
                                        Icon::Play,
                                        Icon::Pause,
                                        icon_box,
                                        icon_col,
                                        fb,
                                        play_morph,
                                    );
                                } else {
                                    icons::paint_elastic(
                                        window, cx, *icon, shadow_box, shadow_col, fb,
                                    );
                                    icons::paint_elastic(
                                        window, cx, *icon, icon_box, icon_col, fb,
                                    );
                                }
                            }
                        }

                        // Centerline magnetic snap guides while dragging
                        if snap_guide_x {
                            chrome::fill_round(
                                window,
                                win_w * 0.5 - 0.5,
                                6.0,
                                1.0,
                                (win_h - 12.0).max(1.0),
                                0.5,
                                Hsla::from(gpui_kit::rgb(0xFFFFFF)).opacity(0.45),
                            );
                        }
                        if snap_guide_y {
                            chrome::fill_round(
                                window,
                                6.0,
                                win_h * 0.5 - 0.5,
                                (win_w - 12.0).max(1.0),
                                1.0,
                                0.5,
                                Hsla::from(gpui_kit::rgb(0xFFFFFF)).opacity(0.45),
                            );
                        }

                        // Stationary lyric bounds (never pushed aside on hover!)
                        let horiz_area_top = 8.0_f32;
                        let horiz_area_h = (win_h - 16.0).max(60.0);
                        let vert_area_left = 8.0_f32;
                        let vert_area_w = (win_w - 16.0).max(80.0);

                        // 2. Native AMLL single-line lyric + translation OR InterludeDots OR fallback
                        match (snap.active_row.as_ref(), shaped_active.as_ref()) {
                            (Some(row), Some(shaped)) => match orient {
                                DesktopOrientation::Horizontal => {
                                    let light =
                                        row_light(row, snap.position_ms, true, false, &snap.display);
                                    let progress = paint::line_progress(
                                        snap.position_ms,
                                        row.start_ms,
                                        row.duration_ms(),
                                    );
                                    let masks: Vec<f32> = shaped
                                        .masks
                                        .iter()
                                        .map(|frames| position_at(frames, progress))
                                        .collect();
                                    let row_light_val = RowLight {
                                        base: light.dark * line_fade,
                                        dark: light.dark * line_fade,
                                        bright: light.bright * line_fade,
                                        mask: if light.sweep.is_some() {
                                            Some(masks)
                                        } else {
                                            None
                                        },
                                        glow: light.glow.map(|mut g| {
                                            for v in &mut g {
                                                *v *= line_fade;
                                            }
                                            g
                                        }),
                                    };
                                    let has_trans =
                                        show_trans && !row.translation.trim().is_empty();
                                    let pad_x = 24.0_f32;
                                    let natural_w = shaped
                                        .lines
                                        .first()
                                        .map(|l| f32::from(l.layout.width))
                                        .unwrap_or(0.0);
                                    let horiz_overflow = auto_scroll && natural_w > horiz_avail_w;
                                    let trans_fs = (shaped.font_size * 0.62).clamp(11.0, 40.0);
                                    let total_h = if has_trans {
                                        shaped.line_height + 4.0 + trans_fs * 1.25
                                    } else {
                                        shaped.line_height
                                    };
                                    let origin_y = horiz_area_top + (horiz_area_h - total_h) * 0.5;
                                    let (origin_x, wrap_w, align) = if horiz_overflow {
                                        (pad_x - horiz_main_scroll, natural_w + 32.0, LineAlign::Left)
                                    } else {
                                        ((win_w - horiz_avail_w) * 0.5, horiz_avail_w, LineAlign::Center)
                                    };
                                    let draw_main = |window: &mut Window| {
                                        paint::paint_row_with_shadow(
                                            window,
                                            shaped,
                                            origin_x,
                                            origin_y,
                                            shaped.line_height,
                                            shaped.font_size,
                                            &shaped.words,
                                            &row_light_val,
                                            snap.position_ms,
                                            1.0,
                                            WrapperTransform::default(),
                                            -10000.0,
                                            10000.0,
                                            1.0,
                                            wrap_w,
                                            align,
                                            0.0,
                                            DESKTOP_DROP_SHADOW,
                                            Some(text_color),
                                        );
                                    };
                                    if horiz_overflow {
                                        window.with_content_mask(
                                            Some(ContentMask {
                                                bounds: gpui_kit::bounds(
                                                    point(px(pad_x), px(0.0)),
                                                    gpui_kit::size(px(horiz_avail_w), px(win_h)),
                                                ),
                                            }),
                                            draw_main,
                                        );
                                    } else {
                                        draw_main(window);
                                    }
                                    if has_trans {
                                        let trans_a = (snap.display.trans_current.max(0.75)
                                            * line_fade)
                                            .clamp(0.0, 1.0);
                                        paint_horiz_translation(
                                            window,
                                            &row.translation,
                                            win_w,
                                            origin_y + shaped.line_height + 4.0,
                                            trans_fs,
                                            text_color,
                                            trans_a,
                                            auto_scroll,
                                            horiz_trans_scroll,
                                        );
                                    }
                                }
                                DesktopOrientation::Vertical => {
                                    let has_trans =
                                        show_trans && !row.translation.trim().is_empty();
                                    // Main lyric stays on screen outer side, translation on inner side:
                                    // Left half of screen (trans_right_t = 0): main LEFT (0.34), translation RIGHT (0.72).
                                    // Right half of screen (trans_right_t = 1): translation LEFT (0.28), main RIGHT (0.66).
                                    let main_cx = if has_trans {
                                        let left_half_main = vert_area_left + vert_area_w * 0.34;
                                        let right_half_main = vert_area_left + vert_area_w * 0.66;
                                        left_half_main + (right_half_main - left_half_main) * trans_right_t
                                    } else {
                                        vert_area_left + vert_area_w * 0.5
                                    };
                                    paint_vertical_text_column(
                                        window,
                                        main_cx,
                                        win_h,
                                        row,
                                        Some(shaped),
                                        &snap.display,
                                        snap.position_ms,
                                        vert_eff_main_fs,
                                        text_color,
                                        1.0,
                                        line_fade,
                                        vert_main_scroll,
                                    );
                                    if has_trans {
                                        let left_half_trans = vert_area_left + vert_area_w * 0.72;
                                        let right_half_trans = vert_area_left + vert_area_w * 0.28;
                                        let trans_cx = left_half_trans
                                            + (right_half_trans - left_half_trans) * trans_right_t;
                                        let trans_row = Row {
                                            group: row.group,
                                            text: row.translation.clone(),
                                            words: Vec::new(),
                                            translation: String::new(),
                                            romanization: String::new(),
                                            start_ms: row.start_ms,
                                            end_ms: row.end_ms,
                                            word_timed: false,
                                            is_bg: false,
                                            is_duet: false,
                                        };
                                        paint_vertical_text_column(
                                            window,
                                            trans_cx,
                                            win_h,
                                            &trans_row,
                                            None,
                                            &snap.display,
                                            snap.position_ms,
                                            vert_eff_trans_fs,
                                            text_color,
                                            snap.display.trans_current.max(0.75),
                                            line_fade,
                                            vert_trans_scroll,
                                        );
                                    }
                                }
                            },
                            _ => {
                                if let Some(isnap) = interlude_snap.as_ref() {
                                    let (ix, iy) = match orient {
                                        DesktopOrientation::Horizontal => {
                                            (win_w * 0.5, horiz_area_top + horiz_area_h * 0.5)
                                        }
                                        DesktopOrientation::Vertical => {
                                            (vert_area_left + vert_area_w * 0.5, win_h * 0.5)
                                        }
                                    };
                                    paint_interlude_dots(
                                        window,
                                        orient,
                                        ix,
                                        iy,
                                        font_size,
                                        snap.display.dot_scale,
                                        text_color,
                                        isnap,
                                    );
                                } else if snap.interlude_range.is_none() {
                                    let fallback = if !snap.track_title.is_empty() {
                                        if snap.track_artist.is_empty() {
                                            snap.track_title.clone()
                                        } else {
                                            format!("{} · {}", snap.track_title, snap.track_artist)
                                        }
                                    } else {
                                        "OverLyrics · 等待播放".into()
                                    };
                                    match orient {
                                        DesktopOrientation::Horizontal => {
                                            let fs = (font_size * 0.68).clamp(16.0, 26.0);
                                            let cy = horiz_area_top + horiz_area_h * 0.5;
                                            paint_horiz_translation(
                                                window,
                                                &fallback,
                                                win_w,
                                                cy - fs * 0.62,
                                                fs,
                                                text_color,
                                                0.78,
                                                false,
                                                0.0,
                                            );
                                        }
                                        DesktopOrientation::Vertical => {
                                            let col_cx = vert_area_left + vert_area_w * 0.5;
                                            let fb_row = Row {
                                                group: 0,
                                                text: fallback,
                                                words: Vec::new(),
                                                translation: String::new(),
                                                romanization: String::new(),
                                                start_ms: 0.0,
                                                end_ms: 0.0,
                                                word_timed: false,
                                                is_bg: false,
                                                is_duet: false,
                                            };
                                            paint_vertical_text_column(
                                                window,
                                                col_cx,
                                                win_h,
                                                &fb_row,
                                                None,
                                                &snap.display,
                                                0.0,
                                                (font_size * 0.66).clamp(15.0, 24.0),
                                                text_color,
                                                0.78,
                                                1.0,
                                                0.0,
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        // 3. Brief lock/unlock toast
                        if let Some((msg, ta)) = toast_info.as_ref() {
                            let (tw, _) = chrome::measure(window, msg, 12.0, 600.0);
                            let pw = tw + 24.0;
                            let ph = 26.0;
                            let px0 = (win_w - pw) * 0.5;
                            let py0 = 8.0;
                            chrome::fill_round(
                                window,
                                px0,
                                py0,
                                pw,
                                ph,
                                13.0,
                                Hsla::from(gpui_kit::rgb(0x101218)).opacity(0.78 * *ta),
                            );
                            chrome::paint_text_vcentred(
                                window,
                                msg,
                                px0 + 12.0,
                                py0 + ph * 0.5,
                                12.0,
                                600.0,
                                Hsla::from(gpui_kit::rgb(0xFFFFFF)).opacity(0.95 * *ta),
                            );
                        }
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

/// Independent Liquid Glass Volume popover window — reuses `paint_volume_panel_contents` directly!
struct DesktopVolumePopover {
    bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    track_fb: Feedback,
    cursor: Option<(f32, f32)>,
    dragging: bool,
    was_active: bool,
    configured: bool,
    started: Instant,
    last_ms: f64,
    track: Box4,
}

impl DesktopVolumePopover {
    fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
        Self {
            bridge,
            motion: PanelMotion::default(),
            track_fb: Feedback::default(),
            cursor: None,
            dragging: false,
            was_active: false,
            configured: false,
            started: Instant::now(),
            last_ms: 0.0,
            track: Box4::new(14.0, 50.0, 212.0, 24.0),
        }
    }

    fn apply_x(&self, x: f32) {
        let f = ((x - self.track.x) / self.track.w.max(1.0)).clamp(0.0, 1.0) as f64;
        self.bridge.push(DesktopCommand::SetVolumePct(f * 100.0));
    }
}

impl Render for DesktopVolumePopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if !self.configured {
                self.configured = true;
                window.defer(cx, move |_, _| {
                    win32_window::configure_desktop_overlay(hwnd);
                });
            }
        }
        if window.is_window_active() {
            self.was_active = true;
        } else if self.was_active && !self.dragging {
            self.bridge.mark_volume_closed();
            window.remove_window();
            return div().into_any_element();
        }
        if self.dragging && !win32_window::is_left_mouse_down() {
            self.dragging = false;
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;
        self.motion.step(true, dt);

        let snap = self.bridge.read_snapshot();
        let vp = window.viewport_size();
        let w = f32::from(vp.width).max(1.0);
        let h = f32::from(vp.height).max(1.0);
        let track = Box4::new(14.0, 50.0, (w - 28.0).max(60.0), 24.0);
        self.track = track;
        let hovered = self.cursor.is_some_and(|(mx, my)| track.contains(mx, my));
        self.track_fb.step(hovered, self.dragging, dt);
        let track_fb = self.track_fb;
        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let panel_radius = snap.display.corner_radius.clamp(12.0, 22.0);
        let panel_rect = GlassRect {
            left: 1.0,
            top: 1.0,
            width: (w - 2.0).max(1.0) as f64,
            height: (h - 2.0).max(1.0) as f64,
        };

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.dragging {
                    this.apply_x(x);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let x = f32::from(ev.position.x);
                    this.dragging = true;
                    this.apply_x(x);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.dragging = false;
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        chrome::set_light_theme(light_panel);
                        paint_sidecar_glass_frame(
                            window,
                            Box4::new(1.0, 1.0, (w - 2.0).max(1.0), (h - 2.0).max(1.0)),
                            panel_radius,
                            theme_mode,
                            surf_a,
                        );
                        paint_volume_panel_contents(
                            window,
                            panel_rect,
                            snap.volume_pct,
                            snap.volume_enabled,
                            snap.volume_status.as_str(),
                            snap.volume_label.as_str(),
                            snap.display.accent_hex,
                            track_fb,
                            cont_a,
                        );
                        chrome::set_light_theme(false);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

/// Independent Liquid Glass Lyrics Version Picker popover — reuses `paint_lyrics_picker_contents`!
struct DesktopLyricsPickerPopover {
    bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    cursor: Option<(f32, f32)>,
    scroll_spring: Spring,
    refresh_angle: f32,
    import_fb: Feedback,
    refresh_fb: Feedback,
    cand_fb: HashMap<usize, Feedback>,
    smart_fb: HashMap<usize, Feedback>,
    smart_springs: HashMap<String, Spring>,
    was_active: bool,
    keep_open_for_dialog: bool,
    configured: bool,
    started: Instant,
    last_ms: f64,
}

impl DesktopLyricsPickerPopover {
    fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
        let mut scroll_spring = Spring::new(0.0);
        scroll_spring.update_params(SpringParams::new(220.0, 26.0));
        Self {
            bridge,
            motion: PanelMotion::default(),
            cursor: None,
            scroll_spring,
            refresh_angle: 0.0,
            import_fb: Feedback::default(),
            refresh_fb: Feedback::default(),
            cand_fb: HashMap::new(),
            smart_fb: HashMap::new(),
            smart_springs: HashMap::new(),
            was_active: false,
            keep_open_for_dialog: false,
            configured: false,
            started: Instant::now(),
            last_ms: 0.0,
        }
    }
}

impl Render for DesktopLyricsPickerPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if !self.configured {
                self.configured = true;
                window.defer(cx, move |_, _| {
                    win32_window::configure_desktop_overlay(hwnd);
                });
            }
        }
        if window.is_window_active() {
            self.was_active = true;
            self.keep_open_for_dialog = false;
        } else if self.was_active && !self.keep_open_for_dialog {
            window.remove_window();
            return div().into_any_element();
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;
        self.motion.step(true, dt);

        let snap = self.bridge.read_snapshot();
        let vp = window.viewport_size();
        let w = f32::from(vp.width).max(1.0);
        let h = f32::from(vp.height).max(1.0);
        let panel_rect = GlassRect {
            left: 1.0,
            top: 1.0,
            width: (w - 2.0).max(1.0) as f64,
            height: (h - 2.0).max(1.0) as f64,
        };
        let capacity = (((h - VERSION_HEADER) / VERSION_ROW).floor() as usize).clamp(1, 4);
        let max_scroll_px = snap.candidates.len().saturating_sub(capacity) as f32 * VERSION_ROW;
        let clamped_t = (self.scroll_spring.target() as f32).clamp(0.0, max_scroll_px);
        self.scroll_spring.set_target_position(clamped_t as f64);
        self.scroll_spring.update(dt / 1000.0);
        let scroll_px = (self.scroll_spring.position() as f32).clamp(0.0, max_scroll_px);

        if snap.refresh_searching {
            self.refresh_angle = (self.refresh_angle
                + std::f32::consts::TAU * 1.45 * (dt as f32 / 1000.0))
                % std::f32::consts::TAU;
        } else if self.refresh_angle > 0.0 {
            let next = self.refresh_angle + std::f32::consts::TAU * 1.45 * (dt as f32 / 1000.0);
            self.refresh_angle = if next >= std::f32::consts::TAU { 0.0 } else { next };
        }

        let import_box = picker_import_rect(panel_rect);
        let refresh_box = picker_refresh_rect(panel_rect);
        self.import_fb.step(
            self.cursor.is_some_and(|(mx, my)| import_box.contains(mx, my)),
            false,
            dt,
        );
        self.refresh_fb.step(
            self.cursor.is_some_and(|(mx, my)| refresh_box.contains(mx, my)),
            false,
            dt,
        );

        let list_top = 1.0 + VERSION_HEADER;
        let list_bot = list_top + capacity as f32 * VERSION_ROW;
        let mut smart_fractions = Vec::new();
        for (i, entry) in snap.candidates.iter().enumerate() {
            let y = list_top + i as f32 * VERSION_ROW - scroll_px;
            let row_rect = if y + VERSION_ROW > list_top + 2.0 && y < list_bot - 2.0 {
                Some(Box4::new(9.0, y, w - 18.0, VERSION_ROW - 4.0))
            } else {
                None
            };
            let smart_rect = row_rect.map(picker_smart_btn_rect);
            let smart_hot = smart_rect
                .is_some_and(|sr| self.cursor.is_some_and(|(mx, my)| sr.contains(mx, my)));
            let row_hot = !smart_hot
                && row_rect
                    .is_some_and(|rr| self.cursor.is_some_and(|(mx, my)| rr.contains(mx, my)));
            self.cand_fb.entry(i).or_default().step(row_hot, false, dt);
            self.smart_fb.entry(i).or_default().step(smart_hot, false, dt);

            if !entry.has_translation && !entry.smart_translation_key.is_empty() {
                let target = if entry.smart_translation { 1.0 } else { 0.0 };
                let sp = self
                    .smart_springs
                    .entry(entry.smart_translation_key.clone())
                    .or_insert_with(|| state_icon_spring(target));
                sp.set_target_position(target);
                sp.update(dt / 1000.0);
                smart_fractions.push((
                    entry.smart_translation_key.clone(),
                    sp.position().clamp(0.0, 1.0) as f32,
                ));
            }
        }

        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let panel_radius = snap.display.corner_radius.clamp(12.0, 22.0);
        let refresh_angle = self.refresh_angle;
        let import_fb = self.import_fb;
        let refresh_fb = self.refresh_fb;
        let cand_fb = self.cand_fb.clone();
        let smart_fb = self.smart_fb.clone();
        let candidates_for_click = snap.candidates.clone();

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                this.cursor = Some((f32::from(ev.position.x), f32::from(ev.position.y)));
            }))
            .on_scroll(cx.listener(move |this, ev: &ScrollEvent, _, _| {
                let dy = match ev.delta {
                    ScrollDelta::Lines(p) => p.y * VERSION_ROW * 0.85,
                    ScrollDelta::Pixels(p) => f32::from(p.y),
                };
                let next = ((this.scroll_spring.target() as f32) - dy).clamp(0.0, max_scroll_px);
                this.scroll_spring.set_target_position(next as f64);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if import_box.contains(x, y) {
                        this.keep_open_for_dialog = true;
                        this.bridge.push(DesktopCommand::ImportLyrics);
                        return;
                    }
                    if refresh_box.contains(x, y) {
                        this.refresh_fb.trigger_hop(1.8);
                        this.bridge.push(DesktopCommand::RefreshLyrics);
                        return;
                    }
                    let cur_scroll = (this.scroll_spring.position() as f32).clamp(0.0, max_scroll_px);
                    for (i, entry) in candidates_for_click.iter().enumerate() {
                        let ry = list_top + i as f32 * VERSION_ROW - cur_scroll;
                        if ry + VERSION_ROW <= list_top + 2.0 || ry >= list_bot - 2.0 {
                            continue;
                        }
                        let r = Box4::new(9.0, ry, w - 18.0, VERSION_ROW - 4.0);
                        if !entry.has_translation
                            && !entry.smart_translation_key.is_empty()
                            && picker_smart_btn_rect(r).contains(x, y)
                        {
                            this.bridge.push(DesktopCommand::ToggleSmartTranslation(i));
                            return;
                        }
                        if r.contains(x, y) {
                            this.bridge.push(DesktopCommand::PickCandidate(i));
                            return;
                        }
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        chrome::set_light_theme(light_panel);
                        paint_sidecar_glass_frame(
                            window,
                            Box4::new(1.0, 1.0, (w - 2.0).max(1.0), (h - 2.0).max(1.0)),
                            panel_radius,
                            theme_mode,
                            surf_a,
                        );
                        paint_lyrics_picker_contents(
                            window,
                            cx,
                            panel_rect,
                            "歌词版本",
                            true,
                            &snap.candidates,
                            scroll_px,
                            &smart_fractions,
                            refresh_angle,
                            snap.display.accent_hex,
                            light_panel,
                            cont_a,
                            snap.track_title.as_str(),
                            import_fb,
                            refresh_fb,
                            |i| cand_fb.get(&i).copied().unwrap_or_default(),
                            |i| smart_fb.get(&i).copied().unwrap_or_default(),
                        );
                        chrome::set_light_theme(false);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MoreHit {
    OrientH,
    OrientV,
    SyncMinus,
    SyncPlus,
    FontMinus,
    FontPlus,
    AutoScroll,
    ColorPill,
    LyricsPicker,
    ReturnMain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteDrag {
    Sv,
    Hue,
}

/// Independent Liquid Glass Settings popover for Desktop Lyrics, aligned with the main `···` menu.
struct DesktopMorePopover {
    bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    orient_lens: SegmentedLens,
    orient_dragging: bool,
    orient_track: (f32, f32),
    orient_well: Box4,
    auto_scroll_spring: Spring,
    feedback: HashMap<MoreHit, Feedback>,
    elastics: HashMap<MoreHit, Elastic>,
    panel_elastic: Elastic,
    press_gesture: PressGesture,
    pressed_hit: Option<MoreHit>,
    panel_dragging: bool,
    card_rect: Box4,
    color_row_center_y: f32,
    was_active: bool,
    configured: bool,
    cursor: Option<(f32, f32)>,
    hits: Vec<(MoreHit, Box4)>,
    started: Instant,
    last_ms: f64,
}

impl DesktopMorePopover {
    fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
        let cfg = bridge.read_cfg();
        Self {
            bridge,
            motion: PanelMotion::default(),
            orient_lens: SegmentedLens::default(),
            orient_dragging: false,
            orient_track: (POPOVER_ELASTIC_PAD + 10.0, 232.0),
            orient_well: Box4::default(),
            auto_scroll_spring: state_icon_spring(if cfg.auto_scroll { 1.0 } else { 0.0 }),
            feedback: HashMap::new(),
            elastics: HashMap::new(),
            panel_elastic: Elastic::default(),
            press_gesture: PressGesture::default(),
            pressed_hit: None,
            panel_dragging: false,
            card_rect: Box4::default(),
            color_row_center_y: 0.0,
            was_active: false,
            configured: false,
            cursor: None,
            hits: Vec::new(),
            started: Instant::now(),
            last_ms: 0.0,
        }
    }
}

/// Paints the GPU-accelerated 2D HSV Saturation-Value plane + 6-stop Rainbow Hue bar.
#[allow(clippy::too_many_arguments)]
fn paint_hsv_palette(
    window: &mut Window,
    sv_rect: Box4,
    hue_rect: Box4,
    hsv: (f32, f32, f32),
    opacity: f32,
) {
    let pure_hue = hsv_to_rgb(hsv.0, 1.0, 1.0);
    // 1. Mathematically exact SV square in 2 GPU linear-gradient quads:
    //    Pass A: Horizontal White -> PureHue(H)
    window.paint_quad(quad(
        bounds(
            point(px(sv_rect.x), px(sv_rect.y)),
            size(px(sv_rect.w), px(sv_rect.h)),
        ),
        Corners::all(px(12.0)),
        linear_gradient(
            90.0,
            linear_color_stop(Hsla::from(rgb(0xFFFFFF)).opacity(opacity), 0.0),
            linear_color_stop(Hsla::from(rgb(pure_hue)).opacity(opacity), 1.0),
        ),
        Edges::default(),
        transparent_black(),
        BorderStyle::Solid,
    ));
    //    Pass B: Vertical Transparent -> Black (multiplies color by V = 1 - y)
    window.paint_quad(quad(
        bounds(
            point(px(sv_rect.x), px(sv_rect.y)),
            size(px(sv_rect.w), px(sv_rect.h)),
        ),
        Corners::all(px(12.0)),
        linear_gradient(
            180.0,
            linear_color_stop(Hsla::from(rgb(0x000000)).opacity(0.0), 0.0),
            linear_color_stop(Hsla::from(rgb(0x000000)).opacity(opacity), 1.0),
        ),
        Edges::all(px(1.0)),
        chrome::themed_border(0.24 * opacity),
        BorderStyle::Solid,
    ));

    // SV Loupe cursor
    let lx = sv_rect.x + sv_rect.w * hsv.1.clamp(0.0, 1.0);
    let ly = sv_rect.y + sv_rect.h * (1.0 - hsv.2.clamp(0.0, 1.0));
    let cur_rgb = hsv_to_rgb(hsv.0, hsv.1, hsv.2);
    chrome::fill_round_subpixel(
        window,
        lx - 9.0,
        ly - 8.0,
        18.0,
        18.0,
        9.0,
        Hsla::from(rgb(0x000000)).opacity(0.32 * opacity),
    );
    chrome::fill_round_subpixel(
        window,
        lx - 7.5,
        ly - 7.5,
        15.0,
        15.0,
        7.5,
        Hsla::from(rgb(cur_rgb)).opacity(opacity),
    );
    chrome::stroke_round(
        window,
        lx - 8.0,
        ly - 8.0,
        16.0,
        16.0,
        8.0,
        2.2,
        Hsla::from(rgb(0xFFFFFF)).opacity(0.98 * opacity),
    );

    // 2. Mathematically exact 6-segment Rainbow Hue bar (0..360 deg)
    const HUE_STOPS: [u32; 7] = [
        0xFF0000, 0xFFFF00, 0x00FF00, 0x00FFFF, 0x0000FF, 0xFF00FF, 0xFF0000,
    ];
    let seg_w = hue_rect.w / 6.0;
    let r_hue = hue_rect.h * 0.5;
    for i in 0..6 {
        let sx = hue_rect.x + i as f32 * seg_w;
        let sw = if i == 5 {
            (hue_rect.right() - sx).max(1.0)
        } else {
            seg_w + 0.6
        };
        let corners = Corners {
            top_left: px(if i == 0 { r_hue } else { 0.0 }),
            bottom_left: px(if i == 0 { r_hue } else { 0.0 }),
            top_right: px(if i == 5 { r_hue } else { 0.0 }),
            bottom_right: px(if i == 5 { r_hue } else { 0.0 }),
        };
        window.paint_quad(quad(
            bounds(point(px(sx), px(hue_rect.y)), size(px(sw), px(hue_rect.h))),
            corners,
            linear_gradient(
                90.0,
                linear_color_stop(Hsla::from(rgb(HUE_STOPS[i])).opacity(opacity), 0.0),
                linear_color_stop(Hsla::from(rgb(HUE_STOPS[i + 1])).opacity(opacity), 1.0),
            ),
            Edges::default(),
            transparent_black(),
            BorderStyle::Solid,
        ));
    }
    chrome::stroke_round(
        window,
        hue_rect.x,
        hue_rect.y,
        hue_rect.w,
        hue_rect.h,
        r_hue,
        0.9,
        chrome::themed_border(0.24 * opacity),
    );

    // Hue thumb
    let hx = hue_rect.x + hue_rect.w * (hsv.0 / 360.0).clamp(0.0, 1.0);
    let hy = hue_rect.centre().1;
    chrome::fill_round_subpixel(
        window,
        hx - 9.5,
        hy - 8.5,
        19.0,
        19.0,
        9.5,
        Hsla::from(rgb(0x000000)).opacity(0.28 * opacity),
    );
    chrome::fill_round_subpixel(
        window,
        hx - 8.5,
        hy - 8.5,
        17.0,
        17.0,
        8.5,
        Hsla::from(rgb(pure_hue)).opacity(opacity),
    );
    chrome::stroke_round(
        window,
        hx - 8.5,
        hy - 8.5,
        17.0,
        17.0,
        8.5,
        2.2,
        Hsla::from(rgb(0xFFFFFF)).opacity(0.98 * opacity),
    );
}

impl Render for DesktopMorePopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if !self.configured {
                self.configured = true;
                window.defer(cx, move |_, _| {
                    win32_window::configure_desktop_overlay(hwnd);
                });
            }
        }
        let color_side = self.bridge.color_popover_side.load(Ordering::SeqCst);
        let color_open = color_side != 0 || self.bridge.color_popover_open.load(Ordering::SeqCst);
        if window.is_window_active() {
            self.was_active = true;
        } else if self.was_active && !self.orient_dragging && !self.panel_dragging && !color_open {
            self.bridge.mark_more_closed();
            window.remove_window();
            return div().into_any_element();
        }
        if !win32_window::is_left_mouse_down() {
            if self.orient_dragging {
                self.orient_dragging = false;
                self.orient_lens.end_drag();
            }
            if self.pressed_hit.is_some() || self.panel_dragging {
                self.pressed_hit = None;
                self.panel_dragging = false;
                self.press_gesture.cancel();
            }
        } else if self.orient_dragging || self.panel_dragging || self.pressed_hit.is_some() {
            // Keep tracking the cursor even when the pointer drags outside the popover window.
            if let Some((cx_p, cy_p)) = win32_window::cursor_screen_pos() {
                let sf = window.scale_factor().max(0.5);
                let wb = window.bounds();
                let lx = (cx_p as f32 / sf) - f32::from(wb.origin.x);
                let ly = (cy_p as f32 / sf) - f32::from(wb.origin.y);
                self.cursor = Some((lx, ly));
                if self.orient_dragging {
                    let (tx, tw) = self.orient_track;
                    self.orient_lens.update_drag(lx, tx, tw, 2, 1.0);
                } else {
                    self.press_gesture.update(lx, ly);
                }
            }
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;
        self.motion.step(true, dt);

        let cfg = self.bridge.read_cfg();
        let snap = self.bridge.read_snapshot();
        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let win_h = f32::from(vp.height).max(1.0);

        let raw_delta = self.press_gesture.delta();
        let panel_held = self.panel_dragging || (self.pressed_hit.is_some() && self.press_gesture.dragged());
        self.panel_elastic.step(raw_delta, panel_held, dt);
        let pp = self.panel_elastic.pull();

        let card_w = (win_w - POPOVER_ELASTIC_PAD * 2.0).max(120.0);
        let card_h = (win_h - POPOVER_ELASTIC_PAD * 2.0).max(120.0);
        let cx0 = POPOVER_ELASTIC_PAD + pp[0];
        let cy0 = POPOVER_ELASTIC_PAD + pp[1];
        let card_rect = Box4::new(cx0, cy0, card_w, card_h);
        self.card_rect = card_rect;

        // Top segmented orientation switcher (aligned with main window `···` mode switcher)
        let track_x = cx0 + 10.0;
        let track_w = (card_w - 20.0).max(40.0);
        let cell_w = track_w * 0.5;
        let base_well = Box4::new(cx0 + 8.0, cy0 + 6.0, (card_w - 16.0).max(40.0), 32.0);
        let seg_h = Box4::new(track_x + 1.0, cy0 + 8.0, (cell_w - 2.0).max(16.0), 28.0);
        let seg_v = Box4::new(track_x + cell_w + 1.0, cy0 + 8.0, (cell_w - 2.0).max(16.0), 28.0);
        self.orient_track = (track_x, track_w);
        self.orient_well = base_well;

        let cfg_orient_slot = match cfg.orientation {
            DesktopOrientation::Horizontal => 0usize,
            DesktopOrientation::Vertical => 1usize,
        };
        let active_orient_hit = if cfg_orient_slot == 0 {
            MoreHit::OrientH
        } else {
            MoreHit::OrientV
        };
        let active_orient_fb = self.feedback.get(&active_orient_hit).copied().unwrap_or_default();
        self.orient_lens.step(
            cfg_orient_slot as f32 * cell_w + 4.0,
            (cell_w - 8.0).max(16.0),
            self.orient_dragging,
            dt,
        );
        let seg_well = self.orient_lens.well_rect(base_well);
        let orient_lens_box = self.orient_lens.rect(track_x, cy0 + 10.0, 24.0, active_orient_fb);
        let orient_swell = self.orient_lens.swell();
        let visual_orient_slot = if self.orient_dragging || self.orient_lens.pending() {
            (((orient_lens_box.centre().0 - track_x) / cell_w).floor() as i32).clamp(0, 1) as usize
        } else {
            cfg_orient_slot
        };

        // 6 compact rows matching main window `···` menu (252px width, 16x16 icons at x+14, 12px labels at x+38)
        let row_x = cx0 + 6.0;
        let row_w = (card_w - 12.0).max(40.0);
        let row_h = 35.0_f32;
        let sync_row = Box4::new(row_x, cy0 + 44.0, row_w, row_h);
        let sync_minus = Box4::new(cx0 + card_w - 119.0, sync_row.y + (row_h - 24.0) * 0.5, 24.0, 24.0);
        let sync_plus = Box4::new(cx0 + card_w - 32.0, sync_row.y + (row_h - 24.0) * 0.5, 24.0, 24.0);

        let font_row = Box4::new(row_x, cy0 + 80.0, row_w, row_h);
        let font_minus = Box4::new(cx0 + card_w - 119.0, font_row.y + (row_h - 24.0) * 0.5, 24.0, 24.0);
        let font_plus = Box4::new(cx0 + card_w - 32.0, font_row.y + (row_h - 24.0) * 0.5, 24.0, 24.0);

        let color_row = Box4::new(row_x, cy0 + 116.0, row_w, row_h);
        let color_pill = Box4::new(cx0 + card_w - 119.0, color_row.y + (row_h - 25.0) * 0.5, 111.0, 25.0);
        self.color_row_center_y = color_row.centre().1;

        let scroll_row = Box4::new(row_x, cy0 + 152.0, row_w, row_h);
        let scroll_track = Box4::new(cx0 + card_w - 46.0, scroll_row.centre().1 - 10.0, 36.0, 20.0);
        self.auto_scroll_spring
            .set_target_position(if cfg.auto_scroll { 1.0 } else { 0.0 });
        self.auto_scroll_spring.update(dt / 1000.0);
        let scroll_frac = self.auto_scroll_spring.position().clamp(0.0, 1.0) as f32;

        let picker_row = Box4::new(row_x, cy0 + 188.0, row_w, row_h);
        let ret_row = Box4::new(row_x, cy0 + 230.0, row_w, row_h);

        let hits = vec![
            (MoreHit::OrientH, seg_h),
            (MoreHit::OrientV, seg_v),
            (MoreHit::SyncMinus, sync_minus),
            (MoreHit::SyncPlus, sync_plus),
            (MoreHit::FontMinus, font_minus),
            (MoreHit::FontPlus, font_plus),
            (MoreHit::ColorPill, color_row),
            (MoreHit::AutoScroll, scroll_row),
            (MoreHit::LyricsPicker, picker_row),
            (MoreHit::ReturnMain, ret_row),
        ];
        self.hits = hits.clone();

        for (hit, rect) in &hits {
            let hot = self.cursor.is_some_and(|(mx, my)| rect.contains(mx, my));
            let held = self.pressed_hit == Some(*hit);
            let fb = self.feedback.entry(*hit).or_default();
            fb.step(hot, held, dt);
            let el = self.elastics.entry(*hit).or_default();
            el.step(raw_delta, held, dt);
            fb.pull = el.pull();
            fb.squeeze = el.squeeze();
            fb.press = el.pressure().clamp(0.0, 1.0);
            if let Some((mx, my)) = self.cursor {
                fb.focus = [
                    ((mx - rect.x) / rect.w.max(1.0) * 2.0 - 1.0).clamp(-1.5, 1.5),
                    ((my - rect.y) / rect.h.max(1.0) * 2.0 - 1.0).clamp(-1.5, 1.5),
                ];
            }
        }

        let fb_map = self.feedback.clone();
        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let panel_radius = snap.display.corner_radius.clamp(12.0, 22.0);
        let accent_hex = snap.display.accent_hex;
        let sync_label = if snap.sync_offset_ms == 0 {
            "0ms".into()
        } else if snap.sync_offset_ms > 0 {
            format!("+{}ms", snap.sync_offset_ms)
        } else {
            format!("{}ms", snap.sync_offset_ms)
        };
        let font_label = format!("{}px", cfg.font_size.round() as i32);
        let color_name = color_preset_label(cfg.text_color);

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.orient_dragging {
                    let (tx, tw) = this.orient_track;
                    this.orient_lens.update_drag(x, tx, tw, 2, 1.0);
                } else if this.pressed_hit.is_some() || this.panel_dragging {
                    this.press_gesture.update(x, y);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if this.orient_well.contains(x, y) {
                        this.orient_dragging = true;
                        let (tx, tw) = this.orient_track;
                        this.orient_lens.update_drag(x, tx, tw, 2, 1.0);
                        return;
                    }
                    let hit = this.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h);
                    this.press_gesture.begin(x, y);
                    if let Some(h) = hit {
                        this.pressed_hit = Some(h);
                        this.panel_dragging = false;
                    } else if this.card_rect.contains(x, y) {
                        this.pressed_hit = None;
                        this.panel_dragging = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if this.orient_dragging {
                        let (tx, tw) = this.orient_track;
                        let slot = this.orient_lens.slot_at_release(tx, tw, 2, x);
                        this.orient_lens.end_drag();
                        this.orient_dragging = false;
                        let target = if slot == 0 {
                            DesktopOrientation::Horizontal
                        } else {
                            DesktopOrientation::Vertical
                        };
                        if target != this.bridge.read_cfg().orientation {
                            this.bridge.push_popover(PopoverAction::SetOrientation(target));
                            window.remove_window();
                        }
                        return;
                    }
                    this.press_gesture.update(x, y);
                    this.panel_dragging = false;
                    let pressed = this.pressed_hit.take();
                    let cur_hit = this.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h);
                    let same = pressed.is_some() && cur_hit == pressed;
                    let clicked = this.press_gesture.release(same);
                    if !clicked {
                        return;
                    }
                    let Some(hit) = pressed else {
                        return;
                    };
                    match hit {
                        MoreHit::OrientH | MoreHit::OrientV => {}
                        MoreHit::SyncMinus => this.bridge.push(DesktopCommand::SyncNudge(-200)),
                        MoreHit::SyncPlus => this.bridge.push(DesktopCommand::SyncNudge(200)),
                        MoreHit::FontMinus => this.bridge.push_popover(PopoverAction::FontDelta(-4.0)),
                        MoreHit::FontPlus => this.bridge.push_popover(PopoverAction::FontDelta(4.0)),
                        MoreHit::AutoScroll => {
                            this.bridge.push_popover(PopoverAction::ToggleAutoScroll);
                        }
                        MoreHit::ColorPill => {
                            let wb = window.bounds();
                            let card_screen = Box4::new(
                                f32::from(wb.origin.x) + POPOVER_ELASTIC_PAD,
                                f32::from(wb.origin.y) + POPOVER_ELASTIC_PAD,
                                this.card_rect.w,
                                this.card_rect.h,
                            );
                            let row_cy_screen = f32::from(wb.origin.y) + this.color_row_center_y;
                            this.bridge.push_popover(PopoverAction::OpenColorPopover(
                                card_screen,
                                row_cy_screen,
                            ));
                        }
                        MoreHit::LyricsPicker => {
                            this.bridge.push_popover(PopoverAction::OpenLyricsPicker);
                            window.remove_window();
                        }
                        MoreHit::ReturnMain => {
                            this.bridge.push_popover(PopoverAction::ReturnToMain);
                            window.remove_window();
                        }
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        chrome::set_light_theme(light_panel);
                        let fb = |h: MoreHit| fb_map.get(&h).copied().unwrap_or_default();
                        let text = |a: f32| chrome::text(a * cont_a);

                        paint_sidecar_glass_frame(
                            window,
                            card_rect,
                            panel_radius,
                            theme_mode,
                            surf_a,
                        );

                        // 1. Draggable segmented orientation switcher (`chrome::SegmentedLens`)
                        chrome::paint_segmented_well(window, seg_well, seg_well.h * 0.5, cont_a);
                        let (rest_alpha, _) = chrome::segmented_crossfade(orient_swell);
                        chrome::paint_segmented_rest_pill(
                            window,
                            orient_lens_box,
                            orient_lens_box.h * 0.5,
                            accent_hex,
                            rest_alpha,
                            cont_a,
                        );
                        for (idx, (label, box_r, hit)) in [
                            ("横排", seg_h, MoreHit::OrientH),
                            ("竖排", seg_v, MoreHit::OrientV),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let selected = idx == visual_orient_slot;
                            let mf = fb(hit);
                            if !selected && mf.hover > 0.01 && orient_swell < 0.1 {
                                chrome::control_surface_alpha(
                                    window,
                                    box_r,
                                    mf,
                                    false,
                                    cont_a * 0.45,
                                );
                            }
                            chrome::paint_text_box_centred(
                                window,
                                label,
                                box_r,
                                12.0,
                                if selected { 650.0 } else { 550.0 },
                                if selected {
                                    chrome::segmented_active_text_blended(
                                        accent_hex,
                                        rest_alpha,
                                        0.96 * cont_a,
                                    )
                                } else {
                                    text(0.62 + 0.22 * mf.hover)
                                },
                            );
                        }
                        if orient_swell > 0.005 {
                            chrome::paint_segmented_lens(
                                window,
                                orient_lens_box,
                                orient_lens_box.h * 0.5,
                                orient_swell,
                                fb(if visual_orient_slot == 0 {
                                    MoreHit::OrientH
                                } else {
                                    MoreHit::OrientV
                                }),
                                cont_a,
                            );
                        }

                        // Top divider
                        chrome::fill_round(
                            window,
                            cx0 + 12.0,
                            cy0 + 41.0,
                            (card_w - 24.0).max(20.0),
                            0.75,
                            0.35,
                            text(0.08),
                        );

                        // Row 1: [Sync] 歌词同步 (- 0ms +)
                        icons::paint(
                            window,
                            cx,
                            Icon::Sync,
                            Box4::new(cx0 + 14.0, sync_row.y + (sync_row.h - 16.0) * 0.5, 16.0, 16.0),
                            text(0.55),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            "歌词同步",
                            cx0 + 38.0,
                            sync_row.centre().1,
                            12.0,
                            500.0,
                            text(0.88),
                        );
                        chrome::paint_stepper(
                            window,
                            sync_minus,
                            sync_plus,
                            fb(MoreHit::SyncMinus),
                            fb(MoreHit::SyncPlus),
                            &sync_label,
                            true,
                            cont_a,
                        );

                        // Row 2: [FontSize] 字号大小 (- 36px +, step 4px)
                        icons::paint(
                            window,
                            cx,
                            Icon::FontSize,
                            Box4::new(cx0 + 14.0, font_row.y + (font_row.h - 16.0) * 0.5, 16.0, 16.0),
                            text(0.55),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            "字号大小",
                            cx0 + 38.0,
                            font_row.centre().1,
                            12.0,
                            500.0,
                            text(0.88),
                        );
                        chrome::paint_stepper(
                            window,
                            font_minus,
                            font_plus,
                            fb(MoreHit::FontMinus),
                            fb(MoreHit::FontPlus),
                            &font_label,
                            true,
                            cont_a,
                        );

                        // Row 3: [Palette] 文字颜色 -> opens secondary DesktopColorPopover
                        let cf = fb(MoreHit::ColorPill);
                        chrome::control_surface_alpha_radius(
                            window, color_row, cf, color_open, cont_a, 13.5,
                        );
                        icons::paint(
                            window,
                            cx,
                            Icon::Palette,
                            Box4::new(
                                cx0 + 14.0,
                                color_row.y + (color_row.h - 16.0) * 0.5,
                                16.0,
                                16.0,
                            ),
                            text(0.55 + 0.25 * cf.hover),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            "文字颜色",
                            cx0 + 38.0,
                            color_row.centre().1,
                            12.0,
                            500.0,
                            text(0.88 + 0.08 * cf.hover),
                        );
                        let color_arrow = if color_side > 0 {
                            chrome::ChoiceArrow::Right
                        } else if color_side < 0 {
                            chrome::ChoiceArrow::Left
                        } else {
                            chrome::ChoiceArrow::Down
                        };
                        chrome::paint_choice_pill_with_arrow(
                            window,
                            color_pill,
                            &format!("    {}", color_name),
                            color_open,
                            color_arrow,
                            accent_hex,
                            cf,
                            cont_a,
                        );
                        chrome::fill_round(
                            window,
                            color_pill.x + 9.0,
                            color_pill.centre().1 - 4.5,
                            9.0,
                            9.0,
                            4.5,
                            Hsla::from(rgb(cfg.text_color)).opacity(cont_a),
                        );
                        chrome::stroke_round(
                            window,
                            color_pill.x + 9.0,
                            color_pill.centre().1 - 4.5,
                            9.0,
                            9.0,
                            4.5,
                            0.75,
                            chrome::themed_border(0.32 * cont_a),
                        );

                        // Row 4: [Lyrics] 长句滚动 (switch)
                        let sf = fb(MoreHit::AutoScroll);
                        chrome::control_surface_alpha_radius(
                            window, scroll_row, sf, false, cont_a, 13.5,
                        );
                        icons::paint(
                            window,
                            cx,
                            Icon::Lyrics,
                            Box4::new(
                                cx0 + 14.0,
                                scroll_row.y + (scroll_row.h - 16.0) * 0.5,
                                16.0,
                                16.0,
                            ),
                            text(0.55 + 0.25 * sf.hover),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            "长句滚动",
                            cx0 + 38.0,
                            scroll_row.centre().1,
                            12.0,
                            500.0,
                            text(0.88 + 0.08 * sf.hover),
                        );
                        chrome::paint_switch_with_accent(
                            window,
                            scroll_track.x,
                            scroll_track.y,
                            scroll_track.w,
                            scroll_track.h,
                            cfg.auto_scroll,
                            scroll_frac,
                            sf,
                            accent_hex,
                            cont_a,
                        );

                        // Row 5: [List] 歌词版本
                        let pf = fb(MoreHit::LyricsPicker);
                        chrome::control_surface_alpha_radius(
                            window, picker_row, pf, false, cont_a, 13.5,
                        );
                        chrome::chevron(
                            window,
                            picker_row.right() - 16.0,
                            picker_row.y + (picker_row.h - 8.0) * 0.5,
                            text(0.36 + 0.30 * pf.hover),
                        );
                        icons::paint(
                            window,
                            cx,
                            Icon::List,
                            Box4::new(
                                cx0 + 14.0,
                                picker_row.y + (picker_row.h - 16.0) * 0.5,
                                16.0,
                                16.0,
                            ),
                            text(0.55 + 0.25 * pf.hover),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            "歌词版本",
                            cx0 + 38.0,
                            picker_row.centre().1,
                            12.0,
                            500.0,
                            text(0.88 + 0.08 * pf.hover),
                        );

                        // Bottom divider
                        chrome::fill_round(
                            window,
                            cx0 + 12.0,
                            cy0 + 227.0,
                            (card_w - 24.0).max(20.0),
                            0.75,
                            0.35,
                            text(0.08),
                        );

                        // Row 6: [CoverExpand] 返回主窗口
                        let rf = fb(MoreHit::ReturnMain);
                        chrome::control_surface_alpha_radius(
                            window, ret_row, rf, false, cont_a, 13.5,
                        );
                        chrome::chevron(
                            window,
                            ret_row.right() - 16.0,
                            ret_row.y + (ret_row.h - 8.0) * 0.5,
                            text(0.36 + 0.30 * rf.hover),
                        );
                        icons::paint(
                            window,
                            cx,
                            Icon::CoverExpand,
                            Box4::new(
                                cx0 + 14.0,
                                ret_row.y + (ret_row.h - 16.0) * 0.5,
                                16.0,
                                16.0,
                            ),
                            text(0.55 + 0.25 * rf.hover),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            "返回主窗",
                            cx0 + 38.0,
                            ret_row.centre().1,
                            12.0,
                            500.0,
                            text(0.88 + 0.08 * rf.hover),
                        );

                        chrome::set_light_theme(false);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorView {
    Presets,
    CustomPalette,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ColorHit {
    Preset(usize),
    CustomOpen,
    PaletteBack,
    PaletteReset,
    PaletteDone,
}

/// Independent right-side secondary popover for Desktop Lyrics text color presets + GPU HSV custom palette.
struct DesktopColorPopover {
    bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    view: ColorView,
    hsv: (f32, f32, f32),
    palette_drag: Option<PaletteDrag>,
    sv_rect: Box4,
    hue_rect: Box4,
    feedback: HashMap<ColorHit, Feedback>,
    elastics: HashMap<ColorHit, Elastic>,
    panel_elastic: Elastic,
    press_gesture: PressGesture,
    pressed_hit: Option<ColorHit>,
    panel_dragging: bool,
    card_rect: Box4,
    hits: Vec<(ColorHit, Box4)>,
    cursor: Option<(f32, f32)>,
    was_active: bool,
    configured: bool,
    started: Instant,
    last_ms: f64,
}

impl DesktopColorPopover {
    fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
        let cfg = bridge.read_cfg();
        Self {
            bridge,
            motion: PanelMotion::default(),
            view: ColorView::Presets,
            hsv: rgb_to_hsv(cfg.text_color),
            palette_drag: None,
            sv_rect: Box4::default(),
            hue_rect: Box4::default(),
            feedback: HashMap::new(),
            elastics: HashMap::new(),
            panel_elastic: Elastic::default(),
            press_gesture: PressGesture::default(),
            pressed_hit: None,
            panel_dragging: false,
            card_rect: Box4::default(),
            hits: Vec::new(),
            cursor: None,
            was_active: false,
            configured: false,
            started: Instant::now(),
            last_ms: 0.0,
        }
    }

    fn apply_palette_pointer(&mut self, drag: PaletteDrag, x: f32, y: f32) {
        match drag {
            PaletteDrag::Sv => {
                let s = ((x - self.sv_rect.x) / self.sv_rect.w.max(1.0)).clamp(0.0, 1.0);
                let v = (1.0 - (y - self.sv_rect.y) / self.sv_rect.h.max(1.0)).clamp(0.0, 1.0);
                self.hsv.1 = s;
                self.hsv.2 = v;
            }
            PaletteDrag::Hue => {
                let h = ((x - self.hue_rect.x) / self.hue_rect.w.max(1.0)).clamp(0.0, 1.0) * 360.0;
                self.hsv.0 = h;
            }
        }
        let rgb = hsv_to_rgb(self.hsv.0, self.hsv.1, self.hsv.2);
        self.bridge.push_popover(PopoverAction::SetColor(rgb));
    }
}

impl Render for DesktopColorPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if !self.configured {
                self.configured = true;
                window.defer(cx, move |_, _| {
                    win32_window::configure_desktop_overlay(hwnd);
                });
            }
        }
        if window.is_window_active() {
            self.was_active = true;
        } else if self.was_active && self.palette_drag.is_none() && !self.panel_dragging {
            self.bridge.mark_color_closed();
            window.remove_window();
            return div().into_any_element();
        }
        if !win32_window::is_left_mouse_down() {
            if self.palette_drag.is_some() {
                self.palette_drag = None;
            }
            if self.pressed_hit.is_some() || self.panel_dragging {
                self.pressed_hit = None;
                self.panel_dragging = false;
                self.press_gesture.cancel();
            }
        } else if self.palette_drag.is_some() || self.panel_dragging || self.pressed_hit.is_some() {
            if let Some((cx_p, cy_p)) = win32_window::cursor_screen_pos() {
                let sf = window.scale_factor().max(0.5);
                let wb = window.bounds();
                let lx = (cx_p as f32 / sf) - f32::from(wb.origin.x);
                let ly = (cy_p as f32 / sf) - f32::from(wb.origin.y);
                self.cursor = Some((lx, ly));
                if let Some(drag) = self.palette_drag {
                    self.apply_palette_pointer(drag, lx, ly);
                } else {
                    self.press_gesture.update(lx, ly);
                }
            }
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;
        self.motion.step(true, dt);

        let cfg = self.bridge.read_cfg();
        let snap = self.bridge.read_snapshot();
        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let win_h = f32::from(vp.height).max(1.0);

        let raw_delta = self.press_gesture.delta();
        let panel_held = self.panel_dragging
            || (self.pressed_hit.is_some() && self.press_gesture.dragged());
        self.panel_elastic.step(raw_delta, panel_held, dt);
        let pp = self.panel_elastic.pull();

        let card_w = (win_w - POPOVER_ELASTIC_PAD * 2.0).max(120.0);
        let card_h = (win_h - POPOVER_ELASTIC_PAD * 2.0).max(120.0);
        let cx0 = POPOVER_ELASTIC_PAD + pp[0];
        let cy0 = POPOVER_ELASTIC_PAD + pp[1];
        let card_rect = Box4::new(cx0, cy0, card_w, card_h);
        self.card_rect = card_rect;

        let pad = 12.0_f32;
        let inner_w = (card_w - pad * 2.0).max(80.0);
        let item_h = 32.0_f32;
        let list_top = cy0 + 8.0;

        let pal_back = Box4::new(cx0 + 10.0, cy0 + 8.0, 54.0, 26.0);
        let sv_rect = Box4::new(cx0 + pad, cy0 + 38.0, inner_w, 82.0);
        let hue_rect = Box4::new(cx0 + pad, cy0 + 126.0, inner_w, 14.0);
        let preview_rect = Box4::new(cx0 + pad, cy0 + 146.0, inner_w, 24.0);
        let pal_reset = Box4::new(cx0 + pad, cy0 + 178.0, (inner_w - 8.0) * 0.5, 26.0);
        let pal_done = Box4::new(
            cx0 + pad + (inner_w + 8.0) * 0.5,
            cy0 + 178.0,
            (inner_w - 8.0) * 0.5,
            26.0,
        );
        self.sv_rect = sv_rect;
        self.hue_rect = hue_rect;

        let mut hits = Vec::new();
        match self.view {
            ColorView::Presets => {
                for i in 0..COLOR_PRESETS.len() {
                    let r = Box4::new(cx0 + 6.0, list_top + i as f32 * item_h, card_w - 12.0, item_h);
                    hits.push((ColorHit::Preset(i), r));
                }
                let custom_y = list_top + COLOR_PRESETS.len() as f32 * item_h + 6.0;
                hits.push((
                    ColorHit::CustomOpen,
                    Box4::new(cx0 + 6.0, custom_y, card_w - 12.0, item_h),
                ));
            }
            ColorView::CustomPalette => {
                hits.extend_from_slice(&[
                    (ColorHit::PaletteBack, pal_back),
                    (ColorHit::PaletteReset, pal_reset),
                    (ColorHit::PaletteDone, pal_done),
                ]);
            }
        }
        self.hits = hits.clone();

        for (hit, rect) in &hits {
            let hot = self.cursor.is_some_and(|(mx, my)| rect.contains(mx, my));
            let held = self.pressed_hit == Some(*hit);
            let fb = self.feedback.entry(*hit).or_default();
            fb.step(hot, held, dt);
            let el = self.elastics.entry(*hit).or_default();
            el.step(raw_delta, held, dt);
            fb.pull = el.pull();
            fb.squeeze = el.squeeze();
            fb.press = el.pressure().clamp(0.0, 1.0);
            if let Some((mx, my)) = self.cursor {
                fb.focus = [
                    ((mx - rect.x) / rect.w.max(1.0) * 2.0 - 1.0).clamp(-1.5, 1.5),
                    ((my - rect.y) / rect.h.max(1.0) * 2.0 - 1.0).clamp(-1.5, 1.5),
                ];
            }
        }

        let fb_map = self.feedback.clone();
        let view_mode = self.view;
        let hsv = self.hsv;
        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let panel_radius = snap.display.corner_radius.clamp(12.0, 22.0);
        let accent_hex = snap.display.accent_hex;

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if let Some(drag) = this.palette_drag {
                    this.apply_palette_pointer(drag, x, y);
                } else if this.pressed_hit.is_some() || this.panel_dragging {
                    this.press_gesture.update(x, y);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if this.view == ColorView::CustomPalette {
                        if this.sv_rect.contains(x, y) {
                            this.palette_drag = Some(PaletteDrag::Sv);
                            this.apply_palette_pointer(PaletteDrag::Sv, x, y);
                            return;
                        }
                        if this.hue_rect.contains(x, y) {
                            this.palette_drag = Some(PaletteDrag::Hue);
                            this.apply_palette_pointer(PaletteDrag::Hue, x, y);
                            return;
                        }
                    }
                    let hit = this.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h);
                    this.press_gesture.begin(x, y);
                    if let Some(h) = hit {
                        this.pressed_hit = Some(h);
                        this.panel_dragging = false;
                    } else if this.card_rect.contains(x, y) {
                        this.pressed_hit = None;
                        this.panel_dragging = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if this.palette_drag.take().is_some() {
                        return;
                    }
                    this.press_gesture.update(x, y);
                    this.panel_dragging = false;
                    let pressed = this.pressed_hit.take();
                    let cur_hit = this.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h);
                    let same = pressed.is_some() && cur_hit == pressed;
                    if !this.press_gesture.release(same) {
                        return;
                    }
                    let Some(hit) = pressed else {
                        return;
                    };
                    match hit {
                        ColorHit::Preset(i) => {
                            if let Some(&(rgb_v, _)) = COLOR_PRESETS.get(i) {
                                this.hsv = rgb_to_hsv(rgb_v);
                                this.bridge.push_popover(PopoverAction::SetColor(rgb_v));
                            }
                        }
                        ColorHit::CustomOpen => {
                            this.hsv = rgb_to_hsv(this.bridge.read_cfg().text_color);
                            this.view = ColorView::CustomPalette;
                        }
                        ColorHit::PaletteBack => {
                            this.view = ColorView::Presets;
                        }
                        ColorHit::PaletteReset => {
                            let def = COLOR_PRESETS[0].0;
                            this.hsv = rgb_to_hsv(def);
                            this.bridge.push_popover(PopoverAction::SetColor(def));
                        }
                        ColorHit::PaletteDone => {
                            this.bridge.mark_color_closed();
                            this.bridge.push_popover(PopoverAction::CloseColorPopover);
                            window.remove_window();
                        }
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        chrome::set_light_theme(light_panel);
                        let fb = |h: ColorHit| fb_map.get(&h).copied().unwrap_or_default();
                        let text = |a: f32| chrome::text(a * cont_a);

                        paint_sidecar_glass_frame(
                            window,
                            card_rect,
                            panel_radius,
                            theme_mode,
                            surf_a,
                        );

                        if view_mode == ColorView::CustomPalette {
                            let back_fb = fb(ColorHit::PaletteBack);
                            chrome::control_surface_alpha_radius(
                                window, pal_back, back_fb, false, cont_a, 8.0,
                            );
                            chrome::paint_text_box_centred(
                                window,
                                "‹ 返回",
                                pal_back,
                                12.0,
                                600.0,
                                text(0.84 + 0.14 * back_fb.hover),
                            );
                            chrome::paint_text_box_centred(
                                window,
                                "自定义颜色",
                                Box4::new(cx0 + pad + 44.0, cy0 + 8.0, inner_w - 88.0, 26.0),
                                12.5,
                                650.0,
                                text(0.95),
                            );

                            paint_hsv_palette(window, sv_rect, hue_rect, hsv, cont_a);

                            let cur_rgb = hsv_to_rgb(hsv.0, hsv.1, hsv.2);
                            chrome::fill_round(
                                window,
                                preview_rect.x,
                                preview_rect.y,
                                preview_rect.w,
                                preview_rect.h,
                                8.0,
                                chrome::text(0.06 * cont_a),
                            );
                            chrome::stroke_round(
                                window,
                                preview_rect.x,
                                preview_rect.y,
                                preview_rect.w,
                                preview_rect.h,
                                8.0,
                                0.85,
                                chrome::themed_border(0.16 * cont_a),
                            );
                            chrome::fill_round(
                                window,
                                preview_rect.x + 8.0,
                                preview_rect.centre().1 - 7.5,
                                15.0,
                                15.0,
                                7.5,
                                Hsla::from(rgb(cur_rgb)).opacity(cont_a),
                            );
                            chrome::stroke_round(
                                window,
                                preview_rect.x + 8.0,
                                preview_rect.centre().1 - 7.5,
                                15.0,
                                15.0,
                                7.5,
                                0.9,
                                chrome::themed_border(0.28 * cont_a),
                            );
                            chrome::paint_text_vcentred(
                                window,
                                &format!("#{:06X}", cur_rgb),
                                preview_rect.x + 30.0,
                                preview_rect.centre().1,
                                12.0,
                                650.0,
                                text(0.92),
                            );
                            chrome::paint_text_vcentred(
                                window,
                                "实时生效",
                                preview_rect.right() - 56.0,
                                preview_rect.centre().1,
                                11.0,
                                500.0,
                                text(0.52),
                            );

                            chrome::paint_glass_button(
                                window,
                                pal_reset,
                                "恢复纯白",
                                accent_hex,
                                false,
                                false,
                                fb(ColorHit::PaletteReset),
                                cont_a,
                            );
                            chrome::paint_glass_button(
                                window,
                                pal_done,
                                "完成",
                                accent_hex,
                                true,
                                false,
                                fb(ColorHit::PaletteDone),
                                cont_a,
                            );
                            chrome::set_light_theme(false);
                            return;
                        }

                        let mut matched_preset = false;
                        for (i, &(rgb_v, label)) in COLOR_PRESETS.iter().enumerate() {
                            let r = Box4::new(
                                cx0 + 6.0,
                                list_top + i as f32 * item_h,
                                card_w - 12.0,
                                item_h,
                            );
                            let selected = cfg.text_color == rgb_v;
                            matched_preset |= selected;
                            let f = fb(ColorHit::Preset(i));
                            chrome::control_surface_alpha_radius(
                                window, r, f, selected, cont_a, 10.0,
                            );
                            chrome::paint_list_item(
                                window,
                                r,
                                label,
                                selected,
                                Some(rgb_v),
                                accent_hex,
                                f,
                                cont_a,
                            );
                        }
                        let div_y = list_top + COLOR_PRESETS.len() as f32 * item_h + 2.5;
                        chrome::fill_round(
                            window,
                            cx0 + 12.0,
                            div_y,
                            (card_w - 24.0).max(20.0),
                            0.75,
                            0.35,
                            text(0.10),
                        );
                        let custom_r = Box4::new(cx0 + 6.0, div_y + 3.5, card_w - 12.0, item_h);
                        let cf = fb(ColorHit::CustomOpen);
                        chrome::control_surface_alpha_radius(
                            window,
                            custom_r,
                            cf,
                            !matched_preset,
                            cont_a,
                            10.0,
                        );
                        if !matched_preset {
                            chrome::checkmark(
                                window,
                                custom_r.x + 12.0,
                                custom_r.y + (custom_r.h - 8.0) * 0.5,
                                text(0.94),
                            );
                        }
                        let dot_x = custom_r.x + 38.0;
                        let dot_y = custom_r.y + (custom_r.h - 10.0) * 0.5;
                        window.paint_quad(quad(
                            bounds(point(px(dot_x), px(dot_y)), size(px(10.0), px(10.0))),
                            Corners::all(px(5.0)),
                            linear_gradient(
                                135.0,
                                linear_color_stop(Hsla::from(rgb(0xFF5E94)).opacity(cont_a), 0.0),
                                linear_color_stop(Hsla::from(rgb(0x4FC3FF)).opacity(cont_a), 1.0),
                            ),
                            Edges::all(px(0.6)),
                            chrome::themed_border(0.30 * cont_a),
                            BorderStyle::Solid,
                        ));
                        chrome::paint_text_vcentred(
                            window,
                            "自定义调色板…",
                            dot_x + 18.0,
                            custom_r.centre().1,
                            12.0,
                            500.0,
                            text(0.88 + 0.08 * cf.hover),
                        );
                        chrome::chevron(
                            window,
                            custom_r.right() - 14.0,
                            custom_r.centre().1 - 4.0,
                            text(0.45 + 0.30 * cf.hover),
                        );

                        chrome::set_light_theme(false);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

fn snap_axis(
    raw: f32,
    win_len: f32,
    disp_origin: f32,
    disp_len: f32,
    was_snapped: bool,
) -> (f32, bool) {
    let center_pos = disp_origin + (disp_len - win_len) * 0.5;
    let threshold = if was_snapped { 18.0 } else { 12.0 };
    if (raw - center_pos).abs() <= threshold {
        (center_pos, true)
    } else {
        let min_v = disp_origin - win_len + 64.0;
        let max_v = disp_origin + disp_len - 64.0;
        (raw.clamp(min_v, max_v), false)
    }
}

fn snap_and_clamp_drag(
    raw_x: f32,
    raw_y: f32,
    win_w: f32,
    win_h: f32,
    was_sx: bool,
    was_sy: bool,
    window: &Window,
    cx: &App,
) -> (f32, f32, bool, bool) {
    let Some(d) = window.display(cx) else {
        return (raw_x, raw_y, false, false);
    };
    let b = d.bounds();
    let (nx, sx) = snap_axis(
        raw_x,
        win_w,
        f32::from(b.origin.x),
        f32::from(b.size.width),
        was_sx,
    );
    let (ny, sy) = snap_axis(
        raw_y,
        win_h,
        f32::from(b.origin.y),
        f32::from(b.size.height),
        was_sy,
    );
    (nx, ny, sx, sy)
}

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
