use super::*;

pub(crate) const VOLUME_POPOVER_W: f32 = 240.0 + POPOVER_ELASTIC_PAD * 2.0;
pub(crate) const VOLUME_POPOVER_H: f32 = 96.0 + POPOVER_ELASTIC_PAD * 2.0;

/// Independent Liquid Glass Volume popover window — reuses `paint_volume_panel_contents`
/// with elastic panel dragging inside outer padding (matching More popover & Close dialog).
pub(crate) struct DesktopVolumePopover {
    bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    track_fb: Feedback,
    cursor: Option<(f32, f32)>,
    dragging: bool,
    drag_pct: Option<f64>,
    panel_dragging: bool,
    press_gesture: PressGesture,
    panel_elastic: Elastic,
    was_active: bool,
    configured: bool,
    started: Instant,
    last_ms: f64,
    card: Box4,
    track: Box4,
}

impl DesktopVolumePopover {
    pub(crate) fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
        Self {
            bridge,
            motion: PanelMotion::default(),
            track_fb: Feedback::default(),
            cursor: None,
            dragging: false,
            drag_pct: None,
            panel_dragging: false,
            press_gesture: PressGesture::default(),
            panel_elastic: Elastic::default(),
            was_active: false,
            configured: false,
            started: Instant::now(),
            last_ms: 0.0,
            card: Box4::new(POPOVER_ELASTIC_PAD, POPOVER_ELASTIC_PAD, 240.0, 96.0),
            track: Box4::new(POPOVER_ELASTIC_PAD + 14.0, POPOVER_ELASTIC_PAD + 50.0, 212.0, 24.0),
        }
    }

    fn apply_x(&mut self, x: f32) {
        let f = ((x - self.track.x) / self.track.w.max(1.0)).clamp(0.0, 1.0) as f64;
        let pct = f * 100.0;
        self.drag_pct = Some(pct);
        self.bridge.push(DesktopCommand::SetVolumePct(pct));
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
        } else if self.was_active && !self.dragging && !self.panel_dragging {
            self.bridge.mark_volume_closed();
            window.remove_window();
            return div().into_any_element();
        }
        if !win32_window::is_left_mouse_down() {
            if self.dragging || self.panel_dragging {
                self.dragging = false;
                self.drag_pct = None;
                self.panel_dragging = false;
                self.press_gesture.cancel();
            }
        } else if self.dragging || self.panel_dragging {
            if let Some((cx_p, cy_p)) = win32_window::cursor_screen_pos() {
                let sf = window.scale_factor().max(0.5);
                let wb = window.bounds();
                let lx = (cx_p as f32 / sf) - f32::from(wb.origin.x);
                let ly = (cy_p as f32 / sf) - f32::from(wb.origin.y);
                self.cursor = Some((lx, ly));
                if self.dragging {
                    self.apply_x(lx);
                } else {
                    self.press_gesture.update(lx, ly);
                }
            }
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;
        self.motion.step(true, dt);
        self.panel_elastic
            .step(self.press_gesture.delta(), self.panel_dragging, dt);
        let pp = self.panel_elastic.pull();

        let snap = self.bridge.read_snapshot();
        let vp = window.viewport_size();
        let w = f32::from(vp.width).max(1.0);
        let h = f32::from(vp.height).max(1.0);
        let pad = POPOVER_ELASTIC_PAD;
        let card_w = (w - pad * 2.0).max(120.0);
        let card_h = (h - pad * 2.0).max(60.0);
        let card = Box4::new(pad + pp[0], pad + pp[1], card_w, card_h);
        let track = Box4::new(card.x + 14.0, card.y + 50.0, (card.w - 28.0).max(60.0), 24.0);
        self.card = card;
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
            left: card.x as f64,
            top: card.y as f64,
            width: card.w as f64,
            height: card.h as f64,
        };
        let display_pct = self.drag_pct.unwrap_or(snap.volume_pct);
        let display_status = if snap.volume_enabled {
            format!("{}%", display_pct.round() as i32)
        } else {
            snap.volume_status.clone()
        };

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.dragging {
                    this.apply_x(x);
                } else if this.panel_dragging {
                    this.press_gesture.update(x, y);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if this.track.contains(x, y) && this.bridge.read_snapshot().volume_enabled {
                        this.dragging = true;
                        this.apply_x(x);
                    } else if this.card.contains(x, y) {
                        this.panel_dragging = true;
                        this.press_gesture.begin(x, y);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.dragging = false;
                    this.drag_pct = None;
                    this.panel_dragging = false;
                    this.press_gesture.cancel();
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        chrome::set_light_theme(light_panel);
                        paint_sidecar_glass_frame(
                            window,
                            card,
                            panel_radius,
                            theme_mode,
                            surf_a,
                        );
                        paint_volume_panel_contents(
                            window,
                            panel_rect,
                            display_pct,
                            snap.volume_enabled,
                            display_status.as_str(),
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

