use super::*;

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
    pub display_lines: u8,
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
            display_lines: 1,
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
        if let Some(dl) = obj.get("displayLines").and_then(|v| v.as_u64()) {
            cfg.display_lines = (dl as u8).clamp(1, 3);
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
                "displayLines": self.display_lines.clamp(1, 3),
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

