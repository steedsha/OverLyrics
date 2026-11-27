use super::*;

pub struct DropdownPresentation {
    pub row_index: usize,
    pub option_count: usize,
    pub rect: Box4,
    pub options: Vec<(usize, String, String, Box4, bool, bool)>,
    pub lens_rect: Box4,
    pub lens_swell: f32,
    pub max_scroll: f32,
    pub scroll: f32,
    pub selected_idx: usize,
    pub is_color: bool,
    pub glass: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
}

impl DropdownPresentation {
    pub fn translate(&mut self, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.rect.x += dx;
        self.rect.y += dy;
        self.lens_rect.x += dx;
        self.lens_rect.y += dy;
        for (_, _, _, opt_r, _, _) in &mut self.options {
            opt_r.x += dx;
            opt_r.y += dy;
        }
    }
}

pub struct SegmentedPresentation {
    pub track_rect: Box4,
    pub well_rect: Box4,
    pub lens_rect: Box4,
    pub swell: f32,
    pub active_slot: usize,
    pub glass: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
}

pub struct Presentation {
    pub kind: SettingsKind,
    pub pages: &'static [&'static str],
    pub editing_key: Option<&'static str>,
    pub editing_draft: String,
    pub caret_visible: bool,
    pub rect: Box4,
    pub page: usize,
    pub title: &'static str,
    pub rows: Vec<(usize, Item, Box4)>,
    pub outgoing_rows: Vec<(usize, Item, Box4)>,
    pub page_progress: f32,
    pub page_dir: i8,
    pub values: Value,
    pub status: String,
    pub scroll: f32,
    pub max_scroll: f32,
    pub tab_well_rect: Box4,
    pub tab_lens_rect: Box4,
    pub tab_swell: f32,
    pub tab_lens_glass: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
    pub close_lens: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
    pub dropdown: Option<DropdownPresentation>,
    pub segmented: std::collections::HashMap<&'static str, SegmentedPresentation>,
    pub accent_hex: u32,
    pub switch_fractions: std::collections::HashMap<&'static str, f32>,
    pub active_player_bucket: String,
    pub active_profile: &'static str,
    pub active_mode_label: &'static str,
    pub artist_aliases: Vec<(String, String)>,
    pub custom_player_offsets: Vec<(String, f64)>,
    pub custom_models: Vec<String>,
    pub modal: Option<SettingsModal>,
    pub modal_offset: [f32; 2],
    pub modal_glass: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
    pub theme_mode: crate::player::model::ThemeMode,
    pub status_alpha: f32,
    pub reset_armed: bool,
    pub dragging_source_row: Option<usize>,
    pub source_drag_swell: f32,
    pub source_drag_y: f32,
    pub source_row_offsets: std::collections::HashMap<&'static str, f32>,
}
impl Settings {
    pub fn presentation(&self, rect: Box4) -> Presentation {
        let accent_hex = Display::from_config(&self.config).accent_hex;
        let mut values = self.effective();
        for item in self.items() { values[item.key] = self.value(item.key); }
        let mut switch_fractions = std::collections::HashMap::new();
        for item in self.items() {
            if matches!(item.control, Control::Switch) {
                let fallback = if values
                    .get(item.key)
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    1.0
                } else {
                    0.0
                };
                let frac = self
                    .switch_springs
                    .get(item.key)
                    .map(|s| s.position() as f32)
                    .unwrap_or(fallback);
                let frac=self.switch_preview.filter(|(key,_)|*key==item.key).map(|(_,f)|f).unwrap_or(frac);
                switch_fractions.insert(item.key, frac);
            }
        }
        let (tab_well_rect, tab_lens_rect) = if self.kind == SettingsKind::Application {
            let sw = Self::sidebar_width_for(self.kind, rect);
            let count = self.pages().len() as f32;
            let base_well = Box4::new(rect.x + 11.0, rect.y + 49.0, (sw - 18.0).max(40.0), count * 38.0 + 2.0);
            (
                self.tab_lens.well_rect_axis(base_well, true),
                self.tab_lens.v_rect(
                    rect.x + 18.0,
                    rect.y + 52.0,
                    (sw - 32.0).max(36.0),
                    crate::player::interaction::Feedback::default(),
                ),
            )
        } else {
            let base_well = Box4::new(rect.x + 10.0, rect.y + 52.0, rect.w - 20.0, 34.0);
            (
                self.tab_lens.well_rect_axis(base_well, false),
                self.tab_lens.rect(
                    rect.x + 12.0,
                    rect.y + 57.0,
                    24.0,
                    crate::player::interaction::Feedback::default(),
                ),
            )
        };
        let rows = self.rows(rect);
        let mut segmented = std::collections::HashMap::new();
        for (_, item, row) in &rows {
            if let Control::Segmented(options) = item.control {
                let count = options.len().max(1);
                let track = Self::segmented_track_rect(*row, count);
                let selected = options
                    .iter()
                    .position(|(k, _)| Some(*k) == values.get(item.key).and_then(Value::as_str))
                    .unwrap_or(0);
                let (well_rect, lens_rect, swell, active_slot) =
                    if let Some(lens) = self.segmented_lenses.get(item.key) {
                        let cell_w = track.w / count as f32;
                        let lens_rect = lens.rect(
                            track.x,
                            track.y + 3.0,
                            (track.h - 6.0).max(16.0),
                            crate::player::interaction::Feedback::default(),
                        );
                        let well_rect = lens.well_rect_axis(track, false);
                        let swell = lens.swell();
                        let held = self.segmented_drag.is_some_and(|(k, _)| k == item.key);
                        let active_slot = if held || lens.pending() || swell > 0.02 {
                            (((lens_rect.centre().0 - track.x) / cell_w.max(1.0)).floor() as isize)
                                .clamp(0, count.saturating_sub(1) as isize) as usize
                        } else {
                            selected
                        };
                        (well_rect, lens_rect, swell, active_slot)
                    } else {
                        let cell_w = track.w / count as f32;
                        let lens_rect = Box4::new(
                            track.x + selected as f32 * cell_w + 2.0,
                            track.y + 3.0,
                            (cell_w - 4.0).max(16.0),
                            (track.h - 6.0).max(16.0),
                        );
                        (track, lens_rect, 0.0, selected)
                    };
                segmented.insert(
                    item.key,
                    SegmentedPresentation {
                        track_rect: track,
                        well_rect,
                        lens_rect,
                        swell,
                        active_slot,
                        glass: None,
                    },
                );
            }
        }
        Presentation {
            kind: self.kind,
            pages: self.pages(),
            editing_key: self.editing_key,
            editing_draft: self.editing_draft.clone(),
            caret_visible: (self.editing_key.is_some() || self.modal.is_some())
                && ((self.last_tick_ms.unwrap_or(0.0) / 530.0) as u64).is_multiple_of(2),
            rect,
            page: self.page,
            title: self.title,
            rows,
            outgoing_rows: self
                .outgoing_page
                .filter(|_| self.page_anim < 1.0)
                .map(|prev| self.rows_for(prev, rect, self.outgoing_scroll))
                .unwrap_or_default(),
            page_progress: self.page_anim,
            page_dir: self.page_dir,
            values,
            status: self.status.clone(),
            scroll: self.scroll,
            max_scroll: self.max_scroll(rect),
            tab_well_rect,
            tab_lens_rect,
            tab_swell: self.tab_lens.swell(),
            tab_lens_glass: None,
            close_lens: None,
            dropdown: self.dropdown_layout(rect),
            segmented,
            accent_hex,
            switch_fractions,
            active_player_bucket: self.active_player_bucket(),
            active_profile: self.active_profile(),
            active_mode_label: self.display_mode_label.unwrap_or_else(|| match self.active_profile() {
                "nocover" => "无封面模式",
                "mini" => "迷你模式",
                _ => "窗口模式",
            }),
            artist_aliases: self.artist_aliases(),
            custom_player_offsets: self
                .saved_player_offsets()
                .into_iter()
                .map(|b| {
                    let val = self
                        .config
                        .pointer(&format!("/lyrics/playerSyncOffsets/{b}"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    (b, val)
                })
                .collect(),
            custom_models: {
                let provider = self.value("lyrics.aiPreset").as_str().unwrap_or("deepseek").to_owned();
                self.custom_models_for(&provider)
            },
            modal: self.modal.clone(),
            modal_offset: [0.0, 0.0],
            modal_glass: None,
            theme_mode: Display::from_config(&self.config).theme_mode,
            status_alpha: self.status_alpha,
            reset_armed: self
                .reset_confirm_until_ms
                .is_some_and(|t| self.last_tick_ms.unwrap_or(0.0) <= t),
            dragging_source_row: self.source_drag,
            source_drag_swell: self.source_drag_swell,
            source_drag_y: self.source_drag_y,
            source_row_offsets: self
                .source_row_offsets
                .iter()
                .map(|(&k, &(pos, _))| (k, pos))
                .collect(),
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub fn paint(
    window: &mut gpui_kit::Window,
    cx: &gpui_kit::App,
    p: &Presentation,
    opacity: f32,
    feedback: impl Fn(u8) -> crate::player::interaction::Feedback,
    choice_feedback: impl Fn(u8) -> crate::player::interaction::Feedback,
    page_feedback: impl Fn(u8) -> crate::player::interaction::Feedback,
    dropdown_feedback: impl Fn(u8) -> crate::player::interaction::Feedback,
    close: crate::player::interaction::Feedback,
    modal_btns: [crate::player::interaction::Feedback; 2],
) {
    use crate::player::chrome;
    let r = p.rect;
    let c = Settings::content_for(p.kind, r);
    let color = |a| chrome::text(a * opacity);
    let close_box = Settings::close_rect_for(p.kind, r);
    let sw = Settings::sidebar_width_for(p.kind, r);
    if p.kind == SettingsKind::Application {
        let header_cy = close_box.centre().1;
        chrome::fill_round(window, r.x + sw - 1.0, r.y + 14.0, 0.85, (r.h - 28.0).max(16.0), 0.4, color(0.08));
        chrome::paint_text_vcentred(window, p.title, r.x + 18., header_cy, 13., 600., color(0.55));
        let page_label = p.pages.get(p.page).copied().unwrap_or(p.title);
        chrome::paint_text_vcentred(window, page_label, c.x + 2., header_cy, 17., 700., color(0.96));
        if matches!(p.page, 1 | 2) {
            let profile_label = p.active_mode_label;
            let title_w = chrome::measure(window, page_label, 17., 700.).0;
            let badge_w = chrome::measure(window, profile_label, 11., 550.).0 + 14.0;
            let bx = c.x + 2. + title_w + 10.;
            if bx + badge_w < close_box.x - 8.0 {
                let badge_r = Box4::new(bx, header_cy - 10.0, badge_w, 20.0);
                chrome::fill_round(window, badge_r.x, badge_r.y, badge_r.w, badge_r.h, 6., color(0.075));
                chrome::paint_text_box_centred(window, profile_label, badge_r, 11., 550., color(0.66));
            }
        }
    } else {
        chrome::paint_text_vcentred(window, p.title, r.x + 18., close_box.centre().1, 17., 650., color(0.96));
    }
    if let Some((grect, surface, snapshot, hook)) = p.close_lens {
        window.paint_custom_surface_with_scene(
            gpui_kit::Bounds {
                origin: gpui_kit::point(gpui_kit::px(grect.left as f32), gpui_kit::px(grect.top as f32)),
                size: gpui_kit::size(gpui_kit::px(grect.width as f32), gpui_kit::px(grect.height as f32)),
            },
            surface,
            snapshot,
            hook,
        );
    }
    if p.close_lens.is_some() {
        let press = close.press.clamp(0.0, 1.0);
        let scale_btn = (0.90 + 0.14 * press).max(0.85);
        let lw = close_box.w * scale_btn * (1.0 + close.squeeze[0] * 0.72);
        let lh = close_box.h * scale_btn * (1.0 + close.squeeze[1] * 0.72);
        let cx_b = close_box.x + close_box.w * 0.5 + close.pull[0] * 0.55;
        let cy_b = close_box.y + close_box.h * 0.5 + close.pull[1] * 0.55;
        let lens_box = Box4::new(cx_b - lw * 0.5, cy_b - lh * 0.5, lw, lh);
        chrome::paint_segmented_lens(
            window,
            lens_box,
            lw.min(lh) * 0.5,
            (press * 0.85).clamp(0.0, 1.0),
            close,
            1.0,
        );
    }
    let icon_inset = if p.kind == SettingsKind::Application { 7. } else { 7. };
    let icon_sz = if p.kind == SettingsKind::Application { 14. } else { 16. };
    crate::player::icons::paint_elastic(
        window,
        cx,
        crate::player::icons::Icon::Close,
        Box4::new(close_box.x + icon_inset, close_box.y + icon_inset, icon_sz, icon_sz),
        color(0.75 + 0.22 * close.hover),
        close,
    );
    let swell = p.tab_swell.max(0.0);
    let is_app = p.kind == SettingsKind::Application;
    if !is_app {
        chrome::paint_segmented_well(
            window,
            p.tab_well_rect,
            11.0 + 1.5 * swell,
            opacity,
        );
    }
    let active_page_fb = page_feedback(p.page as u8);
    let lens_box = Box4::new(
        p.tab_lens_rect.x + active_page_fb.pull[0] * 0.35,
        p.tab_lens_rect.y + active_page_fb.pull[1] * 0.25,
        p.tab_lens_rect.w,
        p.tab_lens_rect.h,
    );
    let lens_radius = (lens_box.h * 0.5).max(if is_app { 10.0 } else { 9.5 } + 3.5 * swell);
    let (rest_alpha, _) = chrome::segmented_crossfade(swell);
    chrome::paint_segmented_rest_pill(window, lens_box, lens_radius, p.accent_hex, rest_alpha, opacity);
    let tab_font_sz = 13.0 * (1.0 + 0.08 * swell);
    let scaled_tab = |i: usize| {
        let b_base = Settings::tab_rect_for_kind(p.kind, r, i, p.pages.len());
        if is_app {
            let base_cy = r.y + 52.0 + (p.pages.len() as f32 * 38.0 - 4.0) * 0.5;
            let spread = p.tab_well_rect.h / (p.pages.len() as f32 * 38.0).max(1.0);
            let cy = base_cy + (b_base.centre().1 - base_cy) * spread;
            Box4::new(b_base.x, cy - b_base.h * 0.5, b_base.w, b_base.h)
        } else {
            let base_track_cx = r.x + r.w * 0.5;
            let spread = if r.w > 24.0 { p.tab_well_rect.w / (r.w - 20.0) } else { 1.0 };
            let cx = base_track_cx + (b_base.centre().0 - base_track_cx) * spread;
            Box4::new(cx - b_base.w * 0.5, b_base.y, b_base.w, b_base.h)
        }
    };
    for (i, label) in p.pages.iter().enumerate() {
        let tab = scaled_tab(i);
        let tf = page_feedback(i as u8);
        if p.page != i && (tf.hover > 0.01 || tf.press > 0.01) && swell < 0.1 {
            chrome::control_surface_alpha(window, tab, tf, false, opacity * 0.45);
        }
        let text_color = if p.page == i {
            chrome::segmented_active_text_blended(p.accent_hex, rest_alpha, 0.96 * opacity)
        } else {
            color(0.62 + 0.22 * tf.hover)
        };
        chrome::paint_text_box_centred(
            window,
            label,
            tab,
            tab_font_sz,
            if p.page == i { 650. } else { 550. },
            text_color,
        );
    }
    if swell > 0.005 {
        if let Some((grect, surface, snapshot, hook)) = p.tab_lens_glass {
            window.paint_custom_surface_with_scene(
                gpui_kit::Bounds {
                    origin: gpui_kit::point(gpui_kit::px(grect.left as f32), gpui_kit::px(grect.top as f32)),
                    size: gpui_kit::size(gpui_kit::px(grect.width as f32), gpui_kit::px(grect.height as f32)),
                },
                surface,
                snapshot,
                hook,
            );
        }
        chrome::paint_segmented_lens(window, lens_box, lens_radius, p.tab_swell, active_page_fb, opacity);
    }
    let page_mask = gpui_kit::ContentMask {
        bounds: gpui_kit::bounds(
            gpui_kit::point(gpui_kit::px(c.x - 4.0), gpui_kit::px(c.y)),
            gpui_kit::size(gpui_kit::px(c.w + 8.0), gpui_kit::px(c.h)),
        ),
    };
    if p.outgoing_rows.is_empty() || p.page_progress >= 1.0 {
        window.with_content_mask(Some(page_mask), |window| {
            paint_page_rows(
                window,
                p,
                &p.rows,
                0.0,
                0.0,
                opacity,
                &feedback,
                &choice_feedback,
            );
        });
    } else {
        let t = 1.0 - (1.0 - p.page_progress.clamp(0.0, 1.0)).powi(3);
        let dir = p.page_dir as f32;
        let (dx_out, dy_out, dx_in, dy_in) = if p.kind == SettingsKind::Application {
            let travel = (c.h * 0.14).clamp(28.0, 64.0);
            (0.0, -dir * travel * t, 0.0, dir * travel * (1.0 - t))
        } else {
            let travel = (c.w * 0.28).clamp(48.0, 110.0);
            (-dir * travel * t, 0.0, dir * travel * (1.0 - t), 0.0)
        };
        let out_alpha = opacity * (1.0 - t * 1.15).clamp(0.0, 1.0);
        let in_alpha = opacity * t.clamp(0.0, 1.0);
        window.with_content_mask(Some(page_mask), |window| {
            if out_alpha > 0.01 {
                let no_fb = |_: u8| crate::player::interaction::Feedback::default();
                paint_page_rows(
                    window,
                    p,
                    &p.outgoing_rows,
                    dx_out,
                    dy_out,
                    out_alpha,
                    &no_fb,
                    &no_fb,
                );
            }
            if in_alpha > 0.01 {
                paint_page_rows(
                    window,
                    p,
                    &p.rows,
                    dx_in,
                    dy_in,
                    in_alpha,
                    &feedback,
                    &choice_feedback,
                );
            }
        });
    }
    if p.max_scroll > 0.5 {
        let fraction = (p.scroll / p.max_scroll).clamp(0.0, 1.0);
        let track_y = c.y + 4.0;
        let track_h = (c.h - 8.0).max(20.0);
        let thumb = (track_h * track_h / (track_h + p.max_scroll)).clamp(24.0, track_h);
        chrome::fill_round(
            window,
            r.right() - 7.0,
            track_y + fraction * (track_h - thumb),
            2.4,
            thumb,
            1.2,
            color(0.24),
        );
    }
    if !p.status.is_empty() && p.status_alpha > 0.005 {
        let ta = (p.status_alpha * opacity).clamp(0.0, 1.0);
        let max_tw = (r.w - 48.0).max(120.0);
        let tw = chrome::measure(window, p.status.as_str(), 12.0, 560.0).0;
        let toast_w = (tw + 28.0).min(max_tw);
        let toast_h = 30.0;
        let toast_r = Box4::new(
            r.x + (r.w - toast_w) * 0.5,
            r.bottom() - toast_h - 12.0,
            toast_w,
            toast_h,
        );
        chrome::elevation(window, toast_r, 15.0, 0.26 * ta);
        let (bg, border) = match p.theme_mode {
            crate::player::model::ThemeMode::Light => (
                gpui_kit::Hsla::from(gpui_kit::rgb(0xFF_FF_FF)).opacity(0.90 * ta),
                chrome::themed_border(0.14 * ta),
            ),
            crate::player::model::ThemeMode::Dark => (
                gpui_kit::Hsla::from(gpui_kit::rgb(0x12_14_19)).opacity(0.92 * ta),
                chrome::themed_border(0.20 * ta),
            ),
            crate::player::model::ThemeMode::LiquidGlass => (
                gpui_kit::Hsla::from(gpui_kit::rgb(0x1C_1E_24)).opacity(0.68 * ta),
                chrome::themed_border(0.26 * ta),
            ),
        };
        chrome::fill_round(window, toast_r.x, toast_r.y, toast_r.w, toast_r.h, 15.0, bg);
        chrome::stroke_round(window, toast_r.x, toast_r.y, toast_r.w, toast_r.h, 15.0, 0.85, border);
        chrome::paint_text_fitted_vcentred(
            window,
            p.status.as_str(),
            toast_r.x + 14.0,
            toast_r.centre().1,
            (toast_r.w - 28.0).max(40.0),
            12.0,
            560.0,
            chrome::text(0.94 * ta),
        );
    }
    if let Some(d) = &p.dropdown {
        // A full shadow under a translucent list also darkens its interior.
        // Keep list glass at the same tint regardless of its height or option type.
        if let Some((grect, surface, snapshot, hook)) = d.glass {
            window.paint_custom_surface_with_scene(
                gpui_kit::Bounds {
                    origin: gpui_kit::point(gpui_kit::px(grect.left as f32), gpui_kit::px(grect.top as f32)),
                    size: gpui_kit::size(gpui_kit::px(grect.width as f32), gpui_kit::px(grect.height as f32)),
                },
                surface,
                snapshot,
                hook,
            );
        }
        let fallback_alpha = if d.glass.is_none() {
            Some(0.94)
        } else {
            match p.theme_mode {
                crate::player::model::ThemeMode::Light => Some(0.86),
                crate::player::model::ThemeMode::Dark => Some(0.55),
                crate::player::model::ThemeMode::LiquidGlass => None,
            }
        };
        if let Some(fa) = fallback_alpha {
            let tint = if p.theme_mode == crate::player::model::ThemeMode::Dark {
                gpui_kit::Hsla::from(gpui_kit::rgb(0x0B_0D_11)).opacity(fa * opacity)
            } else {
                chrome::themed_surface(fa * opacity)
            };
            chrome::fill_round(window, d.rect.x, d.rect.y, d.rect.w, d.rect.h, 12., tint);
        }
        window.paint_quad(gpui_kit::quad(
            gpui_kit::bounds(gpui_kit::point(gpui_kit::px(d.rect.x), gpui_kit::px(d.rect.y)), gpui_kit::size(gpui_kit::px(d.rect.w), gpui_kit::px(d.rect.h))),
            gpui_kit::Corners::all(gpui_kit::px(12.0)),
            gpui_kit::transparent_black(),
            gpui_kit::Edges::all(gpui_kit::px(0.85)),
            color(0.22),
            gpui_kit::BorderStyle::Solid,
        ));
        let inner_mask = gpui_kit::ContentMask {
            bounds: gpui_kit::Bounds {
                origin: gpui_kit::point(gpui_kit::px(d.rect.x + 2.0), gpui_kit::px(d.rect.y + 4.0)),
                size: gpui_kit::size(gpui_kit::px((d.rect.w - 4.0).max(1.0)), gpui_kit::px((d.rect.h - 8.0).max(1.0))),
            },
        };
        window.with_content_mask(Some(inner_mask), |window| {
            chrome::fill_round(window,d.lens_rect.x,d.lens_rect.y,d.lens_rect.w,d.lens_rect.h,9.,color(0.14));
            for (opt_i, key, label, opt_r, selected, _is_custom) in &d.options {
                if opt_r.bottom() < d.rect.y || opt_r.y > d.rect.bottom() {
                    continue;
                }
                let swatch_hex = if d.is_color {
                    key.strip_prefix('#').and_then(|h| u32::from_str_radix(h, 16).ok())
                } else {
                    None
                };
                chrome::paint_list_item(
                    window,
                    *opt_r,
                    label,
                    *selected,
                    swatch_hex,
                    p.accent_hex,
                    dropdown_feedback(*opt_i as u8),
                    opacity,
                );
            }
        });
        if d.max_scroll > 0.5 {
            let track_y = d.rect.y + 6.0;
            let track_h = (d.rect.h - 12.0).max(16.0);
            let thumb_h = (track_h * track_h / (track_h + d.max_scroll)).clamp(18.0, track_h);
            let frac = (d.scroll / d.max_scroll).clamp(0.0, 1.0);
            chrome::fill_round(
                window,
                d.rect.right() - 6.0,
                track_y + frac * (track_h - thumb_h),
                2.2,
                thumb_h,
                1.1,
                color(0.26),
            );
        }
    }
    if let Some(modal) = &p.modal {
        let (box_r, f1, f2, cancel, save) = Settings::modal_rects_with_offset(r, modal, p.modal_offset);
        if let Some((grect, surface, snapshot, hook)) = p.modal_glass {
            window.paint_custom_surface_with_scene(
                gpui_kit::Bounds {
                    origin: gpui_kit::point(gpui_kit::px(grect.left as f32), gpui_kit::px(grect.top as f32)),
                    size: gpui_kit::size(gpui_kit::px(grect.width as f32), gpui_kit::px(grect.height as f32)),
                },
                surface,
                snapshot,
                hook,
            );
        }
        let fallback_alpha = if p.modal_glass.is_none() {
            Some(0.94)
        } else {
            match p.theme_mode {
                crate::player::model::ThemeMode::Light => Some(0.86),
                crate::player::model::ThemeMode::Dark => Some(0.55),
                crate::player::model::ThemeMode::LiquidGlass => None,
            }
        };
        if let Some(fa) = fallback_alpha {
            let tint = if p.theme_mode == crate::player::model::ThemeMode::Dark {
                gpui_kit::Hsla::from(gpui_kit::rgb(0x0B_0D_11)).opacity(fa * opacity)
            } else {
                chrome::themed_surface(fa * opacity)
            };
            chrome::fill_round(window, box_r.x, box_r.y, box_r.w, box_r.h, 14.0, tint);
        }
        chrome::stroke_round(window, box_r.x, box_r.y, box_r.w, box_r.h, 14.0, 0.9, chrome::themed_border(0.22 * opacity));
        if matches!(modal, SettingsModal::PrivacyPolicy) {
            chrome::paint_text(window, "隐私条款", box_r.x + 18.0, box_r.y + 15.0, 15.0, 650.0, color(0.95));
            let clauses = [
                ("1. 零遥测与本地存储", "本软件不含任何自有统计、埋点或数据上报服务器。所有设置、歌词缓存、偏移记录、自定义歌词及翻译 API Key 仅存储在本地（AppData\\Roaming\\OverLyrics）。"),
                ("2. 媒体信息与本地音频分析", "通过 Windows SMTC 读取当前播放曲目与进度，仅用于本地展示与匹配歌词；开启节拍律动时仅在内存中通过 WASAPI 环回分析实时振幅，不录音、不落盘、不上传。"),
                ("3. 第三方网络服务", "检索歌词与封面时会向您启用的公开词源（酷狗、网易云、QQ 音乐、LRCLIB、AMLL 等）发送曲目与歌手信息；启用翻译时会向您选定的翻译服务发送歌词文本。"),
                ("4. 控制权与开源透明", "您可随时在设置中关闭词源、清理缓存或删除本地配置目录。本项目完全开源，所有行为公开可审计。"),
            ];
            let max_w = (box_r.w - 36.0).max(120.0);
            let max_y = cancel.y - 10.0;
            let mut cy = box_r.y + 44.0;
            for (heading, body) in clauses {
                if cy + 14.0 > max_y { break; }
                chrome::paint_text(window, heading, box_r.x + 18.0, cy, 12.0, 650.0, color(0.92));
                cy += 17.0;
                for line in wrap_modal_lines(window, body, max_w, 11.2, 450.0) {
                    if cy + 13.0 > max_y { break; }
                    chrome::paint_text(window, &line, box_r.x + 18.0, cy, 11.2, 450.0, color(0.72));
                    cy += 15.0;
                }
                cy += 6.0;
            }
            chrome::paint_glass_button(window, cancel, "关闭", p.accent_hex, true, false, modal_btns[0], opacity);
            return;
        }
        let (title, l1, ph1, v1, f1_active, l2, ph2, v2, f2_active) = match modal {
            SettingsModal::CustomModel { draft, .. } => (
                "添加自定义模型",
                "模型 API 名称",
                "例如 gpt-5-mini",
                draft.as_str(),
                true,
                "",
                "",
                "",
                false,
            ),
            SettingsModal::ArtistAlias { from, to, focus } => (
                "添加歌手名映射",
                "原始歌手名（播放器显示）",
                "例如 Yanzi Sun",
                from.as_str(),
                *focus == 0,
                "目标歌手名（用于匹配歌词）",
                "例如 孙燕姿",
                to.as_str(),
                *focus == 1,
            ),
            SettingsModal::PrivacyPolicy => unreachable!(),
        };
        chrome::paint_text(window, title, box_r.x + 16.0, box_r.y + 15.0, 14.5, 650.0, color(0.95));
        chrome::paint_text(window, l1, f1.x + 2.0, f1.y - 16.5, 11.5, 580.0, color(0.62));
        chrome::paint_glass_input(window, f1, ph1, v1, false, f1_active, p.caret_visible, p.accent_hex, opacity);
        if let Some(f2_rect) = f2 {
            chrome::paint_text(window, l2, f2_rect.x + 2.0, f2_rect.y - 16.5, 11.5, 580.0, color(0.62));
            chrome::paint_glass_input(window, f2_rect, ph2, v2, false, f2_active, p.caret_visible, p.accent_hex, opacity);
        }
        chrome::paint_glass_button(window, cancel, "取消", p.accent_hex, false, false, modal_btns[0], opacity);
        chrome::paint_glass_button(window, save, "保存", p.accent_hex, true, false, modal_btns[1], opacity);
    }
}

fn wrap_modal_lines(window: &mut gpui_kit::Window, text: &str, max_w: f32, size: f32, weight: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if crate::player::chrome::measure(window, &cur, size, weight).0 > max_w && cur.chars().count() > 1 {
            let last = cur.pop().unwrap();
            lines.push(cur);
            cur = String::from(last);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn paint_page_rows(
    window: &mut gpui_kit::Window,
    p: &Presentation,
    rows: &[(usize, Item, Box4)],
    dx: f32,
    dy: f32,
    opacity: f32,
    feedback: &impl Fn(u8) -> crate::player::interaction::Feedback,
    choice_feedback: &impl Fn(u8) -> crate::player::interaction::Feedback,
) {
    use crate::player::chrome;
    let color = |a| chrome::text(a * opacity);
    let mut section_starts: Vec<usize> = Vec::new();
    let mut has_sources = false;
    let mut has_sync = false;
    let mut has_alias = false;
    for (idx, (_, item, _)) in rows.iter().enumerate() {
        let label = if item.key.starts_with("lyrics.enabledSources.") {
            if !has_sources {
                has_sources = true;
                Some("歌词来源")
            } else {
                None
            }
        } else if matches!(item.key, "lyrics.playerSyncOffsets.apple")
            || item.key == ACTIVE_PLAYER_OFFSET_KEY
            || item.key == CUSTOM_PLAYER_OFFSET_KEY
        {
            if !has_sync {
                has_sync = true;
                Some("播放器同步")
            } else {
                None
            }
        } else if item.key == "lyrics.artistAliases" {
            if !has_alias {
                has_alias = true;
                Some("歌手名映射")
            } else {
                None
            }
        } else if item.key == "lyrics.addArtistAlias" && !has_alias {
            has_alias = true;
            Some("歌手名映射")
        } else {
            Settings::section_label(item.key)
        };
        if idx == 0 || label.is_some() {
            section_starts.push(idx);
        }
    }
    for (g_idx, &start) in section_starts.iter().enumerate() {
        let end = section_starts.get(g_idx + 1).copied().unwrap_or(rows.len());
        let (_, first_item, first) = &rows[start];
        let (_, _, last) = &rows[end - 1];
        let header = if first_item.key.starts_with("lyrics.enabledSources.") {
            Some("歌词来源")
        } else if first_item.key == ACTIVE_PLAYER_OFFSET_KEY || first_item.key == CUSTOM_PLAYER_OFFSET_KEY {
            Some("播放器同步")
        } else if matches!(first_item.key, "lyrics.artistAliases" | "lyrics.addArtistAlias") {
            Some("歌手名映射")
        } else {
            Settings::section_label(first_item.key)
        };
        if let Some(title) = header {
            chrome::paint_text(
                window,
                title,
                first.x + dx + 6.0,
                first.y + dy - 20.0,
                11.0,
                600.0,
                color(0.48),
            );
        }
        let group_box = Box4::new(
            first.x + dx - 2.0,
            first.y + dy - 3.0,
            first.w + 4.0,
            (last.bottom() - first.y) + 6.0,
        );
        if chrome::is_light_theme() {
            chrome::fill_round(
                window,
                group_box.x,
                group_box.y,
                group_box.w,
                group_box.h,
                12.0,
                gpui_kit::Hsla::from(gpui_kit::rgb(0xFF_FF_FF)).opacity(0.76 * opacity),
            );
            chrome::stroke_round(
                window,
                group_box.x,
                group_box.y,
                group_box.w,
                group_box.h,
                12.0,
                0.75,
                chrome::themed_border(0.08 * opacity),
            );
        } else {
            chrome::fill_round(window, group_box.x, group_box.y, group_box.w, group_box.h, 12.0, color(0.045));
            chrome::stroke_round(window, group_box.x, group_box.y, group_box.w, group_box.h, 12.0, 0.75, color(0.08));
        }
    }
    for (row_pos, (i, item, raw_row)) in rows.iter().enumerate() {
        let row = Box4::new(raw_row.x + dx, raw_row.y + dy, raw_row.w, raw_row.h);
        if row_pos > 0 && !section_starts.contains(&row_pos) {
            chrome::fill_round(window, row.x + 8.0, row.y - 2.0, row.w - 16.0, 0.75, 0.35, color(0.06));
        }
        let value = &p.values[item.key];
        match item.control {
            Control::Slider { min, max, .. } => {
                let label: std::borrow::Cow<'_, str> = if item.key == ACTIVE_PLAYER_OFFSET_KEY {
                    std::borrow::Cow::Owned(format!(
                        "{} 同步偏移",
                        oly_core::mapping::player_display_name(&p.active_player_bucket)
                    ))
                } else {
                    std::borrow::Cow::Borrowed(item.label)
                };
                let slider_top_cy = row.y + 11.0;
                chrome::paint_text_vcentred(
                    window,
                    &label,
                    row.x + 8.,
                    slider_top_cy,
                    12.5,
                    500.,
                    color(0.82),
                );
                let v = value.as_f64().unwrap_or(min);
                let text = if item.key.starts_with("lyrics.playerSyncOffsets.") {
                    let iv = v.round() as i64;
                    if iv == 0 {
                        "0 ms".to_string()
                    } else {
                        format!("{iv:+} ms")
                    }
                } else if item.key == "letterSpacing" {
                    format!("{v:.3} em")
                } else if matches!(item.key, "lineGap" | "innerGap" | "linePaddingX" | "lineRadius") {
                    format!("{v:.2} em")
                } else if matches!(item.key, "coverRadius" | "cornerRadius" | "edgeFeather") {
                    format!("{v:.0} px")
                } else if matches!(item.key, "motionCoverCacheMB" | "lyrics.lyricsCacheMB") {
                    format!("{v:.0} MB")
                } else if matches!(
                    item.key,
                    "fontSize"
                        | "transFontSize"
                        | "playingLineTopOffset"
                        | "romanFontScale"
                        | "bgLineScale"
                        | "idleLineOpacity"
                        | "unplayedOriginalOpacity"
                        | "brightAlpha"
                        | "translatedLyricsOpacity"
                        | "transCurrentAlpha"
                        | "bgLineOpacity"
                        | "phoneticLyricsOpacity"
                        | "blurStrength"
                        | "lyrics.matchThreshold"
                ) {
                    format!("{v:.0}%")
                } else if matches!(
                    item.key,
                    "beatFlashThreshold"
                        | "beatFlashStrength"
                        | "beatWobbleThreshold"
                        | "beatWobbleStrength"
                ) {
                    format!("{v:.2}x")
                } else if max <= 4. {
                    format!("{v:.2}")
                } else {
                    format!("{v:.0}")
                };
                let vw = chrome::measure(window, &text, 12., 550.).0;
                chrome::paint_text_vcentred(
                    window,
                    &text,
                    (row.right() - 8. - vw).max(row.x + 120.),
                    slider_top_cy,
                    12.,
                    550.,
                    color(0.9),
                );
                chrome::paint_slider(
                    window,
                    row.x + 8.,
                    row.y + 34.,
                    row.w - 16.,
                    ((v - min) / (max - min)) as f32,
                    chrome::accent_rgb(p.accent_hex, 0.9 * opacity),
                    color(0.18),
                    feedback(*i as u8),
                    opacity,
                );
            }
            Control::PlayerOffset(idx) => {
                if let Some((bucket, v)) = p.custom_player_offsets.get(idx) {
                    let label = format!(
                        "{} 同步偏移",
                        oly_core::mapping::player_display_name(bucket)
                    );
                    let slider_top_cy = row.y + 11.0;
                    chrome::paint_text_vcentred(
                        window,
                        &label,
                        row.x + 8.,
                        slider_top_cy,
                        12.5,
                        500.,
                        color(0.82),
                    );
                    let iv = v.round() as i64;
                    let text = if iv == 0 {
                        "0 ms".to_string()
                    } else {
                        format!("{iv:+} ms")
                    };
                    let vw = chrome::measure(window, &text, 12., 550.).0;
                    chrome::paint_text_vcentred(
                        window,
                        &text,
                        (row.right() - 8. - vw).max(row.x + 120.),
                        slider_top_cy,
                        12.,
                        550.,
                        color(0.9),
                    );
                    chrome::paint_slider(
                        window,
                        row.x + 8.,
                        row.y + 34.,
                        row.w - 16.,
                        ((*v - (-2000.0)) / 4000.0) as f32,
                        chrome::accent_rgb(p.accent_hex, 0.9 * opacity),
                        color(0.18),
                        feedback(*i as u8),
                        opacity,
                    );
                }
            }
            Control::Switch => {
                let is_source_row = item.key.starts_with("lyrics.enabledSources.");
                let is_dragging = is_source_row && p.dragging_source_row == Some(*i);
                if is_dragging {
                    // Paint the dragged source row in a second pass on top of sliding neighbors.
                    continue;
                }
                let row_dy = if is_source_row {
                    p.source_row_offsets.get(item.key).copied().unwrap_or(0.0)
                } else {
                    0.0
                };
                let draw_row = Box4::new(row.x, row.y + row_dy, row.w, row.h);
                let track = if is_source_row {
                    Settings::source_switch_track(draw_row)
                } else {
                    crate::player::details::switch_track(draw_row)
                };
                let grip_x = track.x - 24.0;
                let label_right = if is_source_row { grip_x - 10.0 } else { track.x - 8.0 };
                let label_w = (label_right - draw_row.x - 8.0).max(40.0);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    draw_row.x + 8.,
                    draw_row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                if is_source_row {
                    let cy = draw_row.centre().1;
                    let grip_color = color(0.34);
                    for dy_bar in [-4.2f32, 0.0, 4.2] {
                        chrome::fill_round(window, grip_x, cy + dy_bar - 0.75, 12.5, 1.5, 0.75, grip_color);
                    }
                }
                let on = value.as_bool().unwrap_or(false);
                let frac = p
                    .switch_fractions
                    .get(item.key)
                    .copied()
                    .unwrap_or(if on { 1.0 } else { 0.0 });
                let sf = choice_feedback(*i as u8);
                chrome::paint_switch_with_accent(
                    window,
                    track.x,
                    track.y,
                    track.w,
                    track.h,
                    on,
                    frac,
                    sf,
                    p.accent_hex,
                    opacity,
                );
            }
            Control::Text { secret } => {
                let field = Settings::choice_pill_rect(row);
                let label_w = (field.x - row.x - 16.).max(40.);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                let active = p.editing_key == Some(item.key);
                let raw = if active { p.editing_draft.as_str() } else { value.as_str().unwrap_or_default() };
                let placeholder = if active { "输入后按回车确认" } else { "点击输入" };
                chrome::paint_glass_input(
                    window,
                    field,
                    placeholder,
                    raw,
                    secret,
                    active,
                    p.caret_visible,
                    p.accent_hex,
                    opacity,
                );
            }
            Control::Segmented(options) => {
                let count = options.len().max(1);
                let fallback_track = Settings::segmented_track_rect(*raw_row, count);
                let seg = p.segmented.get(item.key);
                let base_track = seg.map(|s| s.track_rect).unwrap_or(fallback_track);
                let well = seg.map(|s| s.well_rect).unwrap_or(base_track);
                let lens = seg.map(|s| s.lens_rect).unwrap_or(base_track);
                let swell = seg.map(|s| s.swell).unwrap_or(0.0).max(0.0);
                let active_slot = seg.map(|s| s.active_slot).unwrap_or(0);
                let fb = choice_feedback(*i as u8);
                let track = Box4::new(base_track.x + dx, base_track.y + dy, base_track.w, base_track.h);
                let well_d = Box4::new(well.x + dx, well.y + dy, well.w, well.h);
                let lens_d = Box4::new(
                    lens.x + dx + fb.pull[0] * 0.35,
                    lens.y + dy + fb.pull[1] * 0.25,
                    lens.w,
                    lens.h,
                );
                let label_w = (track.x - row.x - 16.).max(40.);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                chrome::paint_segmented_well(window, well_d, well_d.h * 0.5, opacity);
                let pill_radius = lens_d.h * 0.5;
                let (rest_alpha, _) = chrome::segmented_crossfade(swell);
                chrome::paint_segmented_rest_pill(
                    window,
                    lens_d,
                    pill_radius,
                    p.accent_hex,
                    rest_alpha,
                    opacity,
                );
                let cell_w = track.w / count as f32;
                let base_cx = track.centre().0;
                let spread = if track.w > 2.0 { well_d.w / track.w } else { 1.0 };
                let font_sz = 11.5 * (1.0 + 0.08 * swell);
                for (slot_i, (_, opt_label)) in options.iter().enumerate() {
                    let slot_cx = base_cx + ((track.x + (slot_i as f32 + 0.5) * cell_w) - base_cx) * spread;
                    let sb = Box4::new(slot_cx - cell_w * 0.5, track.y, cell_w, track.h);
                    let selected = slot_i == active_slot;
                    chrome::paint_text_box_centred(
                        window,
                        opt_label,
                        sb,
                        font_sz,
                        if selected { 650.0 } else { 520.0 },
                        if selected {
                            chrome::segmented_active_text_blended(p.accent_hex, rest_alpha, 0.96 * opacity)
                        } else {
                            color(0.64 + 0.18 * fb.hover)
                        },
                    );
                }
                if dx.abs() < 0.5 && dy.abs() < 0.5 && swell > 0.005 {
                    if let Some((grect, surface, snapshot, hook)) = seg.and_then(|s| s.glass) {
                        window.paint_custom_surface_with_scene(
                            gpui_kit::Bounds {
                                origin: gpui_kit::point(
                                    gpui_kit::px(grect.left as f32),
                                    gpui_kit::px(grect.top as f32),
                                ),
                                size: gpui_kit::size(
                                    gpui_kit::px(grect.width as f32),
                                    gpui_kit::px(grect.height as f32),
                                ),
                            },
                            surface,
                            snapshot,
                            hook,
                        );
                    }
                    chrome::paint_segmented_lens(window, lens_d, pill_radius, swell, fb, opacity);
                }
            }
            Control::Choice(options) => {
                let pill = Settings::choice_pill_rect(row);
                let label_w = (pill.x - row.x - 16.).max(40.);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                let label = options
                    .iter()
                    .find(|(k, _)| Some(*k) == value.as_str())
                    .map(|(_, v)| *v)
                    .or_else(|| {
                        if item.key == "lyrics.aiModel" {
                            value
                                .as_str()
                                .and_then(|cur| p.custom_models.iter().find(|m| m.as_str() == cur).map(String::as_str))
                        } else {
                            None
                        }
                    })
                    .unwrap_or("默认");
                let is_open = p.dropdown.as_ref().is_some_and(|d| d.row_index == *i);
                chrome::paint_choice_pill_state(
                    window,
                    pill,
                    label,
                    is_open,
                    p.accent_hex,
                    choice_feedback(*i as u8),
                    opacity,
                );
            }
            Control::Action { button, danger } => {
                let fb = choice_feedback(*i as u8);
                let dynamic_btn: Option<String> = if item.key == "action.checkUpdate" {
                    Some(crate::updater::global().button_label())
                } else {
                    None
                };
                let btn_text = if item.key == "action.resetDefaults" && p.reset_armed {
                    "再次点击确认"
                } else if let Some(ref dyn_label) = dynamic_btn {
                    dyn_label.as_str()
                } else {
                    button
                };
                let primary = item.key == "action.checkUpdate"
                    && crate::updater::global().is_update_available();
                let is_about_btn = matches!(
                    item.key,
                    "action.openWebsite"
                        | "action.openRepo"
                        | "action.checkUpdate"
                        | "action.privacyPolicy"
                );
                let btn_w = if is_about_btn {
                    64.0
                } else {
                    (chrome::measure(window, btn_text, 12.0, 580.0).0 + 24.0).clamp(64.0, 184.0)
                };
                let btn = Box4::new(row.right() - 8.0 - btn_w, row.y + (row.h - 28.0) * 0.5, btn_w, 28.0);
                let label_w = (btn.x - row.x - 16.0).max(40.0);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                chrome::paint_glass_button(
                    window,
                    btn,
                    btn_text,
                    p.accent_hex,
                    primary,
                    danger,
                    fb,
                    opacity,
                );
            }
            Control::Info(val) => {
                let dynamic_val = if item.key == "about.version" {
                    Some(crate::updater::global().version_badge(val))
                } else {
                    None
                };
                let val_str = dynamic_val.as_deref().unwrap_or(val);
                let val_w = chrome::measure(window, val_str, 12.5, 500.0).0;
                let label_w = (row.w - val_w - 28.0).max(40.0);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                chrome::paint_text_vcentred(
                    window,
                    val_str,
                    row.right() - 8. - val_w,
                    row.centre().1,
                    12.5,
                    500.,
                    color(0.60),
                );
            }
            Control::AliasRow(alias_idx) => {
                if let Some((from, to)) = p.artist_aliases.get(alias_idx) {
                    let fb = choice_feedback(*i as u8);
                    let del_w = 50.0;
                    let del_r = Box4::new(row.right() - 8.0 - del_w, row.y + (row.h - 26.0) * 0.5, del_w, 26.0);
                    let text_w = (del_r.x - row.x - 16.0).max(40.0);
                    let display = format!("{from}  →  {to}");
                    chrome::paint_text_fitted_vcentred(
                        window,
                        &display,
                        row.x + 8.0,
                        row.centre().1,
                        text_w,
                        12.5,
                        500.0,
                        color(0.88),
                    );
                    chrome::paint_glass_button(
                        window,
                        del_r,
                        "删除",
                        p.accent_hex,
                        false,
                        false,
                        fb,
                        opacity,
                    );
                }
            }
        }
    }
    if let Some(drag_idx) = p.dragging_source_row {
        if let Some((i, item, raw_row)) = rows
            .iter()
            .find(|(idx, item, _)| *idx == drag_idx && item.key.starts_with("lyrics.enabledSources."))
        {
            let swell = p.source_drag_swell.clamp(0.0, 1.0);
            let scale = 1.0 + 0.028 * swell;
            let expand_x = 4.0 * swell;
            let expand_y = 2.0 * swell;
            let base_row = Box4::new(
                raw_row.x + dx,
                raw_row.y + dy + p.source_drag_y,
                raw_row.w,
                raw_row.h,
            );
            let card = Box4::new(
                base_row.x - expand_x,
                base_row.y - expand_y,
                base_row.w + expand_x * 2.0,
                base_row.h + expand_y * 2.0,
            );
            chrome::elevation(window, card, 10.5, 0.14 * swell * opacity);
            let fill = if chrome::is_light_theme() {
                gpui_kit::Hsla::from(gpui_kit::rgb(0xFF_FF_FF))
                    .opacity((0.86 + 0.08 * swell) * opacity)
            } else {
                color(0.075 + 0.045 * swell)
            };
            chrome::fill_round(window, card.x, card.y, card.w, card.h, 10.5, fill);
            chrome::stroke_round(
                window,
                card.x,
                card.y,
                card.w,
                card.h,
                10.5,
                0.75,
                if chrome::is_light_theme() {
                    chrome::themed_border((0.10 + 0.06 * swell) * opacity)
                } else {
                    color(0.14 + 0.08 * swell)
                },
            );
            let base_track = Settings::source_switch_track(base_row);
            let sw = base_track.w * scale;
            let sh = base_track.h * scale;
            let track = Box4::new(
                base_track.centre().0 - sw * 0.5 + expand_x * 0.45,
                base_track.centre().1 - sh * 0.5,
                sw,
                sh,
            );
            let grip_w = 12.5 * scale;
            let grip_h = 1.5 * scale;
            let grip_gap = 4.2 * scale;
            let grip_x = track.x - 24.0 * scale;
            let label_x = base_row.x + 8.0 - expand_x * 0.35;
            let label_w = (grip_x - 10.0 - label_x).max(40.0);
            chrome::paint_text_fitted_vcentred(
                window,
                item.label,
                label_x,
                base_row.centre().1,
                label_w,
                12.5 * scale,
                550.,
                color(0.95),
            );
            let cy = base_row.centre().1;
            let grip_color = color(0.66 + 0.18 * swell);
            for dy_bar in [-grip_gap, 0.0, grip_gap] {
                chrome::fill_round(
                    window,
                    grip_x,
                    cy + dy_bar - grip_h * 0.5,
                    grip_w,
                    grip_h,
                    grip_h * 0.5,
                    grip_color,
                );
            }
            let value = &p.values[item.key];
            let on = value.as_bool().unwrap_or(false);
            let frac = p
                .switch_fractions
                .get(item.key)
                .copied()
                .unwrap_or(if on { 1.0 } else { 0.0 });
            let sf = choice_feedback(*i as u8);
            chrome::paint_switch_with_accent(
                window,
                track.x,
                track.y,
                track.w,
                track.h,
                on,
                frac,
                sf,
                p.accent_hex,
                opacity,
            );
        }
    }
}
