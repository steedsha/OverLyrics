use super::*;

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
pub(in crate::player::desktop_lyrics) struct DesktopColorPopover {
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
    pub(in crate::player::desktop_lyrics) fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
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
            self.bridge.push_popover(PopoverAction::DismissCascadeIfInactive);
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

