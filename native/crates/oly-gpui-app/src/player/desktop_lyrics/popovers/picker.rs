use super::*;

/// Independent Liquid Glass Lyrics Version Picker popover — reuses `paint_lyrics_picker_contents`!
pub(crate) struct DesktopLyricsPickerPopover {
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
    pub(crate) fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
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

