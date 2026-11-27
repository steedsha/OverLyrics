use super::*;

/// Independent Liquid Glass Volume popover window — reuses `paint_volume_panel_contents` directly!
pub(in crate::player::desktop_lyrics) struct DesktopVolumePopover {
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
    pub(in crate::player::desktop_lyrics) fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
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

