//! Pure geometry and lighting for the player view.
//!
//! Everything here is a function of the window size, the display settings and
//! the playhead, so it can be tested without a GPU or an App. The view itself
//! only converts these numbers into paint calls.
//!
//! The layout constants are not invented: they are the ones the frozen
//! `reference/ui-results.json` dump recorded for the three viewports, and the
//! three predictions below reproduce its region table to the hundredth of a
//! pixel at 1920x1080, 1272x843 and 800x500.

use oly_contracts::AmllLine;
use oly_render::emphasis::GLOW_ALPHA_SCALE;

/// The `fontSize` the shipped profile is calibrated against.
pub const BASE_FONT: f32 = 100.0;

/// Display settings that change geometry or opacity.
///
/// Resolved from the base display section and active cover/nocover profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub font_family: &'static str,
    pub trans_font_family: &'static str,
    pub enable_spring: bool,
    pub enable_scale: bool,
    pub align_right: bool,
    pub hide_passed_lines: bool,
    pub spring_feel: SpringFeel,
    pub line_width_aspect: f32,
    pub line_padding_x: Option<f32>,
    pub line_radius: f32,
    pub hover_bg_alpha: f32,
    pub font_size: f64,
    pub trans_font_size: f64,
    pub weight: f32,
    pub trans_weight: f32,
    pub letter_spacing_em: f32,
    pub line_height_em: f32,
    pub show_translation: bool,
    pub show_romanization: bool,
    pub roman_font_scale: f32,
    pub phonetic_opacity: f32,
    pub blur_max: bool,
    pub blur_strength: f32,
    pub balanced: bool,
    pub unplayed: f32,
    pub idle: f32,
    pub trans_opacity: f32,
    pub trans_current: f32,
    pub bg_opacity: f32,
    pub bg_scale: f32,
    pub dot_scale: f32,
    pub edge_feather: f32,
    pub top_offset: f32,
    pub word_fade: f32,
    pub long_syllable: i64,
    pub line_gap: f32,
    pub inner_gap: f32,
    pub bright: f32,
    pub show_cover: bool,
    pub motion_cover: bool,
    pub immersion_background: bool,
    pub immersion_compact: bool,
    pub beat_follow: bool,
    pub beat_flash_threshold: f32,
    pub beat_flash_strength: f32,
    pub beat_wobble_threshold: f32,
    pub beat_wobble_strength: f32,
    pub motion_cache_mb: u64,
    pub show_progress: bool,
    pub theme_mode: ThemeMode,
    pub accent_auto: bool,
    pub accent_hex: u32,
    pub cover_radius: f32,
    pub corner_radius: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeMode {
    LiquidGlass,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpringFeel { Soft, Standard, Lively }

impl Default for Display {
    fn default() -> Self {
        Self::from_config(&serde_json::from_str(include_str!("../../../oly-core/src/config_defaults.json")).expect("bundled config"))
    }
}

impl Display {
    /// Resolve the current window profile, keeping shared visibility controls in base.
    pub fn effective_json(config: &serde_json::Value) -> serde_json::Value {
        Self::effective_json_for(config, None)
    }

    /// Resolve a display config object using an explicit profile key (`cover` / `nocover` / `mini`)
    /// when supplied, or inferring `cover` / `nocover` from `showCover` otherwise.
    pub fn effective_json_for(
        config: &serde_json::Value,
        profile_override: Option<&str>,
    ) -> serde_json::Value {
        let base = config.get("display").unwrap_or(config);
        let mut result = base.as_object().cloned().unwrap_or_default();
        let cover = base.get("showCover").and_then(serde_json::Value::as_bool).unwrap_or(true);
        let profile = profile_override.unwrap_or(if cover { "cover" } else { "nocover" });
        let store = base.get("typeProfiles").and_then(|p| p.get(profile));
        if let Some(values) = store.and_then(serde_json::Value::as_object) {
            result.extend(values.clone());
        }
        // This particular value deliberately ignores the base section, like the drawer.
        let top = store.and_then(|s| s.get("playingLineTopOffset")).and_then(serde_json::Value::as_f64)
            .filter(|n| n.is_finite()).unwrap_or(match profile {
                "cover" => 42.0,
                _ => 50.0,
            });
        result.insert("playingLineTopOffset".into(), serde_json::json!(top));
        result.insert("showCover".into(), serde_json::json!(cover));
        serde_json::Value::Object(result)
    }

    /// Parse `Display` for an explicit profile key (`cover` / `nocover` / `mini`), keeping
    /// that profile's `typeProfiles[profile]` values separate from other modes.
    pub fn from_config_for(config: &serde_json::Value, profile: &str) -> Self {
        let mut resolved = Self::effective_json_for(config, Some(profile));
        if let Some(obj) = resolved.as_object_mut() {
            let cover = obj.get("showCover").and_then(serde_json::Value::as_bool).unwrap_or(true);
            let target_key = if cover { "cover" } else { "nocover" };
            let top = obj
                .get("playingLineTopOffset")
                .cloned()
                .unwrap_or_else(|| serde_json::json!(if profile == "cover" { 42.0 } else { 50.0 }));
            obj.insert(
                "typeProfiles".into(),
                serde_json::json!({ target_key: { "playingLineTopOffset": top } }),
            );
        }
        let mut scoped = config.clone();
        if scoped.get("display").is_some() {
            if let Some(root) = scoped.as_object_mut() {
                root.insert("display".into(), resolved);
            }
        } else {
            scoped = resolved;
        }
        Self::from_config(&scoped)
    }

    pub fn from_config(config: &serde_json::Value) -> Self {
        let v = Self::effective_json(config);
        let num = |key: &str, fallback: f64| v.get(key).and_then(serde_json::Value::as_f64).filter(|x| x.is_finite()).unwrap_or(fallback);
        let boolean = |key: &str, fallback| v.get(key).and_then(serde_json::Value::as_bool).unwrap_or(fallback);
        let string = |key: &str| v.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
        let pct = |key: &str, fallback| (num(key, fallback) / 100.0).clamp(0.0, 1.0) as f32;
        let font_family = font_family(string("fontFamily"));
        let trans_family = string("transFontFamily");
        Self {
            font_family,
            trans_font_family: if trans_family.is_empty() { font_family } else { self::font_family(trans_family) },
            enable_spring: boolean("enableSpring", true),
            enable_scale: boolean("enableScale", true),
            align_right: string("align") == "right",
            hide_passed_lines: boolean("hidePassedLines", false),
            spring_feel: match string("springFeel") { "soft" | "calm" => SpringFeel::Soft, "lively" | "strong" => SpringFeel::Lively, _ => SpringFeel::Standard },
            line_width_aspect: num("lineWidthAspect", 1.0).clamp(0.3, 1.0) as f32,
            line_padding_x: v.get("linePaddingX").and_then(serde_json::Value::as_f64).filter(|n| n.is_finite()).map(|n| n.clamp(0.0, 4.0) as f32),
            line_radius: num("lineRadius", 0.25).max(0.0) as f32,
            hover_bg_alpha: pct("hoverBgAlpha", 7.0),
            font_size: num("fontSize", 100.0).clamp(10.0, 400.0),
            trans_font_size: num("transFontSize", 100.0).clamp(10.0, 400.0),
            weight: num("fontWeight", 800.0).round() as f32,
            trans_weight: num("transFontWeight", 500.0).clamp(100.0, 900.0).round() as f32,
            letter_spacing_em: num("letterSpacing", 0.005) as f32,
            line_height_em: 1.2,
            show_translation: boolean("showTranslation", true),
            show_romanization: boolean("showRomanization", false),
            roman_font_scale: (num("romanFontScale", 50.0).clamp(20.0, 150.0) / 100.0) as f32,
            phonetic_opacity: pct("phoneticLyricsOpacity", 30.0),
            blur_max: boolean("blurMax", false),
            blur_strength: num("blurStrength", 150.0).clamp(50.0, 400.0) as f32,
            balanced: string("lineBreakMode") == "balanced",
            unplayed: pct("unplayedOriginalOpacity", 60.0),
            idle: pct("idleLineOpacity", 20.0),
            trans_opacity: pct("translatedLyricsOpacity", 20.0),
            trans_current: pct("transCurrentAlpha", 85.0),
            bg_opacity: pct("bgLineOpacity", 40.0),
            bg_scale: (num("bgLineScale", 70.0).clamp(30.0, 150.0) / 100.0) as f32,
            dot_scale: (num("dotScale", 100.0).clamp(30.0, 250.0) / 100.0) as f32,
            edge_feather: num("edgeFeather", 108.0).max(0.0) as f32,
            top_offset: num("playingLineTopOffset", 42.0) as f32,
            word_fade: num("wordFadeWidth", 0.5).clamp(0.0, 2.0) as f32,
            long_syllable: num("longSyllableThreshold", 1000.0) as i64,
            line_gap: num("lineGap", 0.0).clamp(-0.6, 1.5) as f32,
            inner_gap: num("innerGap", 0.0).max(0.0) as f32,
            bright: pct("brightAlpha", 100.0),
            show_cover: boolean("showCover", true),
            motion_cover: boolean("motionCover", true),
            immersion_background: string("windowBackground") != "glass",
            immersion_compact: boolean("immersionCompact", false),
            beat_follow: boolean("beatFollow", true),
            beat_flash_threshold: num("beatFlashThreshold", 1.04).clamp(1.01, 2.0) as f32,
            beat_flash_strength: num("beatFlashStrength", 1.20).clamp(0.0, 2.50) as f32,
            beat_wobble_threshold: num("beatWobbleThreshold", 1.10).clamp(1.02, 2.5) as f32,
            beat_wobble_strength: num("beatWobbleStrength", 1.20).clamp(0.0, 2.50) as f32,
            motion_cache_mb: (num("motionCoverCacheMB", 500.0).clamp(50.0, 4096.0).round()) as u64,
            show_progress: boolean("showProgress", true),
            theme_mode: match string("themeMode").trim().to_ascii_lowercase().as_str() {
                "light" => ThemeMode::Light,
                "dark" => ThemeMode::Dark,
                _ => ThemeMode::LiquidGlass,
            },
            accent_auto: {
                let s = string("uiAccentColor").trim();
                s.is_empty() || s.eq_ignore_ascii_case("auto")
            },
            accent_hex: parse_accent_hex(string("uiAccentColor")),
            cover_radius: num("coverRadius", 20.0).clamp(0.0, 48.0) as f32,
            corner_radius: num("cornerRadius", 20.0).clamp(0.0, 36.0) as f32,
        }
    }

    pub fn spring_params(&self) -> oly_core::scroll::SpringParams {
        let (mass, damping, stiffness) = match self.spring_feel {
            SpringFeel::Soft => (0.99, 21.75, 55.8),
            SpringFeel::Standard => (0.9, 15.0, 90.0),
            SpringFeel::Lively => (0.9, 10.8, 135.0),
        };
        oly_core::scroll::SpringParams { mass, damping, stiffness, soft: false }
    }
}

fn parse_accent_hex(value: &str) -> u32 {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => 0x0A84_FF,
        "#0a84ff" | "blue" => 0x0A84_FF,
        "#ff9f0a" | "orange" => 0xFF9F_0A,
        s => s
            .strip_prefix('#')
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .unwrap_or(0x0A84_FF),
    }
}

/// Extract the dominant legible accent colour (`0xRRGGBB`) from an RGBA cover image.
pub fn extract_cover_accent(rgba: &[u8], width: u32, height: u32) -> Option<u32> {
    if width == 0 || height == 0 || rgba.len() < (width as usize * height as usize * 4) {
        return None;
    }
    const BINS: usize = 24;
    let mut bin_weight = [0.0f32; BINS];
    let mut bin_r = [0.0f32; BINS];
    let mut bin_g = [0.0f32; BINS];
    let mut bin_b = [0.0f32; BINS];
    let mut avg_r = 0.0f32;
    let mut avg_g = 0.0f32;
    let mut avg_b = 0.0f32;
    let mut avg_count = 0.0f32;

    let step_x = (width / 24).max(1);
    let step_y = (height / 24).max(1);
    for y in (0..height).step_by(step_y as usize) {
        for x in (0..width).step_by(step_x as usize) {
            let idx = ((y * width + x) * 4) as usize;
            let r = rgba[idx] as f32 / 255.0;
            let g = rgba[idx + 1] as f32 / 255.0;
            let b = rgba[idx + 2] as f32 / 255.0;
            avg_r += r;
            avg_g += g;
            avg_b += b;
            avg_count += 1.0;

            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            let l = (max + min) * 0.5;
            let d = max - min;
            if d < 0.06 || l < 0.08 || l > 0.92 {
                continue;
            }
            let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-4);
            let mut h = if (max - r).abs() < 1e-5 {
                (g - b) / d + if g < b { 6.0 } else { 0.0 }
            } else if (max - g).abs() < 1e-5 {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            };
            h = (h / 6.0).rem_euclid(1.0);
            let bin = ((h * BINS as f32).floor() as usize).min(BINS - 1);
            let l_weight = (1.0 - (l - 0.50).abs() * 1.35).max(0.15);
            let w = (0.35 + 0.65 * s) * l_weight;
            bin_weight[bin] += w;
            bin_r[bin] += r * w;
            bin_g[bin] += g * w;
            bin_b[bin] += b * w;
        }
    }
    let (r, g, b) = if let Some((best, &w)) = bin_weight.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))
        && w > 0.0
    {
        (bin_r[best] / w, bin_g[best] / w, bin_b[best] / w)
    } else if avg_count > 0.0 {
        (avg_r / avg_count, avg_g / avg_count, avg_b / avg_count)
    } else {
        return None;
    };

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let mut l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-4 {
        let v = (l.clamp(0.55, 0.75) * 255.0).round() as u32;
        return Some((v << 16) | (v << 8) | v);
    }
    let mut s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-4);
    let mut h = if (max - r).abs() < 1e-5 {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < 1e-5 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h = (h / 6.0).rem_euclid(1.0);
    s = s.clamp(0.45, 0.85);
    l = l.clamp(0.50, 0.68);
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue2rgb = |mut t: f32| -> u32 {
        if t < 0.0 { t += 1.0; }
        if t > 1.0 { t -= 1.0; }
        let c = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (c.clamp(0.0, 1.0) * 255.0).round() as u32
    };
    let rr = hue2rgb(h + 1.0 / 3.0);
    let gg = hue2rgb(h);
    let bb = hue2rgb(h - 1.0 / 3.0);
    Some((rr << 16) | (gg << 8) | bb)
}

/// Supported settings families stay static so a display snapshot remains Copy.
fn font_family(value: &str) -> &'static str {
    match value.trim() {
        "檎风黑体 CHS" | "檎風黑體 CHS" | "Qyn Flavor Grotesk CHS" => "檎风黑体 CHS",
        "微软雅黑" => "微软雅黑", "宋体" => "宋体", "黑体" => "黑体", "楷体" => "楷体",
        "仿宋" => "仿宋", "等线" => "等线", "Consolas" => "Consolas", _ => "Pretendard",
    }
}

pub use oly_ui::geometry::Box4;

/// `wideArtworkSize` from `src/windows/lyrics/wide-artwork.ts`.
/// Blink lays out in 1/64 px `LayoutUnit`s, and the frozen region dump was
/// taken from the real browser: `5vw` at 1272 px is 63.59375, not 63.6. Any
/// CSS-derived length in this module goes through the same snap so the
/// predicted boxes land on the recorded numbers instead of 0.01 px away.
pub fn snap_layout_unit(value: f32) -> f32 {
    (value * 64.0).round() / 64.0
}

pub fn window_ui_scale(w: f32, h: f32, screen_w: f32, screen_h: f32, fullscreen: bool) -> f32 {
    if fullscreen {
        1.0
    } else {
        (w / screen_w.max(1.0))
            .min(h / screen_h.max(1.0))
            .clamp(0.25, 1.0)
    }
}

pub fn wide_artwork_size(w: f32, h: f32, screen_w: f32, screen_h: f32, fullscreen: bool) -> f32 {
    if fullscreen {
        (600.0_f32)
            .min(w / 2.0 - 56.0)
            .min(h - 216.0)
            .max(0.0)
    } else {
        let w_scale = (w / screen_w.max(1.0)).clamp(0.25, 1.0);
        let h_scale = (h / screen_h.max(1.0)).clamp(0.25, 1.0);
        let screen_col = (600.0_f32)
            .min(screen_w / 2.0 - 56.0)
            .min(screen_h - 216.0)
            .max(0.0);
        let wide_left = (snap_layout_unit((0.05 * screen_w).clamp(24.0, 96.0)) + 24.0) * w_scale;
        let gap = snap_layout_unit((0.06 * screen_w).clamp(32.0, 104.0)) * w_scale;
        let lyrics_right =
            (snap_layout_unit((0.05 * screen_w).clamp(24.0, 96.0)) - 24.0 - 18.0).max(16.0) * w_scale;
        let avail_w = (w - wide_left - gap - 18.0 * w_scale - lyrics_right).max(1.0);
        (screen_col * h_scale)
            .min(avail_w * 0.41)
            .min(h - 216.0 * h_scale)
            .max(0.0)
    }
}

/// The current adapter applies fontSize as a percentage of AMLL's viewport size.
pub fn font_px(w: f32, h: f32, font_size: f64) -> f32 {
    let chain = (0.05 * h).max(0.025 * w).max(12.0);
    chain * (font_size / BASE_FONT as f64) as f32
}

/// Every pixel measurement the view needs for one window size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub win_w: f32,
    pub win_h: f32,
    pub font: f32,
    pub line_height: f32,
    pub trans_font: f32,
    pub trans_height: f32,
    pub lyrics: Box4,
    pub lyric_viewport: Box4,
    pub cover: Box4,
    /// `--wide-column`: the artwork column, and the width the transport and
    /// the progress bar use. It does **not** collapse when the cover is
    /// hidden — only `art_height` does.
    pub column: f32,
    /// `--wide-art-height`: `--wide-column` while the cover is shown, `0` when
    /// `showCover` is off.
    pub art_height: f32,
    /// Top of the `.cover-meta` line box (title ink starts here).
    pub cover_meta_top: f32,
    pub feather_px: f32,
    pub anchor_px: f32,
    pub wide_left: f32,
    pub toolbar: Box4,
    pub progress: Box4,
    pub ui_scale: f32,
}

/// The shipped window-mode geometry, derived from the CSS custom properties:
/// `--wide-left`, `--wide-gap`, `--wide-column`, `--wide-top`.
///
/// `showCover` only zeroes `--wide-art-height`; `--wide-column` and
/// `--wide-top` keep their values, so the lyric column, the progress bar and
/// the transport all stay where they are and only the artwork slot collapses.
pub fn metrics(w: f32, h: f32, screen: (f32, f32), fullscreen: bool, d: &Display) -> Metrics {
    let (wide_left, gap, shift_x, lyrics_right, column, h_scale, ui_scale) = if fullscreen {
        let wide_left = snap_layout_unit((0.05 * w).clamp(24.0, 96.0)) + 24.0;
        let gap = snap_layout_unit((0.06 * w).clamp(32.0, 104.0));
        let lyrics_right = (wide_left - 2.0 * 24.0 - 18.0).max(16.0);
        let column = wide_artwork_size(w, h, screen.0, screen.1, true);
        (wide_left, gap, 18.0, lyrics_right, column, 1.0, 1.0)
    } else {
        let w_scale = (w / screen.0.max(1.0)).clamp(0.25, 1.0);
        let h_scale = (h / screen.1.max(1.0)).clamp(0.25, 1.0);
        let wide_left_fs = snap_layout_unit((0.05 * screen.0).clamp(24.0, 96.0)) + 24.0;
        let gap_fs = snap_layout_unit((0.06 * screen.0).clamp(32.0, 104.0));
        let lyrics_right_fs = (wide_left_fs - 2.0 * 24.0 - 18.0).max(16.0);
        let screen_col = (600.0_f32)
            .min(screen.0 / 2.0 - 56.0)
            .min(screen.1 - 216.0)
            .max(0.0);
        let wide_left = wide_left_fs * w_scale;
        let gap = gap_fs * w_scale;
        let shift_x = 18.0 * w_scale;
        let lyrics_right = lyrics_right_fs * w_scale;
        let avail_w = (w - wide_left - gap - shift_x - lyrics_right).max(1.0);
        let column = (screen_col * h_scale)
            .min(avail_w * 0.41)
            .min(h - 216.0 * h_scale)
            .max(0.0);
        let ui_scale = if screen_col > 1.0 {
            (column / screen_col).clamp(0.25, 1.0)
        } else {
            h_scale
        };
        (wide_left, gap, shift_x, lyrics_right, column, h_scale, ui_scale)
    };
    let art_height = if d.show_cover { column } else { 0.0 };
    let lyrics_left = wide_left + column + gap + shift_x;
    let top = h * 0.08;
    let bottom = h * 0.08;
    let lyrics = if d.show_cover {
        Box4::new(
            lyrics_left,
            top,
            (w - lyrics_left - lyrics_right).max(1.0),
            (h - top - bottom).max(1.0),
        )
    } else {
        let reading_w = (w * 0.80).clamp(360.0, 1080.0).min((w - 64.0).max(1.0));
        let centered_x = ((w - reading_w) * 0.5).max(32.0);
        Box4::new(centered_x, top, reading_w, (h - top - bottom).max(1.0))
    };
    let wide_top = ((h - column - 210.0 * ui_scale) / 2.0).max(20.0 * ui_scale);
    let cover_top = if d.show_cover { wide_top } else { wide_top + 20.0 * ui_scale };
    let cover = Box4::new(wide_left, cover_top, column, art_height);
    let progress = Box4::new(
        wide_left,
        (wide_top + art_height + 92.0 * ui_scale).max(0.0),
        column,
        36.0 * ui_scale,
    );
    let toolbar = Box4::new(
        wide_left,
        (wide_top + art_height + 146.0 * ui_scale).max(0.0),
        column,
        64.0 * ui_scale,
    );
    let cover_meta_top = if d.show_cover {
        cover.bottom() + 24.0 * ui_scale
    } else {
        cover.bottom() + 12.0 * ui_scale
    };
    let font = if fullscreen {
        font_px(w, h, d.font_size)
    } else {
        let ref_h = if screen.1 <= 1.0 { h } else { h.min(screen.1) };
        (0.05 * ref_h).max(12.0) * (d.font_size / BASE_FONT as f64) as f32
    };
    Metrics {
        win_w: w,
        win_h: h,
        font,
        line_height: font * d.line_height_em,
        // `--ol-trans-size: calc(<pct> * max(.5em, 10px))`: a translation is
        // half the base size before the user's multiplier, and never under
        // 10 px.
        trans_font: (font * 0.5).max(10.0) * (d.trans_font_size as f32 / 100.0),
        trans_height: ((font * 0.5).max(10.0) * (d.trans_font_size as f32 / 100.0) * 1.5)
            .max((font * 0.5).max(10.0)),
        lyrics,
        lyric_viewport: lyrics,
        cover,
        column,
        art_height,
        cover_meta_top,
        feather_px: d.edge_feather * h_scale,
        anchor_px: lyrics.h * (d.top_offset / 100.0),
        wide_left,
        toolbar,
        progress,
        ui_scale,
    }
}

/// One lyric row as the renderer draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Concatenated word text, exactly what gets shaped.
    pub text: String,
    /// `(text, start_ms, end_ms)` in document order, untrimmed.
    pub words: Vec<(String, i64, i64)>,
    pub start_ms: f64,
    pub end_ms: f64,
    pub is_bg: bool,
    pub is_duet: bool,
    pub word_timed: bool,
    /// `_srcIndex`, used to attach the translation under the last row of a group.
    pub group: usize,
    /// Translation drawn under this row, empty when there is none.
    pub translation: String,
    pub romanization: String,
}

impl Row {
    pub fn duration_ms(&self) -> f64 {
        (self.end_ms - self.start_ms).max(0.0)
    }
}

/// Project AMLL lines onto drawable rows.
///
/// Each line keeps its own translation (`trans` on the main line, `bgTrans` on
/// the harmony line), matching AMLL's `LyricLineEl.setSubLinesText`. Empty rows
/// are dropped so an instrumental gap does not draw a blank.
fn matching_outer_parens(trimmed: &str) -> Option<(char, char)> {
    let first = trimmed.chars().next()?;
    let expected_close = match first {
        '(' => ')',
        '（' => '）',
        _ => return None,
    };
    let mut depth = 0usize;
    let len = trimmed.len();
    for (idx, ch) in trimmed.char_indices() {
        match ch {
            '(' | '（' => depth += 1,
            ')' | '）' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if idx + ch.len_utf8() == len && ch == expected_close {
                        return Some((first, ch));
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_outer_parens_str(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some((open, close)) = matching_outer_parens(trimmed) {
        trimmed[open.len_utf8()..trimmed.len() - close.len_utf8()]
            .trim()
            .to_string()
    } else {
        raw.to_string()
    }
}

fn strip_bg_word_parens(words: &mut Vec<(String, i64, i64)>, text: &str) -> String {
    let trimmed = text.trim();
    let Some((open, close)) = matching_outer_parens(trimmed) else {
        return text.to_string();
    };
    // Strip the leading opening parenthesis and any surrounding whitespace before it.
    for word in words.iter_mut() {
        let trim_s = word.0.trim_start();
        if trim_s.is_empty() {
            word.0.clear();
            continue;
        }
        if let Some(rest) = trim_s.strip_prefix(open) {
            word.0 = rest.trim_start().to_string();
        }
        break;
    }
    while words.first().is_some_and(|w| w.0.is_empty()) {
        words.remove(0);
    }
    if let Some(first) = words.first_mut() {
        let trimmed_start = first.0.trim_start().to_string();
        first.0 = trimmed_start;
    }
    // Strip the trailing closing parenthesis and any trailing whitespace after it.
    for word in words.iter_mut().rev() {
        let trim_e = word.0.trim_end();
        if trim_e.is_empty() {
            word.0.clear();
            continue;
        }
        if let Some(rest) = trim_e.strip_suffix(close) {
            word.0 = rest.trim_end().to_string();
        }
        break;
    }
    while words.last().is_some_and(|w| w.0.is_empty()) {
        words.pop();
    }
    if let Some(last) = words.last_mut() {
        let trimmed_end = last.0.trim_end().to_string();
        last.0 = trimmed_end;
    }
    words.iter().map(|w| w.0.as_str()).collect()
}

pub fn rows_from(lines: &[AmllLine]) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(lines.len());
    let mut main_duet = false;
    for line in lines {
        let raw_text: String = line.words.iter().map(|w| w.word.as_str()).collect();
        if raw_text.trim().is_empty() {
            continue;
        }
        let mut words: Vec<(String, i64, i64)> = line
            .words
            .iter()
            .map(|w| (w.word.clone(), w.start_time, w.end_time))
            .collect();
        let (text, translation) = if line.is_bg {
            let text = strip_bg_word_parens(&mut words, &raw_text);
            let translation = strip_outer_parens_str(line.translated_lyric.trim())
                .trim()
                .to_string();
            (text, translation)
        } else {
            (raw_text, line.translated_lyric.trim().to_string())
        };
        if text.trim().is_empty() {
            continue;
        }
        if !line.is_bg {
            main_duet = line.is_duet;
        }
        rows.push(Row {
            text,
            words,
            start_ms: line.start_time,
            end_ms: line.end_time,
            is_bg: line.is_bg,
            is_duet: main_duet,
            word_timed: true,
            group: line.src_index,
            translation,
            romanization: line.roman_lyric.clone(),
        });
    }
    rows
}

#[cfg(test)]
mod profile_and_bg_tests {
    use super::*;
    use oly_contracts::AmllWord;

    #[test]
    fn strip_outer_parens_on_bg_rows_preserves_inner_and_paired_groups() {
        let amll = vec![
            AmllLine {
                words: vec![
                    AmllWord { word: "(".into(), start_time: 100, end_time: 120 },
                    AmllWord { word: "Ooh".into(), start_time: 120, end_time: 400 },
                    AmllWord { word: ")".into(), start_time: 400, end_time: 420 },
                ],
                start_time: 100.0,
                end_time: 420.0,
                translated_lyric: "（喔）".into(),
                roman_lyric: String::new(),
                is_bg: true,
                is_duet: false,
                src_index: 0,
            },
            AmllLine {
                words: vec![
                    AmllWord { word: "(A) ".into(), start_time: 500, end_time: 700 },
                    AmllWord { word: "(B)".into(), start_time: 700, end_time: 900 },
                ],
                start_time: 500.0,
                end_time: 900.0,
                translated_lyric: String::new(),
                roman_lyric: String::new(),
                is_bg: true,
                is_duet: false,
                src_index: 1,
            },
            AmllLine {
                words: vec![
                    AmllWord { word: "(Main line)".into(), start_time: 1000, end_time: 1500 },
                ],
                start_time: 1000.0,
                end_time: 1500.0,
                translated_lyric: "(主唱)".into(),
                roman_lyric: String::new(),
                is_bg: false,
                is_duet: false,
                src_index: 2,
            },
        ];
        let rows = rows_from(&amll);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].text, "Ooh");
        assert_eq!(rows[0].words, vec![("Ooh".into(), 120, 400)]);
        assert_eq!(rows[0].translation, "喔");
        assert_eq!(rows[1].text, "(A) (B)");
        assert_eq!(rows[2].text, "(Main line)");
        assert_eq!(rows[2].translation, "(主唱)");
    }

    #[test]
    fn from_config_for_keeps_cover_and_mini_profiles_separate() {
        let cfg = serde_json::json!({
            "display": {
                "showCover": true,
                "fontSize": 32,
                "typeProfiles": {
                    "cover": { "fontSize": 40, "playingLineTopOffset": 42 },
                    "mini": { "fontSize": 18, "playingLineTopOffset": 50 }
                }
            }
        });
        let win_d = Display::from_config_for(&cfg, "cover");
        let mini_d = Display::from_config_for(&cfg, "mini");
        assert_eq!(win_d.font_size, 40.0);
        assert_eq!(mini_d.font_size, 18.0);
        assert_eq!(win_d.top_offset, 42.0);
        assert_eq!(mini_d.top_offset, 50.0);
    }
}

/// Height of one drawn row, given how many visual lines it wrapped to.
pub fn row_height(lines: usize, has_translation: bool, m: &Metrics, d: &Display, is_bg: bool) -> f32 {
    let scale = if is_bg { d.bg_scale } else { 1.0 };
    let body = m.line_height * lines.max(1) as f32 * scale;
    let trans = if has_translation && d.show_translation {
        m.trans_height * scale + m.trans_font * scale * d.inner_gap
    } else {
        0.0
    };
    body + trans + row_padding(m, d, is_bg) * 2.0 + m.font * scale * (0.2 + d.line_gap).max(0.0)
}

/// Romanisation follows translation and has no inner-gap margin in adapter CSS.
pub fn roman_metrics(m: &Metrics, d: &Display, is_bg: bool) -> (f32, f32) {
    let scale = if is_bg { d.bg_scale } else { 1.0 };
    let font = (m.font * scale * d.roman_font_scale).max(10.0);
    (font, font * 1.5)
}

pub fn roman_height(has_romanization: bool, m: &Metrics, d: &Display, is_bg: bool) -> f32 {
    if has_romanization && d.show_romanization { roman_metrics(m, d, is_bg).1 } else { 0.0 }
}

/// Wrapper padding is expressed in the row's main font em, matching adapter CSS.
pub fn row_padding(m: &Metrics, d: &Display, is_bg: bool) -> f32 {
    m.font * if is_bg { d.bg_scale } else { 1.0 } * (0.4 + d.line_gap).max(0.0)
}

/// Vertical geometry of the rows for one frame.
///
/// `tops[i]` is the row's top in container coordinates with the focal row on
/// its anchor, which is the reference's zero-offset baseline; the caller adds
/// the scroll offset.
#[derive(Clone, Debug, PartialEq)]
pub struct RowGeometry {
    pub tops: Vec<f32>,
    pub heights: Vec<f32>,
    /// Sum of every row height.
    pub total: f32,
    /// Content-space top of the focal row.
    pub focal_top: f32,
    /// Scroll range that keeps the focal row reachable.
    pub min: f32,
    pub max: f32,
}

impl RowGeometry {
    pub fn follow_target(&self) -> f32 { self.max - self.focal_top }
}

/// Build the row stack for one frame.
///
/// Tops stay in content space; follow_target positions the focal row at the
/// viewport anchor without changing the range available to manual browsing.
pub fn row_geometry(
    heights: &[f32],
    focal: usize,
    anchor_px: f32,
    container_h: f32,
) -> RowGeometry {
    let mut tops = Vec::with_capacity(heights.len());
    let mut y = 0.0f32;
    for h in heights {
        tops.push(y);
        y += *h;
    }
    let total = y;
    let focal_top = tops.get(focal).copied().unwrap_or(0.0);
    let _ = container_h;
    // A content-space offset: every row remains reachable, regardless of playback.
    let min = (anchor_px - tops.last().copied().unwrap_or(0.0)).min(0.0);
    RowGeometry {
        tops,
        heights: heights.to_vec(),
        total,
        focal_top,
        min,
        max: anchor_px.max(0.0),
    }
}

/// Vertical geometry for main/BG lyric groups, optional interlude dots slot,
/// and optional bottom credits line.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupGeometry {
    pub tops: Vec<f32>,
    pub heights: Vec<f32>,
    pub interlude_top: Option<f32>,
    pub bottom_top: f32,
    pub total: f32,
    pub focal_top: f32,
    pub min: f32,
    pub max: f32,
}

impl GroupGeometry {
    pub fn follow_target(&self) -> f32 {
        self.max - self.focal_top
    }
}

pub fn group_geometry(
    heights: &[f32],
    focal: oly_core::timeline::FocalTarget,
    anchor_px: f32,
    interlude: Option<(isize, f32)>,
    has_bottom: bool,
) -> GroupGeometry {
    use oly_core::timeline::FocalTarget;
    let mut tops = Vec::with_capacity(heights.len());
    let mut y = 0.0f32;
    let mut interlude_top = None;
    let interlude_anchor = interlude.map(|(a, _)| a);
    let interlude_h = interlude.map(|(_, h)| h).unwrap_or(0.0);
    if interlude_anchor == Some(-1) {
        interlude_top = Some(y);
        y += interlude_h;
    }
    for (i, h) in heights.iter().enumerate() {
        tops.push(y);
        y += *h;
        if interlude_anchor == Some(i as isize) {
            interlude_top = Some(y);
            y += interlude_h;
        }
    }
    let total = y;
    let bottom_top = y;
    let focal_top = match focal {
        FocalTarget::Line(idx) => tops.get(idx).copied().unwrap_or(0.0),
        FocalTarget::Interlude(_) => interlude_top.unwrap_or(0.0),
        FocalTarget::Bottom => bottom_top,
    };
    let last_reachable = if has_bottom {
        bottom_top
    } else {
        tops.last().copied().unwrap_or(0.0)
    };
    let min = (anchor_px - last_reachable).min(0.0);
    GroupGeometry {
        tops,
        heights: heights.to_vec(),
        interlude_top,
        bottom_top,
        total,
        focal_top,
        min,
        max: anchor_px.max(0.0),
    }
}

/// Active-ness fraction `[0, 1]` derived from AMLL's `±80` `bgSlideY` spring.
pub fn bg_wrapper_progress(slide_y: f64) -> f32 {
    (1.0 - slide_y.abs() / 80.0).clamp(0.0, 1.0) as f32
}

/// Wrapper scale (`0.8 -> 1.0`) matching AMLL's `.bgWrapper`.
pub fn bg_wrapper_scale(active_progress: f32) -> f32 {
    0.8 + active_progress * 0.2
}

/// Linear CSS-like opacity transition over `duration_ms`.
pub fn step_opacity(current: f32, target: f32, dt_ms: f64, duration_ms: f64, immediate: bool) -> f32 {
    if immediate || duration_ms <= 0.0 {
        return target;
    }
    if dt_ms <= 0.0 {
        return current;
    }
    let step = (dt_ms / duration_ms) as f32;
    if current < target {
        (current + step).min(target)
    } else if current > target {
        (current - step).max(target)
    } else {
        target
    }
}

/// Automatic motion owns the spring; direct manipulation stays one-to-one.
#[derive(Default)]
pub struct FollowMotion {
    spring: Option<oly_core::scroll::Spring>,
    quiet_ms: f64,
    was_browsing: bool,
    params: Option<oly_core::scroll::SpringParams>,
}
impl FollowMotion {
    pub fn is_direct(&self) -> bool { self.was_browsing }
    pub fn pending(&self) -> bool { self.was_browsing || self.spring.as_ref().is_some_and(|s| !s.arrived()) }
    pub fn resume_now(&mut self) {
        self.was_browsing = false;
        self.quiet_ms = 0.0;
    }
    pub fn step(&mut self, current: f64, target: f64, browsing: bool, dt_ms: f64, interval: Option<f64>) -> f64 {
        self.step_configured(current, target, browsing, dt_ms, interval, true,
            oly_core::scroll::pos_y_spring_policy(false, false, interval, false))
    }

    pub fn step_configured(&mut self, current: f64, target: f64, browsing: bool, dt_ms: f64,
        _interval: Option<f64>, enabled: bool, params: oly_core::scroll::SpringParams) -> f64 {
        use oly_core::scroll::{Spring, AUTO_ALIGN_RESUME_DELAY_MS};
        let first = self.spring.is_none();
        let spring = self.spring.get_or_insert_with(|| Spring::new(target));
        if browsing {
            spring.set_position(current);
            self.quiet_ms = 0.0;
            self.was_browsing = true;
            return current;
        }
        if self.was_browsing {
            self.quiet_ms += dt_ms.max(0.0);
            if self.quiet_ms < AUTO_ALIGN_RESUME_DELAY_MS { return current; }
            spring.set_position(current);
            self.was_browsing = false;
        }
        if self.params != Some(params) {
            spring.update_params(params);
            self.params = Some(params);
        }
        if !enabled {
            spring.set_position(target);
            return target;
        }
        if first { return target; }
        if spring.target() != target {
            spring.set_target_position(target);
        }
        spring.update(dt_ms.clamp(0.0, 100.0) / 1000.0);
        spring.position()
    }
}

/// Per-row delayed spring, reproducing AMLL's cascade rather than a rigid stack.
pub struct RowMotion {
    spring: oly_core::scroll::Spring,
    requested: f64,
    params: Option<oly_core::scroll::SpringParams>,
}
impl RowMotion {
    pub fn new(position: f64) -> Self { Self { spring: oly_core::scroll::Spring::new(position), requested: position, params: None } }
    pub fn position(&self) -> f64 { self.spring.position() }
    pub fn pending(&self) -> bool { !self.spring.arrived() }
    pub fn step(&mut self, target: f64, direct: bool, delay: f64, dt_ms: f64, params: oly_core::scroll::SpringParams) -> f64 {
        if direct {
            self.spring = oly_core::scroll::Spring::new(target);
            self.requested = target;
            self.params = None;
        } else {
            if (self.requested - target).abs() > 0.001 || self.params != Some(params) {
                self.requested = target;
                self.spring.update_params(params);
                self.params = Some(params);
                self.spring.set_target_after(target, delay);
            }
            self.spring.update(dt_ms.clamp(0.0, 100.0) / 1000.0);
        }
        self.spring.position()
    }
}

/// AMLL restores full size on pause; inactive playing rows settle at 97%.
pub fn line_scale_target(active: bool, playing: bool, background: bool) -> f64 {
    if active || !playing { 100.0 } else if background { 75.0 } else { 97.0 }
}

/// How one row is lit this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Light {
    /// Alpha for unplayed glyphs.
    pub dark: f32,
    /// Alpha for glyphs the karaoke band has passed.
    pub bright: f32,
    /// Per-word fill progress in `[0,1]`, `None` when the row is not active.
    pub sweep: Option<Vec<f32>>,
    /// Per-word glow alpha, parallel to `sweep`.
    pub glow: Option<Vec<f32>>,
}

/// Glow envelope for one held word.
///
/// The ported animator ramps a text-shadow in and out across the note; the
/// shipped profile scales the peak by [`GLOW_ALPHA_SCALE`]. Sampling the
/// envelope directly here keeps the same shape without depending on the
/// keyframe table's internals.
pub fn glow_alpha(duration_ms: i64, progress: f32) -> f32 {glow_alpha_threshold(duration_ms,progress,1000)}
fn glow_alpha_threshold(duration_ms:i64,progress:f32,threshold:i64)->f32 {
    if duration_ms < threshold.max(0) {
        return 0.0;
    }
    let p = progress.clamp(0.0, 1.0);
    let wave = (std::f32::consts::PI * p).sin().max(0.0);
    wave.powf(0.7) * GLOW_ALPHA_SCALE as f32
}

/// Light one row at `playhead_ms`.
///
/// A row that is playing gets its per-word sweep; a row already sung keeps the
/// `unplayed` alpha but is drawn at full brightness so the eye can follow the
/// song, and a row that has not started sits at `idle`.
pub fn row_light(
    row: &Row,
    playhead_ms: f64,
    is_active: bool,
    is_bg: bool,
    d: &Display,
) -> Light {
    let base = if is_bg { d.bg_opacity } else { d.unplayed };
    let idle = if is_bg { d.bg_opacity * 0.5 } else { d.idle };
    if !is_active {
        // Passed rows keep the reference's "已唱" styling: dimmer than the
        // playing line but above the unplayed floor.
        let passed = row.end_ms <= playhead_ms;
        let alpha = if passed {
            (base * 1.15).min(1.0)
        } else {
            idle
        };
        return Light {
            dark: alpha,
            bright: alpha,
            sweep: None,
            glow: None,
        };
    }

    let mut sweep = Vec::with_capacity(row.words.len());
    let mut glow = Vec::with_capacity(row.words.len());
    let total_duration = row.duration_ms().max(1.0);
    for (_text, start, end) in &row.words {
        let span = (*end - *start).max(1) as f64;
        let p = ((playhead_ms - *start as f64) / span).clamp(0.0, 1.0) as f32;
        sweep.push(p);
        // The glow peaks while the note is held, not after it ends.
        let line_p = ((playhead_ms - row.start_ms) / total_duration).clamp(0.0, 1.0) as f32;
        glow.push(glow_alpha_threshold((*end - *start).max(0), line_p,d.long_syllable));
    }
    Light {
        dark: base,
        bright: d.bright,
        sweep: Some(sweep),
        glow: Some(glow),
    }
}

/// Index of the row playing at `playhead_ms`, preferring the last match so an
/// overlapping harmony keeps focus on the lead.
pub fn active_row(rows: &[Row], playhead_ms: f64) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| r.start_ms <= playhead_ms && playhead_ms < r.end_ms)
        .map(|(i, _)| i)
        .next()
        .or_else(|| {
            // Between lines: stay on the last row that started, so the view does
            // not jump to the top of the song in an instrumental gap.
            rows.iter()
                .enumerate()
                .filter(|(_, r)| r.start_ms <= playhead_ms)
                .map(|(i, _)| i)
                .next_back()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> Display {
        Display::default()
    }

    #[test]
    fn romanization_visibility_and_font_scale_control_row_geometry() {
        let mut d = Display::from_config(&serde_json::json!({"showRomanization": false, "romanFontScale": 75, "phoneticLyricsOpacity": 65, "blurMax": true}));
        let m = metrics(1920.0,1080.0,(1920.0,1080.0),true,&d);
        assert_eq!(roman_height(true,&m,&d,false),0.0);
        d.show_romanization = true;
        assert!((roman_height(true,&m,&d,false) - m.font*0.75*1.5).abs() < 0.001);
        assert_eq!(roman_height(false,&m,&d,false),0.0);
        assert_eq!(d.phonetic_opacity,0.65);
        assert!(d.blur_max);
    }

    #[test]
    fn config_uses_active_profile_and_legacy_font_inheritance() {
        let config: serde_json::Value = serde_json::from_str(include_str!("../../../oly-core/src/config_defaults.json")).unwrap();
        let cover = Display::from_config(&config);
        assert_eq!((cover.font_size, cover.top_offset, cover.weight), (95.0, 30.0, 650.0));
        assert_eq!((cover.line_gap, cover.inner_gap), (0.02, 0.2));
        let mut no_cover = config;
        no_cover["display"]["showCover"] = serde_json::json!(false);
        let no_cover = Display::from_config(&no_cover);
        assert_eq!((no_cover.font_size, no_cover.top_offset, no_cover.trans_font_size), (100.0, 40.0, 90.0));
        assert!(!no_cover.enable_scale && no_cover.hide_passed_lines);
        let inherited = Display::from_config(&serde_json::json!({"display": {"fontFamily": "宋体", "transFontFamily": "", "playingLineTopOffset": 1}}));
        assert_eq!(inherited.font_family, "宋体");
        assert_eq!(inherited.trans_font_family, "宋体");
        assert_eq!(inherited.top_offset, 42.0);
    }

    #[test]
    fn hiding_the_cover_only_collapses_the_artwork_slot() {
        // `[data-show-cover='false']` zeroes `--wide-art-height` only. The
        // column, the lyric column, the progress bar and the transport keep
        // their widths; the cover box keeps its width but loses its height and
        // moves down by the 20px flex gap.
        let with = metrics(1272.0, 843.0, (1920.0, 1080.0), false, &display());
        let mut d = display();
        d.show_cover = false;
        let without = metrics(1272.0, 843.0, (1920.0, 1080.0), false, &d);
        assert!(with.art_height > 0.0);
        assert_eq!(without.art_height, 0.0);
        assert!((without.column - with.column).abs() < 0.01);
        assert!((without.lyrics.x - (1272.0 - without.lyrics.right())).abs() < 0.01);
        assert!(without.lyrics.x > 60.0 && without.lyrics.w <= 1080.0);
        assert!((without.progress.w - with.progress.w).abs() < 0.01);
        assert!((without.toolbar.w - with.toolbar.w).abs() < 0.01);
        assert!((without.cover.w - with.cover.w).abs() < 0.01);
        assert_eq!(without.cover.h, 0.0);
        assert!((without.cover.y - (with.cover.y + 20.0 * with.ui_scale)).abs() < 0.01);
        assert!((without.progress.y - (with.cover.y + 92.0 * with.ui_scale)).abs() < 0.01);
        assert!((without.toolbar.y - (with.cover.y + 146.0 * with.ui_scale)).abs() < 0.01);
        assert!((without.cover_meta_top - (without.cover.y + 12.0 * with.ui_scale)).abs() < 0.01);
        assert!((with.cover_meta_top - (with.cover.bottom() + 24.0 * with.ui_scale)).abs() < 0.01);
    }

    #[test]
    fn extract_cover_accent_picks_saturated_color() {
        let mut rgba = vec![0u8; 16 * 16 * 4];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = 240;
            px[1] = 110;
            px[2] = 30;
            px[3] = 255;
        }
        let hex = extract_cover_accent(&rgba, 16, 16).expect("accent");
        assert_ne!(hex, 0);
    }

    #[test]
    fn each_spring_feel_matches_the_adapter_constants_without_compounding() {
        for (name, mass, damping, stiffness) in [("soft",0.99,21.75,55.8),("standard",0.9,15.0,90.0),("lively",0.9,10.8,135.0)] {
            let d = Display::from_config(&serde_json::json!({"springFeel": name}));
            let p = d.spring_params();
            assert_eq!((p.mass,p.damping,p.stiffness), (mass,damping,stiffness));
            assert_eq!(p, d.spring_params());
        }
    }

    #[test]
    fn disabled_spring_preserves_manual_browsing_before_snapping_to_follow() {
        let mut follow = FollowMotion::default();
        let params = Display::default().spring_params();
        assert_eq!(follow.step_configured(0.0, 100.0, false, 4.0, None, false, params), 100.0);
        assert_eq!(follow.step_configured(-200.0, 100.0, true, 4.0, None, false, params), -200.0);
        assert_eq!(follow.step_configured(-200.0, 100.0, false, 400.0, None, false, params), -200.0);
        assert_eq!(follow.step_configured(-200.0, 100.0, false, 200.0, None, false, params), 100.0);
    }

    #[test]
    fn live_font_percent_and_em_spacing_follow_adapter_css() {
        assert!((font_px(1920.0, 1080.0, 100.0) - 54.0).abs() < 0.001);
        let mut d = display();
        d.line_gap = 0.1;
        d.inner_gap = 0.4;
        let m = metrics(1920.0, 1080.0, (1920.0, 1080.0), true, &d);
        let expected = m.line_height + m.font * 1.3 + m.trans_height + m.trans_font * 0.4;
        assert!((row_height(1, true, &m, &d, false) - expected).abs() < 0.001);
    }
    #[test]
    fn the_frozen_region_table_is_reproduced() {
        let full = metrics(1920.0, 1080.0, (1920.0, 1080.0), true, &display());
        assert!((full.cover.x - 120.0).abs() < 0.01);
        assert!((full.cover.w - 600.0).abs() < 0.01);
        assert!((full.lyrics.x - 842.0).abs() < 0.01);
        assert!((full.lyrics.w - 1024.0).abs() < 0.01);
        assert!((full.lyrics.y - 86.390625).abs() < 0.01);

        let win_med = metrics(1272.0, 843.0, (1920.0, 1080.0), false, &display());
        assert!(win_med.cover.w > 420.0 && win_med.cover.w < 470.0);
        assert!(((win_med.lyrics.h / win_med.font) - (full.lyrics.h / full.font)).abs() < 0.01);
        assert!(win_med.toolbar.bottom() <= 843.0);

        let win_small = metrics(800.0, 500.0, (1920.0, 1080.0), false, &display());
        assert!(win_small.cover.w < win_med.cover.w);
        assert!(((win_small.lyrics.h / win_small.font) - (full.lyrics.h / full.font)).abs() < 0.01);
        assert!(win_small.toolbar.bottom() <= 500.0);
    }

    #[test]
    fn the_font_chain_matches_the_three_measured_sizes() {
        assert!((font_px(1920.0, 1080.0, 95.0) - 51.3).abs() < 0.01);
        assert!((font_px(1272.0, 843.0, 95.0) - 40.0425).abs() < 0.02);
        assert!((font_px(800.0, 500.0, 95.0) - 23.75).abs() < 0.01);
    }

    #[test]
    fn a_smaller_settings_font_scales_the_chain_rather_than_the_window() {
        let full = font_px(1920.0, 1080.0, 68.0);
        let half = font_px(1920.0, 1080.0, 34.0);
        assert!((half - full * 0.5).abs() < 0.01);
    }

    #[test]
    fn the_mask_fade_is_half_the_line_height() {
        let d = display();
        let m = metrics(1920.0, 1080.0, (1920.0, 1080.0), true, &d);
        assert!((m.line_height * d.word_fade - 51.3 * 1.2 * 0.5).abs() < 0.01);
    }

    #[test]
    fn glow_only_fires_for_held_notes_and_peaks_in_the_middle() {
        assert_eq!(glow_alpha(999, 0.5), 0.0);
        let mid = glow_alpha(1500, 0.5);
        assert!(mid > 0.7 && mid <= 0.75, "{mid}");
        assert!(glow_alpha(1500, 0.0) < 0.01);
        assert!(glow_alpha(1500, 1.0) < 0.01);
    }

    #[test]
    fn a_playing_row_sweeps_and_a_passed_row_does_not() {
        let rows = vec![Row {
            text: "All".into(),
            words: vec![("All".into(), 0, 1000)],
            start_ms: 0.0,
            end_ms: 1000.0,
            is_bg: false,
            word_timed: true,
            is_duet: false,
            group: 0,
            translation: String::new(),
            romanization: String::new(),
        }];
        let d = display();
        let light = row_light(&rows[0], 500.0, true, false, &d);
        assert_eq!(light.sweep.as_deref(), Some(&[0.5f32][..]));
        let passed = row_light(&rows[0], 1500.0, false, false, &d);
        assert!(passed.sweep.is_none());
        assert_eq!(passed.dark, passed.bright);
    }

    #[test]
    fn the_active_row_stays_on_the_last_started_row_between_lines() {
        let mk = |start: f64, end: f64| Row {
            text: "x".into(),
            words: vec![("x".into(), start as i64, end as i64)],
            start_ms: start,
            end_ms: end,
            is_bg: false,
            word_timed: true,
            is_duet: false,
            group: 0,
            translation: String::new(),
            romanization: String::new(),
        };
        let rows = vec![mk(0.0, 100.0), mk(200.0, 300.0)];
        assert_eq!(active_row(&rows, 50.0), Some(0));
        assert_eq!(active_row(&rows, 150.0), Some(0));
        assert_eq!(active_row(&rows, 250.0), Some(1));
        assert_eq!(active_row(&rows, 999.0), Some(1));
    }

    #[test]
    fn each_row_keeps_its_own_translation_and_harmony_retains_bg_trans() {
        let lines = vec![
            AmllLine {
                words: vec![oly_contracts::AmllWord {
                    start_time: 0,
                    end_time: 100,
                    word: "a".into(),
                }],
                translated_lyric: "主译".into(),
                roman_lyric: "pinyin".into(),
                start_time: 0.0,
                end_time: 100.0,
                is_bg: false,
                is_duet: false,
                src_index: 3,
            },
            AmllLine {
                words: vec![oly_contracts::AmllWord {
                    start_time: 0,
                    end_time: 100,
                    word: "b".into(),
                }],
                translated_lyric: "和声译".into(),
                roman_lyric: String::new(),
                start_time: 0.0,
                end_time: 100.0,
                is_bg: true,
                is_duet: false,
                src_index: 3,
            },
        ];
        let rows = rows_from(&lines);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].romanization, "pinyin");
        assert_eq!(rows[0].translation, "主译");
        assert_eq!(rows[1].translation, "和声译");
    }

    #[test]
    fn group_geometry_inserts_interlude_and_reaches_bottom_credits() {
        use oly_core::timeline::FocalTarget;
        let heights = [60.0f32, 80.0];
        let g = group_geometry(&heights, FocalTarget::Interlude(0), 200.0, Some((0, 40.0)), true);
        assert_eq!(g.tops, vec![0.0, 100.0]);
        assert_eq!(g.interlude_top, Some(60.0));
        assert_eq!(g.bottom_top, 180.0);
        assert_eq!(g.focal_top, 60.0);
        assert_eq!(g.tops[1] + g.follow_target(), 240.0);

        let end = group_geometry(&heights, FocalTarget::Bottom, 200.0, None, true);
        assert_eq!(end.focal_top, 140.0);
        assert!(end.follow_target() >= end.min && end.follow_target() <= end.max);
    }

    #[test]
    fn the_scroll_range_keeps_the_focal_row_reachable() {
        let heights = [100.0f32; 10];
        let g = row_geometry(&heights, 5, 200.0, 600.0);
        assert_eq!(g.total, 1000.0);
        assert_eq!(g.focal_top, 500.0);
        assert!(g.max > 0.0);
        // With no user displacement the focal row sits on the anchor.
        let focal_screen = g.tops[5] - g.focal_top + 200.0;
        assert!((focal_screen - 200.0).abs() < 0.01);
    }
    #[test]
    fn browsing_bounds_do_not_move_when_the_current_line_changes() {
        let a = row_geometry(&[100.0; 10], 0, 200.0, 600.0);
        let b = row_geometry(&[100.0; 10], 7, 200.0, 600.0);
        assert_eq!((a.min, a.max), (b.min, b.max));
        assert!(a.min <= -700.0 && a.max >= 200.0);
    }

    #[test]
    fn automatic_target_moves_up_by_the_height_of_each_passed_row() {
        let a = row_geometry(&[100.0; 10], 0, 200.0, 600.0);
        let b = row_geometry(&[100.0; 10], 5, 200.0, 600.0);
        assert_eq!(a.follow_target(), 200.0);
        assert_eq!(b.follow_target(), -300.0);
        assert_eq!(b.tops[5] + b.follow_target(), 200.0);
    }

    #[test]
    fn following_never_fights_a_gesture_and_waits_before_returning() {
        let mut f = FollowMotion::default();
        assert_eq!(f.step(0.0, 200.0, false, 4.0, None), 200.0);
        for _ in 0..200 { assert_eq!(f.step(-400.0, 100.0, true, 4.0, None), -400.0); }
        for _ in 0..100 { assert_eq!(f.step(-400.0, 100.0, false, 4.0, None), -400.0); }
        for _ in 0..600 { f.step(-400.0, 100.0, false, 4.0, None); }
        assert!((f.spring.unwrap().position() - 100.0).abs() < 0.1);
    }

    #[test]
    fn row_cascade_waits_while_manual_motion_is_immediate() {
        let params = oly_core::scroll::SpringParams::new(170.0,28.0);
        let mut a = RowMotion::new(0.0);
        let mut b = RowMotion::new(0.0);
        for _ in 0..8 {
            a.step(-100.0,false,0.0,4.0,params);
            b.step(-100.0,false,0.05,4.0,params);
        }
        assert!(a.spring.position() < -1.0);
        assert_eq!(b.spring.position(),0.0);
        assert_eq!(b.step(80.0,true,0.05,4.0,params),80.0);
    }

    #[test]
    fn delayed_rows_keep_frames_alive_and_direct_motion_cancels_the_queue() {
        let mut row = RowMotion::new(0.0);
        assert!(!row.pending());
        row.step(100.0,false,0.1,4.0,oly_core::scroll::SpringParams::default());
        assert!(row.pending());
        row.step(30.0,true,0.0,4.0,oly_core::scroll::SpringParams::default());
        assert!(!row.pending());
        assert_eq!(row.step(30.0,false,0.0,500.0,oly_core::scroll::SpringParams::default()),30.0);
    }

    #[test]
    fn paused_and_active_rows_are_full_size_while_playing_neighbors_recede() {
        assert_eq!(line_scale_target(true,true,false),100.0);
        assert_eq!(line_scale_target(false,true,false),97.0);
        assert_eq!(line_scale_target(false,true,true),75.0);
        assert_eq!(line_scale_target(false,false,true),100.0);
    }

}

/// Host owns clipping/gestures; text padding and width never resize the host.
pub fn lyric_content(viewport:Box4,font:f32,window_width:f32,d:&Display)->Box4 {
 let pad=(font*d.line_padding_x.unwrap_or(if window_width<=768.{0.}else{1.})).min(((viewport.w-1.)*0.5).max(0.));
 let available=(viewport.w-2.*pad).max(1.);let width=(available*d.line_width_aspect).max(1.).min(available);
 Box4::new(viewport.x+pad.min(viewport.w*0.5)+if d.align_right{available-width}else{0.},viewport.y,width,viewport.h)
}
#[cfg(test)] mod viewport_tests {
 use super::*;
 #[test] fn text_width_and_padding_stay_inside_the_legacy_host(){let host=Box4::new(580.,67.,660.,709.);let mut d=Display::default();d.line_padding_x=Some(1.);d.line_width_aspect=0.7;let a=lyric_content(host,40.,1272.,&d);d.align_right=true;let b=lyric_content(host,40.,1272.,&d);assert_eq!(a.y,host.y);assert_eq!(a.h,host.h);assert_eq!(a.w,b.w);assert!(a.x>=host.x&&b.right()<=host.right());assert!(b.x>a.x);}
}

/// The two compact layouts from lyrics.css; window height never moves the header.
pub fn compact_metrics(w:f32,h:f32,lyrics:bool,d:&Display)->Metrics {
 let mut m=metrics(w,h,(w,h),false,d);
 m.column=(w-32.).max(1.);m.wide_left=16.;m.art_height=56.;
 m.cover=Box4::new(16.,16.,m.art_height,m.art_height);m.cover_meta_top=22.;
 m.toolbar=if lyrics{Box4::new((w-296.)/2.,h-106.,296.,52.)}else{Box4::new((w-192.)/2.,86.,192.,44.)};
 m.progress=Box4::new(27.,if lyrics{h-160.}else{137.},(w-54.).max(1.),24.);
 m.lyrics=Box4::new(12.,104.,(w-24.).max(1.),(h-132.).max(1.));m.lyric_viewport=m.lyrics;
 m.font=font_px(w,if lyrics{h}else{650.},d.font_size);m.line_height=m.font*d.line_height_em;
 m.trans_font=(m.font*0.5).max(10.)*d.trans_font_size as f32/100.;m.trans_height=m.trans_font*1.5;
 m.anchor_px=m.lyrics.h*d.top_offset/100.;m.feather_px=40.;m
}
#[cfg(test)] mod compact_parity_tests {
 use super::*;
 #[test] fn compact_header_and_transport_follow_reference_css() {
 let d=Display::default();let mini=compact_metrics(380.,176.,false,&d);let lyrics=compact_metrics(380.,650.,true,&d);
 assert_eq!(mini.cover,Box4::new(16.,16.,56.,56.));assert_eq!(mini.cover,lyrics.cover);
 assert_eq!(mini.toolbar.y,86.);assert_eq!(mini.progress.y,137.);
 assert_eq!(lyrics.lyric_viewport,Box4::new(12.,104.,356.,518.));
 assert_eq!(lyrics.toolbar.bottom(),596.);assert_eq!(lyrics.progress.y,490.);
 }
}
