use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MoreHit {
    OrientH,
    OrientV,
    SyncMinus,
    SyncPlus,
    OpenDisplay,
    LyricsPicker,
    ReturnMain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DisplayHit {
    FontMinus,
    FontPlus,
    ColorPill,
    LinesPill,
    AutoScroll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::player::desktop_lyrics) enum PaletteDrag {
    Sv,
    Hue,
}

/// Level 1 independent Liquid Glass menu for Desktop Lyrics.
pub(in crate::player::desktop_lyrics) struct DesktopMorePopover {
    bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    orient_lens: SegmentedLens,
    orient_dragging: bool,
    orient_track: (f32, f32),
    orient_well: Box4,
    feedback: HashMap<MoreHit, Feedback>,
    elastics: HashMap<MoreHit, Elastic>,
    panel_elastic: Elastic,
    press_gesture: PressGesture,
    pressed_hit: Option<MoreHit>,
    panel_dragging: bool,
    card_rect: Box4,
    display_row_center_y: f32,
    was_active: bool,
    configured: bool,
    cursor: Option<(f32, f32)>,
    hits: Vec<(MoreHit, Box4)>,
    started: Instant,
    last_ms: f64,
}

impl DesktopMorePopover {
    pub(in crate::player::desktop_lyrics) fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
        Self {
            bridge,
            motion: PanelMotion::default(),
            orient_lens: SegmentedLens::default(),
            orient_dragging: false,
            orient_track: (POPOVER_ELASTIC_PAD + 10.0, 232.0),
            orient_well: Box4::default(),
            feedback: HashMap::new(),
            elastics: HashMap::new(),
            panel_elastic: Elastic::default(),
            press_gesture: PressGesture::default(),
            pressed_hit: None,
            panel_dragging: false,
            card_rect: Box4::default(),
            display_row_center_y: 0.0,
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
pub(in crate::player::desktop_lyrics) fn paint_hsv_palette(
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
        let child_open = self.bridge.display_popover_open.load(Ordering::SeqCst)
            || self.bridge.color_popover_open.load(Ordering::SeqCst)
            || self.bridge.lines_popover_open.load(Ordering::SeqCst);
        let display_open = self.bridge.display_popover_open.load(Ordering::SeqCst);
        if window.is_window_active() {
            self.was_active = true;
        } else if self.was_active && !self.orient_dragging && !self.panel_dragging && !child_open {
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

        let row_x = cx0 + 6.0;
        let row_w = (card_w - 12.0).max(40.0);
        let row_h = 35.0_f32;
        let sync_row = Box4::new(row_x, cy0 + 46.0, row_w, row_h);
        let sync_minus = Box4::new(cx0 + card_w - 119.0, sync_row.y + (row_h - 24.0) * 0.5, 24.0, 24.0);
        let sync_plus = Box4::new(cx0 + card_w - 32.0, sync_row.y + (row_h - 24.0) * 0.5, 24.0, 24.0);
        let display_row = Box4::new(row_x, cy0 + 84.0, row_w, row_h);
        self.display_row_center_y = display_row.centre().1;
        let picker_row = Box4::new(row_x, cy0 + 122.0, row_w, row_h);
        let ret_row = Box4::new(row_x, cy0 + 168.0, row_w, row_h);

        let hits = vec![
            (MoreHit::OrientH, seg_h),
            (MoreHit::OrientV, seg_v),
            (MoreHit::SyncMinus, sync_minus),
            (MoreHit::SyncPlus, sync_plus),
            (MoreHit::OpenDisplay, display_row),
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
                    if !this.press_gesture.release(same) {
                        return;
                    }
                    let Some(hit) = pressed else {
                        return;
                    };
                    match hit {
                        MoreHit::OrientH | MoreHit::OrientV => {}
                        MoreHit::SyncMinus => this.bridge.push(DesktopCommand::SyncNudge(-200)),
                        MoreHit::SyncPlus => this.bridge.push(DesktopCommand::SyncNudge(200)),
                        MoreHit::OpenDisplay => {
                            let wb = window.bounds();
                            let card_screen = Box4::new(
                                f32::from(wb.origin.x) + POPOVER_ELASTIC_PAD,
                                f32::from(wb.origin.y) + POPOVER_ELASTIC_PAD,
                                this.card_rect.w,
                                this.card_rect.h,
                            );
                            let row_cy_screen = f32::from(wb.origin.y) + this.display_row_center_y;
                            this.bridge.push_popover(PopoverAction::OpenDisplayPopover(
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

                        paint_sidecar_glass_frame(window, card_rect, panel_radius, theme_mode, surf_a);

                        // Orientation segmented switcher
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
                                chrome::control_surface_alpha(window, box_r, mf, false, cont_a * 0.45);
                            }
                            chrome::paint_text_box_centred(
                                window,
                                label,
                                box_r,
                                12.0,
                                if selected { 650.0 } else { 550.0 },
                                if selected {
                                    chrome::segmented_active_text_blended(accent_hex, rest_alpha, 0.96 * cont_a)
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

                        chrome::fill_round(
                            window,
                            cx0 + 12.0,
                            cy0 + 41.0,
                            (card_w - 24.0).max(20.0),
                            0.75,
                            0.35,
                            text(0.08),
                        );

                        // Row 1: 歌词同步
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

                        // Row 2: 显示样式 › (opens L2 DesktopDisplayPopover on side)
                        let df = fb(MoreHit::OpenDisplay);
                        chrome::control_surface_alpha_radius(
                            window, display_row, df, display_open, cont_a, 13.5,
                        );
                        chrome::chevron(
                            window,
                            display_row.right() - 16.0,
                            display_row.y + (display_row.h - 8.0) * 0.5,
                            text(0.36 + 0.30 * df.hover),
                        );
                        icons::paint(
                            window,
                            cx,
                            Icon::FontSize,
                            Box4::new(cx0 + 14.0, display_row.y + (display_row.h - 16.0) * 0.5, 16.0, 16.0),
                            text(0.55 + 0.25 * df.hover),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            "显示样式",
                            cx0 + 38.0,
                            display_row.centre().1,
                            12.0,
                            500.0,
                            text(0.88 + 0.08 * df.hover),
                        );

                        // Row 3: 歌词版本
                        let pf = fb(MoreHit::LyricsPicker);
                        chrome::control_surface_alpha_radius(window, picker_row, pf, false, cont_a, 13.5);
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
                            Box4::new(cx0 + 14.0, picker_row.y + (picker_row.h - 16.0) * 0.5, 16.0, 16.0),
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

                        chrome::fill_round(
                            window,
                            cx0 + 12.0,
                            cy0 + 162.0,
                            (card_w - 24.0).max(20.0),
                            0.75,
                            0.35,
                            text(0.08),
                        );

                        // Row 4: 返回主窗
                        let rf = fb(MoreHit::ReturnMain);
                        chrome::control_surface_alpha_radius(window, ret_row, rf, false, cont_a, 13.5);
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
                            Box4::new(cx0 + 14.0, ret_row.y + (ret_row.h - 16.0) * 0.5, 16.0, 16.0),
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

/// Level 2 side Liquid Glass submenu: 字号大小 / 文字颜色 / 显示行数 / 长句滚动.
pub(in crate::player::desktop_lyrics) struct DesktopDisplayPopover {
    bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    auto_scroll_spring: Spring,
    feedback: HashMap<DisplayHit, Feedback>,
    elastics: HashMap<DisplayHit, Elastic>,
    panel_elastic: Elastic,
    press_gesture: PressGesture,
    pressed_hit: Option<DisplayHit>,
    panel_dragging: bool,
    card_rect: Box4,
    color_row_center_y: f32,
    lines_row_center_y: f32,
    was_active: bool,
    configured: bool,
    cursor: Option<(f32, f32)>,
    hits: Vec<(DisplayHit, Box4)>,
    started: Instant,
    last_ms: f64,
}

impl DesktopDisplayPopover {
    pub(in crate::player::desktop_lyrics) fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
        let cfg = bridge.read_cfg();
        Self {
            bridge,
            motion: PanelMotion::default(),
            auto_scroll_spring: state_icon_spring(if cfg.auto_scroll { 1.0 } else { 0.0 }),
            feedback: HashMap::new(),
            elastics: HashMap::new(),
            panel_elastic: Elastic::default(),
            press_gesture: PressGesture::default(),
            pressed_hit: None,
            panel_dragging: false,
            card_rect: Box4::default(),
            color_row_center_y: 0.0,
            lines_row_center_y: 0.0,
            was_active: false,
            configured: false,
            cursor: None,
            hits: Vec::new(),
            started: Instant::now(),
            last_ms: 0.0,
        }
    }
}

impl Render for DesktopDisplayPopover {
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
        let lines_side = self.bridge.lines_popover_side.load(Ordering::SeqCst);
        let lines_open = lines_side != 0 || self.bridge.lines_popover_open.load(Ordering::SeqCst);
        if window.is_window_active() {
            self.was_active = true;
        } else if self.was_active && !self.panel_dragging && !color_open && !lines_open {
            self.bridge.mark_display_closed();
            window.remove_window();
            return div().into_any_element();
        }
        if !win32_window::is_left_mouse_down() {
            if self.pressed_hit.is_some() || self.panel_dragging {
                self.pressed_hit = None;
                self.panel_dragging = false;
                self.press_gesture.cancel();
            }
        } else if self.panel_dragging || self.pressed_hit.is_some() {
            if let Some((cx_p, cy_p)) = win32_window::cursor_screen_pos() {
                let sf = window.scale_factor().max(0.5);
                let wb = window.bounds();
                let lx = (cx_p as f32 / sf) - f32::from(wb.origin.x);
                let ly = (cy_p as f32 / sf) - f32::from(wb.origin.y);
                self.cursor = Some((lx, ly));
                self.press_gesture.update(lx, ly);
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
        let card_h = (win_h - POPOVER_ELASTIC_PAD * 2.0).max(100.0);
        let cx0 = POPOVER_ELASTIC_PAD + pp[0];
        let cy0 = POPOVER_ELASTIC_PAD + pp[1];
        let card_rect = Box4::new(cx0, cy0, card_w, card_h);
        self.card_rect = card_rect;

        let row_x = cx0 + 6.0;
        let row_w = (card_w - 12.0).max(40.0);
        let row_h = 35.0_f32;
        let font_row = Box4::new(row_x, cy0 + 8.0, row_w, row_h);
        let font_minus = Box4::new(cx0 + card_w - 119.0, font_row.y + (row_h - 24.0) * 0.5, 24.0, 24.0);
        let font_plus = Box4::new(cx0 + card_w - 32.0, font_row.y + (row_h - 24.0) * 0.5, 24.0, 24.0);
        let color_row = Box4::new(row_x, cy0 + 44.0, row_w, row_h);
        let color_pill = Box4::new(cx0 + card_w - 119.0, color_row.y + (row_h - 25.0) * 0.5, 111.0, 25.0);
        self.color_row_center_y = color_row.centre().1;
        let lines_row = Box4::new(row_x, cy0 + 80.0, row_w, row_h);
        let lines_pill = Box4::new(cx0 + card_w - 92.0, lines_row.y + (row_h - 25.0) * 0.5, 84.0, 25.0);
        self.lines_row_center_y = lines_row.centre().1;
        let scroll_row = Box4::new(row_x, cy0 + 116.0, row_w, row_h);
        let scroll_track = Box4::new(cx0 + card_w - 46.0, scroll_row.centre().1 - 10.0, 36.0, 20.0);
        self.auto_scroll_spring
            .set_target_position(if cfg.auto_scroll { 1.0 } else { 0.0 });
        self.auto_scroll_spring.update(dt / 1000.0);
        let scroll_frac = self.auto_scroll_spring.position().clamp(0.0, 1.0) as f32;

        let hits = vec![
            (DisplayHit::FontMinus, font_minus),
            (DisplayHit::FontPlus, font_plus),
            (DisplayHit::ColorPill, color_row),
            (DisplayHit::LinesPill, lines_row),
            (DisplayHit::AutoScroll, scroll_row),
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
        }

        let fb_map = self.feedback.clone();
        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let panel_radius = snap.display.corner_radius.clamp(12.0, 22.0);
        let accent_hex = snap.display.accent_hex;
        let font_label = format!("{}px", cfg.font_size.round() as i32);
        let color_name = color_preset_label(cfg.text_color);
        let lines_label = format!("{} 行", cfg.display_lines.clamp(1, 3));

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.pressed_hit.is_some() || this.panel_dragging {
                    this.press_gesture.update(x, y);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
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
                    let wb = window.bounds();
                    let card_screen = Box4::new(
                        f32::from(wb.origin.x) + POPOVER_ELASTIC_PAD,
                        f32::from(wb.origin.y) + POPOVER_ELASTIC_PAD,
                        this.card_rect.w,
                        this.card_rect.h,
                    );
                    match hit {
                        DisplayHit::FontMinus => this.bridge.push_popover(PopoverAction::FontDelta(-4.0)),
                        DisplayHit::FontPlus => this.bridge.push_popover(PopoverAction::FontDelta(4.0)),
                        DisplayHit::AutoScroll => this.bridge.push_popover(PopoverAction::ToggleAutoScroll),
                        DisplayHit::ColorPill => {
                            let row_cy = f32::from(wb.origin.y) + this.color_row_center_y;
                            this.bridge.push_popover(PopoverAction::OpenColorPopover(card_screen, row_cy));
                        }
                        DisplayHit::LinesPill => {
                            let row_cy = f32::from(wb.origin.y) + this.lines_row_center_y;
                            this.bridge.push_popover(PopoverAction::OpenLinesPopover(card_screen, row_cy));
                        }
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        chrome::set_light_theme(light_panel);
                        let fb = |h: DisplayHit| fb_map.get(&h).copied().unwrap_or_default();
                        let text = |a: f32| chrome::text(a * cont_a);
                        paint_sidecar_glass_frame(window, card_rect, panel_radius, theme_mode, surf_a);

                        // 1. 字号大小
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
                            fb(DisplayHit::FontMinus),
                            fb(DisplayHit::FontPlus),
                            &font_label,
                            true,
                            cont_a,
                        );

                        // 2. 文字颜色 -> L3 DesktopColorPopover
                        let cf = fb(DisplayHit::ColorPill);
                        chrome::control_surface_alpha_radius(window, color_row, cf, color_open, cont_a, 13.5);
                        icons::paint(
                            window,
                            cx,
                            Icon::Palette,
                            Box4::new(cx0 + 14.0, color_row.y + (color_row.h - 16.0) * 0.5, 16.0, 16.0),
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
                            chrome::ChoiceArrow::Right
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

                        // 3. 显示行数 -> L3 DesktopLinesPopover
                        let lf = fb(DisplayHit::LinesPill);
                        chrome::control_surface_alpha_radius(window, lines_row, lf, lines_open, cont_a, 13.5);
                        icons::paint(
                            window,
                            cx,
                            Icon::List,
                            Box4::new(cx0 + 14.0, lines_row.y + (lines_row.h - 16.0) * 0.5, 16.0, 16.0),
                            text(0.55 + 0.25 * lf.hover),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            "显示行数",
                            cx0 + 38.0,
                            lines_row.centre().1,
                            12.0,
                            500.0,
                            text(0.88 + 0.08 * lf.hover),
                        );
                        let lines_arrow = if lines_side < 0 {
                            chrome::ChoiceArrow::Left
                        } else {
                            chrome::ChoiceArrow::Right
                        };
                        chrome::paint_choice_pill_with_arrow(
                            window,
                            lines_pill,
                            &lines_label,
                            lines_open,
                            lines_arrow,
                            accent_hex,
                            lf,
                            cont_a,
                        );

                        // 4. 长句滚动
                        let sf = fb(DisplayHit::AutoScroll);
                        chrome::control_surface_alpha_radius(window, scroll_row, sf, false, cont_a, 13.5);
                        icons::paint(
                            window,
                            cx,
                            Icon::Lyrics,
                            Box4::new(cx0 + 14.0, scroll_row.y + (scroll_row.h - 16.0) * 0.5, 16.0, 16.0),
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

                        chrome::set_light_theme(false);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

/// Level 3 side Liquid Glass list for Display Lines (`1 行` / `2 行` / `3 行`).
pub(in crate::player::desktop_lyrics) struct DesktopLinesPopover {
    bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    feedback: [Feedback; 3],
    panel_elastic: Elastic,
    press_gesture: PressGesture,
    pressed_line: Option<u8>,
    panel_dragging: bool,
    card_rect: Box4,
    was_active: bool,
    configured: bool,
    cursor: Option<(f32, f32)>,
    hits: [(u8, Box4); 3],
    started: Instant,
    last_ms: f64,
}

impl DesktopLinesPopover {
    pub(in crate::player::desktop_lyrics) fn new(bridge: Arc<SharedDesktopBridge>) -> Self {
        Self {
            bridge,
            motion: PanelMotion::default(),
            feedback: [Feedback::default(); 3],
            panel_elastic: Elastic::default(),
            press_gesture: PressGesture::default(),
            pressed_line: None,
            panel_dragging: false,
            card_rect: Box4::default(),
            was_active: false,
            configured: false,
            cursor: None,
            hits: [(1, Box4::default()), (2, Box4::default()), (3, Box4::default())],
            started: Instant::now(),
            last_ms: 0.0,
        }
    }
}

impl Render for DesktopLinesPopover {
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
        } else if self.was_active && !self.panel_dragging {
            self.bridge.mark_lines_closed();
            window.remove_window();
            return div().into_any_element();
        }
        if !win32_window::is_left_mouse_down() {
            if self.pressed_line.is_some() || self.panel_dragging {
                self.pressed_line = None;
                self.panel_dragging = false;
                self.press_gesture.cancel();
            }
        } else if self.panel_dragging || self.pressed_line.is_some() {
            if let Some((cx_p, cy_p)) = win32_window::cursor_screen_pos() {
                let sf = window.scale_factor().max(0.5);
                let wb = window.bounds();
                let lx = (cx_p as f32 / sf) - f32::from(wb.origin.x);
                let ly = (cy_p as f32 / sf) - f32::from(wb.origin.y);
                self.cursor = Some((lx, ly));
                self.press_gesture.update(lx, ly);
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
        let panel_held = self.panel_dragging || (self.pressed_line.is_some() && self.press_gesture.dragged());
        self.panel_elastic.step(raw_delta, panel_held, dt);
        let pp = self.panel_elastic.pull();

        let card_w = (win_w - POPOVER_ELASTIC_PAD * 2.0).max(80.0);
        let card_h = (win_h - POPOVER_ELASTIC_PAD * 2.0).max(80.0);
        let cx0 = POPOVER_ELASTIC_PAD + pp[0];
        let cy0 = POPOVER_ELASTIC_PAD + pp[1];
        let card_rect = Box4::new(cx0, cy0, card_w, card_h);
        self.card_rect = card_rect;

        let hits: [(u8, Box4); 3] = std::array::from_fn(|idx| {
            let n = (idx + 1) as u8;
            (n, Box4::new(cx0 + 6.0, cy0 + 8.0 + idx as f32 * 32.0, (card_w - 12.0).max(40.0), 30.0))
        });
        self.hits = hits;

        for (idx, &(n, rect)) in hits.iter().enumerate() {
            let hot = self.cursor.is_some_and(|(mx, my)| rect.contains(mx, my));
            let held = self.pressed_line == Some(n);
            self.feedback[idx].step(hot, held, dt);
        }

        let fb_arr = self.feedback;
        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let panel_radius = snap.display.corner_radius.clamp(12.0, 22.0);
        let accent_hex = snap.display.accent_hex;
        let cur_lines = cfg.display_lines.clamp(1, 3);

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.pressed_line.is_some() || this.panel_dragging {
                    this.press_gesture.update(x, y);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    let hit = this.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(n, _)| *n);
                    this.press_gesture.begin(x, y);
                    if let Some(n) = hit {
                        this.pressed_line = Some(n);
                        this.panel_dragging = false;
                    } else if this.card_rect.contains(x, y) {
                        this.pressed_line = None;
                        this.panel_dragging = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    this.press_gesture.update(x, y);
                    this.panel_dragging = false;
                    let pressed = this.pressed_line.take();
                    let cur = this.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(n, _)| *n);
                    let same = pressed.is_some() && cur == pressed;
                    if this.press_gesture.release(same) {
                        if let Some(n) = pressed {
                            this.bridge.push_popover(PopoverAction::SetDisplayLines(n));
                            this.bridge.mark_lines_closed();
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
                        let text = |a: f32| chrome::text(a * cont_a);
                        paint_sidecar_glass_frame(window, card_rect, panel_radius, theme_mode, surf_a);
                        for (idx, &(n, r)) in hits.iter().enumerate() {
                            let selected = n == cur_lines;
                            chrome::control_surface_alpha_radius(
                                window,
                                r,
                                fb_arr[idx],
                                selected,
                                cont_a,
                                10.0,
                            );
                            let label = format!("{n} 行");
                            chrome::paint_text_vcentred(
                                window,
                                &label,
                                r.x + 12.0,
                                r.centre().1,
                                12.0,
                                if selected { 600.0 } else { 500.0 },
                                text(if selected { 0.96 } else { 0.84 }),
                            );
                            if selected {
                                chrome::paint_text_vcentred(
                                    window,
                                    "✓",
                                    r.right() - 20.0,
                                    r.centre().1,
                                    12.5,
                                    650.0,
                                    chrome::accent_rgb(accent_hex, 0.95 * cont_a),
                                );
                            }
                        }
                        chrome::set_light_theme(false);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

