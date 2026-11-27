use super::*;

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
    parent: WeakEntity<crate::player::view::PlayerView>,
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
    display_popover: Option<AnyWindowHandle>,
    color_popover: Option<AnyWindowHandle>,
    lines_popover: Option<AnyWindowHandle>,
    picker_popover: Option<AnyWindowHandle>,
    last_more_anchor: Box4,
    bar_rect: Box4,
    idle_poll_pending: bool,
}

impl DesktopLyricsView {
    pub fn new(
        bridge: Arc<SharedDesktopBridge>,
        parent: WeakEntity<crate::player::view::PlayerView>,
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
            display_popover: None,
            color_popover: None,
            lines_popover: None,
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

    fn close_lines_popover(&mut self, cx: &mut App) {
        self.bridge.lines_popover_open.store(false, Ordering::SeqCst);
        self.bridge.lines_popover_side.store(0, Ordering::SeqCst);
        if let Some(h) = self.lines_popover.take() {
            let _ = h.update(cx, |_, w, _| w.remove_window());
        }
    }

    fn close_display_popover(&mut self, cx: &mut App) {
        self.close_color_popover(cx);
        self.close_lines_popover(cx);
        self.bridge.display_popover_open.store(false, Ordering::SeqCst);
        self.bridge.display_popover_side.store(0, Ordering::SeqCst);
        if let Some(h) = self.display_popover.take() {
            let _ = h.update(cx, |_, w, _| w.remove_window());
        }
    }

    fn close_popovers(&mut self, cx: &mut App) {
        self.close_display_popover(cx);
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
        let win_w = f32::from(wb.size.width).max(1.0);
        let win_h = f32::from(wb.size.height).max(1.0);
        let effective_anchor = if anchor.w > 1.0 && anchor.h > 1.0 {
            anchor
        } else if self.bar_rect.w > 1.0 && self.bar_rect.h > 1.0 {
            self.bar_rect
        } else {
            Box4::new(win_w * 0.5 - 16.0, win_h - 32.0, 32.0, 28.0)
        };
        let ax = wx + effective_anchor.x;
        let ay = wy + effective_anchor.y;
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
        let gap = 8.0;
        let (raw_x, raw_y) = match self.cfg.orientation {
            DesktopOrientation::Horizontal => {
                let cx_a = ax + effective_anchor.w * 0.5 - pop_w * 0.5;
                let below_y = wy + win_h + gap;
                let above_y = wy - pop_h - gap;
                let space_below = (dy + dh - 8.0) - below_y;
                let space_above = wy - gap - (dy + 8.0);
                // Prefer below the lyrics window when room allows so the popover never covers lyrics;
                // flip above the entire lyrics window when near the bottom edge.
                let py = if space_below >= pop_h || space_below >= space_above {
                    below_y
                } else {
                    above_y
                };
                (cx_a, py)
            }
            DesktopOrientation::Vertical => {
                let cy_a = ay + effective_anchor.h * 0.5 - pop_h * 0.5;
                let right_x = wx + win_w + gap;
                let left_x = wx - pop_w - gap;
                let space_right = (dx + dw - 8.0) - right_x;
                let space_left = wx - gap - (dx + 8.0);
                let px_v = if space_right >= pop_w || space_right >= space_left {
                    right_x
                } else {
                    left_x
                };
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
            self.close_display_popover(cx);
            if h.update(cx, |_, w, _| w.remove_window()).is_ok() {
                return;
            }
        }
        if self.bridge.was_more_recently_closed() {
            self.close_display_popover(cx);
            return;
        }
        self.close_popovers(cx);
        let (pw, ph) = (252.0_f32 + POPOVER_ELASTIC_PAD * 2.0, 212.0_f32 + POPOVER_ELASTIC_PAD * 2.0);
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

    fn side_popover_origin(
        parent_card_screen: Box4,
        row_center_y_screen: f32,
        card_w: f32,
        card_h: f32,
        align_y_in_card: f32,
        window: &Window,
        cx: &App,
    ) -> ((f32, f32), i8) {
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
        let pw = card_w + POPOVER_ELASTIC_PAD * 2.0;
        let ph = card_h + POPOVER_ELASTIC_PAD * 2.0;
        let gap = 8.0_f32;
        let right_x = parent_card_screen.right() + gap - POPOVER_ELASTIC_PAD;
        let left_x = parent_card_screen.x - gap - card_w - POPOVER_ELASTIC_PAD;
        let (px_c, side): (f32, i8) = if right_x + pw <= dx + dw - 6.0 {
            (right_x, 1)
        } else {
            (left_x.max(dx + 6.0), -1)
        };
        let py_c = (row_center_y_screen - align_y_in_card - POPOVER_ELASTIC_PAD)
            .clamp(dy + 6.0, (dy + dh - ph - 6.0).max(dy + 6.0));
        ((px_c, py_c), side)
    }

    fn open_display_popover(
        &mut self,
        more_card_screen: Box4,
        row_center_y_screen: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(h) = self.display_popover.take() {
            self.close_color_popover(cx);
            self.close_lines_popover(cx);
            self.bridge.display_popover_open.store(false, Ordering::SeqCst);
            self.bridge.display_popover_side.store(0, Ordering::SeqCst);
            if h.update(cx, |_, w, _| w.remove_window()).is_ok() {
                return;
            }
        }
        if self.bridge.was_display_recently_closed() {
            return;
        }
        let card_w = 252.0_f32;
        let card_h = 160.0_f32;
        let pw = card_w + POPOVER_ELASTIC_PAD * 2.0;
        let ph = card_h + POPOVER_ELASTIC_PAD * 2.0;
        let (pos, side) = Self::side_popover_origin(
            more_card_screen,
            row_center_y_screen,
            card_w,
            card_h,
            26.0,
            window,
            cx,
        );
        self.bridge.display_popover_open.store(true, Ordering::SeqCst);
        self.bridge.display_popover_side.store(side, Ordering::SeqCst);
        let bridge = self.bridge.clone();
        self.display_popover = Self::spawn_glass_popover(
            "OverLyrics Desktop Display",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopDisplayPopover::new(bridge),
        );
    }

    fn open_color_popover(
        &mut self,
        parent_card_screen: Box4,
        color_row_center_y_screen: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_lines_popover(cx);
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
        let card_w = 240.0_f32;
        let card_h = 214.0_f32;
        let pw = card_w + POPOVER_ELASTIC_PAD * 2.0;
        let ph = card_h + POPOVER_ELASTIC_PAD * 2.0;
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
        let (pos, side) = Self::side_popover_origin(
            parent_card_screen,
            color_row_center_y_screen,
            card_w,
            card_h,
            selected_row_center_in_card,
            window,
            cx,
        );
        self.bridge.color_popover_open.store(true, Ordering::SeqCst);
        self.bridge.color_popover_side.store(side, Ordering::SeqCst);
        let bridge = self.bridge.clone();
        self.color_popover = Self::spawn_glass_popover(
            "OverLyrics Desktop Color",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopColorPopover::new(bridge),
        );
    }

    fn open_lines_popover(
        &mut self,
        parent_card_screen: Box4,
        row_center_y_screen: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_color_popover(cx);
        if let Some(h) = self.lines_popover.take() {
            self.bridge.lines_popover_open.store(false, Ordering::SeqCst);
            self.bridge.lines_popover_side.store(0, Ordering::SeqCst);
            if h.update(cx, |_, w, _| w.remove_window()).is_ok() {
                return;
            }
        }
        if self.bridge.was_lines_recently_closed() {
            return;
        }
        let card_w = 132.0_f32;
        let card_h = 114.0_f32;
        let pw = card_w + POPOVER_ELASTIC_PAD * 2.0;
        let ph = card_h + POPOVER_ELASTIC_PAD * 2.0;
        let sel_idx = self.cfg.display_lines.clamp(1, 3).saturating_sub(1) as f32;
        let align_y = 8.0 + sel_idx * 32.0 + 16.0;
        let (pos, side) = Self::side_popover_origin(
            parent_card_screen,
            row_center_y_screen,
            card_w,
            card_h,
            align_y,
            window,
            cx,
        );
        self.bridge.lines_popover_open.store(true, Ordering::SeqCst);
        self.bridge.lines_popover_side.store(side, Ordering::SeqCst);
        let bridge = self.bridge.clone();
        self.lines_popover = Self::spawn_glass_popover(
            "OverLyrics Desktop Lines",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopLinesPopover::new(bridge),
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
            let mut m = crate::player::model::metrics(HORIZ_W, HORIZ_H, (1920.0, 1080.0), false, &d);
            m.font = base_font;
            m.line_height = base_font * d.line_height_em.max(1.15);
            return self.shaper.row(row, &m, &d, 4096.0, 0xD100).ok();
        }

        let mut fs = base_font;
        for _ in 0..3 {
            let mut m = crate::player::model::metrics(HORIZ_W, HORIZ_H, (1920.0, 1080.0), false, &d);
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
        let mut m = crate::player::model::metrics(HORIZ_W, HORIZ_H, (1920.0, 1080.0), false, &d);
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
                PopoverAction::SetDisplayLines(n) => {
                    self.cfg.display_lines = n.clamp(1, 3);
                    self.bridge.set_cfg(self.cfg.clone());
                    self.bridge
                        .push(DesktopCommand::SaveConfig(self.cfg.clone()));
                }
                PopoverAction::OpenDisplayPopover(more_card_screen, row_center_y) => {
                    self.open_display_popover(more_card_screen, row_center_y, window, cx);
                }
                PopoverAction::CloseDisplayPopover => {
                    self.close_display_popover(cx);
                }
                PopoverAction::OpenColorPopover(parent_card_screen, color_row_center_y) => {
                    self.open_color_popover(parent_card_screen, color_row_center_y, window, cx);
                }
                PopoverAction::CloseColorPopover => {
                    self.close_color_popover(cx);
                }
                PopoverAction::OpenLinesPopover(parent_card_screen, row_center_y) => {
                    self.open_lines_popover(parent_card_screen, row_center_y, window, cx);
                }
                PopoverAction::CloseLinesPopover => {
                    self.close_lines_popover(cx);
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

        // Sync lock state if toggled externally (e.g. from the system tray switch).
        let bridge_locked = self.bridge.read_cfg().locked;
        if bridge_locked != self.cfg.locked {
            self.set_locked(bridge_locked, now, cx);
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
        let display_lines = self.cfg.display_lines.clamp(1, 3);
        let shaped_active = snap.active_row.as_ref().and_then(|row| {
            self.shape_single_line(row, &snap.display, font_size, horiz_avail_w, auto_scroll)
        });
        let shaped_multi: Vec<(usize, Row, Arc<RowLines>)> = if orient == DesktopOrientation::Horizontal
            && display_lines > 1
            && snap.display_rows.len() > 1
        {
            let count = snap.display_rows.len().min(display_lines as usize);
            let bilingual_slots: f32 = snap
                .display_rows
                .iter()
                .take(count)
                .map(|(_, r)| {
                    if show_trans && !r.translation.trim().is_empty() {
                        1.72
                    } else {
                        1.08
                    }
                })
                .sum();
            let avail_per_slot = ((win_h - 20.0) / bilingual_slots.max(1.0)).max(18.0);
            let fit_scale = (avail_per_slot / (font_size * 1.25)).clamp(0.38, 1.0);
            snap.display_rows
                .iter()
                .take(count)
                .filter_map(|(idx, r)| {
                    let is_active = Some(*idx) == snap.active_row_index;
                    let role_scale = if r.is_bg {
                        0.76
                    } else if is_active {
                        1.0
                    } else {
                        0.86
                    };
                    let row_fs = (font_size * fit_scale * role_scale).clamp(13.0, MAX_FONT);
                    self.shape_single_line(r, &snap.display, row_fs, horiz_avail_w, false)
                        .map(|s| (*idx, r.clone(), s))
                })
                .collect()
        } else {
            Vec::new()
        };

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
                                    if !shaped_multi.is_empty() {
                                        let gap = 5.0_f32;
                                        let total_h: f32 = shaped_multi
                                            .iter()
                                            .enumerate()
                                            .map(|(k, (_, r, s))| {
                                                let row_tfs = (s.font_size * 0.58).clamp(10.5, 26.0);
                                                let th = if show_trans && !r.translation.trim().is_empty() {
                                                    2.0 + row_tfs * 1.18
                                                } else {
                                                    0.0
                                                };
                                                s.line_height + th + if k > 0 { gap } else { 0.0 }
                                            })
                                            .sum();
                                        let mut cur_y = horiz_area_top + (horiz_area_h - total_h) * 0.5;
                                        let origin_x = (win_w - horiz_avail_w) * 0.5;
                                        for (idx, r, s) in &shaped_multi {
                                            let is_active = Some(*idx) == snap.active_row_index
                                                || (r.is_bg
                                                    && snap.position_ms >= r.start_ms
                                                    && snap.position_ms <= r.end_ms + 300.0);
                                            let dim = if is_active {
                                                if r.is_bg { 0.84 } else { 1.0 }
                                            } else if r.is_bg {
                                                0.46
                                            } else {
                                                0.54
                                            };
                                            let light = row_light(r, snap.position_ms, is_active, false, &snap.display);
                                            let progress = paint::line_progress(
                                                snap.position_ms,
                                                r.start_ms,
                                                r.duration_ms(),
                                            );
                                            let masks: Vec<f32> = s
                                                .masks
                                                .iter()
                                                .map(|frames| position_at(frames, progress))
                                                .collect();
                                            let row_light_val = RowLight {
                                                base: (light.dark * dim * line_fade).clamp(0.0, 1.0),
                                                dark: (light.dark * dim * line_fade).clamp(0.0, 1.0),
                                                bright: (light.bright * dim * line_fade).clamp(0.0, 1.0),
                                                mask: if is_active && light.sweep.is_some() {
                                                    Some(masks)
                                                } else {
                                                    None
                                                },
                                                glow: if is_active {
                                                    light.glow.map(|mut g| {
                                                        for v in &mut g {
                                                            *v *= dim * line_fade;
                                                        }
                                                        g
                                                    })
                                                } else {
                                                    None
                                                },
                                            };
                                            paint::paint_row_with_shadow(
                                                window,
                                                s,
                                                origin_x,
                                                cur_y,
                                                s.line_height,
                                                s.font_size,
                                                &s.words,
                                                &row_light_val,
                                                snap.position_ms,
                                                1.0,
                                                WrapperTransform::default(),
                                                -10000.0,
                                                10000.0,
                                                1.0,
                                                horiz_avail_w,
                                                LineAlign::Center,
                                                0.0,
                                                DESKTOP_DROP_SHADOW,
                                                Some(text_color),
                                            );
                                            cur_y += s.line_height;
                                            if show_trans && !r.translation.trim().is_empty() {
                                                let row_tfs = (s.font_size * 0.58).clamp(10.5, 26.0);
                                                let trans_a = (snap.display.trans_current.max(0.75) * dim * line_fade)
                                                    .clamp(0.0, 1.0);
                                                paint_horiz_translation(
                                                    window,
                                                    &r.translation,
                                                    win_w,
                                                    cur_y + 2.0,
                                                    row_tfs,
                                                    text_color,
                                                    trans_a,
                                                    false,
                                                    0.0,
                                                );
                                                cur_y += 2.0 + row_tfs * 1.18;
                                            }
                                            cur_y += gap;
                                        }
                                    } else {
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


pub(super) fn snap_axis(
    raw: f32,
    win_len: f32,
    disp_origin: f32,
    disp_len: f32,
    was_snapped: bool,
) -> (f32, bool) {
    let start_pos = disp_origin;
    let center_pos = disp_origin + (disp_len - win_len) * 0.5;
    let end_pos = disp_origin + disp_len - win_len;
    let threshold = if was_snapped { 18.0 } else { 12.0 };
    for target in [start_pos, end_pos, center_pos] {
        if (raw - target).abs() <= threshold {
            return (target, true);
        }
    }
    let min_v = disp_origin - win_len + 64.0;
    let max_v = disp_origin + disp_len - 64.0;
    (raw.clamp(min_v, max_v), false)
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
    let b = d.visible_bounds();
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

