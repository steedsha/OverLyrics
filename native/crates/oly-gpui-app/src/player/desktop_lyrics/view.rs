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
    last_live_pos_ms: Option<f64>,
    last_track_title: String,
    last_track_artist: String,
    amll_row_springs: HashMap<usize, crate::player::model::RowMotion>,
    amll_bg_alpha: HashMap<usize, f32>,
    amll_row_scales: HashMap<usize, crate::player::model::RowMotion>,
    amll_prev_rows: Vec<(usize, Arc<RowLines>, f32, f32)>,
    amll_exiting: Vec<(usize, Row, Arc<RowLines>, f32, f32, f32)>,
    vert_col_springs: HashMap<usize, crate::player::model::RowMotion>,
    vert_col_alpha: HashMap<usize, f32>,
    vert_col_scrolls: HashMap<usize, f32>,
    vert_multi_cache: HashMap<
        usize,
        (
            u32,
            u32,
            Arc<(Vec<VerticalToken>, Vec<f32>, f32)>,
            Option<(u32, Arc<(Vec<VerticalToken>, Vec<f32>, f32)>)>,
        ),
    >,
    vert_prev_cols: Vec<(usize, f32, f32, f32, f32, f32, Arc<(Vec<VerticalToken>, Vec<f32>, f32)>)>,
    vert_exiting_cols: Vec<(
        usize,
        Row,
        f32,
        f32,
        f32,
        f32,
        f32,
        f32,
        Arc<(Vec<VerticalToken>, Vec<f32>, f32)>,
    )>,
    last_parent_tick_ms: f64,
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
            last_live_pos_ms: None,
            last_track_title: String::new(),
            last_track_artist: String::new(),
            amll_row_springs: HashMap::new(),
            amll_bg_alpha: HashMap::new(),
            amll_row_scales: HashMap::new(),
            amll_prev_rows: Vec::new(),
            amll_exiting: Vec::new(),
            vert_col_springs: HashMap::new(),
            vert_col_alpha: HashMap::new(),
            vert_col_scrolls: HashMap::new(),
            vert_multi_cache: HashMap::new(),
            vert_prev_cols: Vec::new(),
            vert_exiting_cols: Vec::new(),
            last_parent_tick_ms: -1000.0,
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
        self.cfg
            .set_size(f32::from(b.size.width), f32::from(b.size.height));
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
        let (clamped, _) = resolve_restored_origin(self.cfg.saved_pos(), w, h, orient, window, cx);
        window.set_bounds(gpui_kit::bounds(
            point(px(clamped.0), px(clamped.1)),
            size(px(w), px(h)),
        ));
        self.last_origin = Some(clamped);
        self.last_size = Some((w, h));
        self.trans_side_inited = false;
        self.bar_side_inited = false;
        self.cfg.set_pos(clamped.0, clamped.1);
        self.cfg.set_size(w, h);
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

    pub(crate) fn spawn_glass_popover<V: 'static + Render>(
        title: &'static str,
        pos: (f32, f32),
        size_px: (f32, f32),
        window: &Window,
        cx: &mut App,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
    ) -> Option<AnyWindowHandle> {
        let options = WindowOptions {
            inactive_frame_interval: None,
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
        let (pw, ph) = (VOLUME_POPOVER_W, VOLUME_POPOVER_H);
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
        preferred_side: i8,
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
        let min_x = dx + 6.0;
        let max_x = (dx + dw - pw - 6.0).max(min_x);
        let right_fits = right_x + pw <= dx + dw - 6.0;
        let left_fits = left_x >= min_x;
        let (px_c, side): (f32, i8) = match preferred_side {
            -1 => {
                if left_fits {
                    (left_x, -1)
                } else if right_fits {
                    (right_x, 1)
                } else {
                    (left_x.clamp(min_x, max_x), -1)
                }
            }
            1 => {
                if right_fits {
                    (right_x, 1)
                } else if left_fits {
                    (left_x, -1)
                } else {
                    (right_x.clamp(min_x, max_x), 1)
                }
            }
            _ => {
                if right_fits {
                    (right_x, 1)
                } else if left_fits {
                    (left_x, -1)
                } else {
                    let space_right = (dx + dw) - parent_card_screen.right();
                    let space_left = parent_card_screen.x - dx;
                    if space_right >= space_left {
                        (right_x.clamp(min_x, max_x), 1)
                    } else {
                        (left_x.clamp(min_x, max_x), -1)
                    }
                }
            }
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
        let pref_l2: i8 = window
            .display(cx)
            .map(|d| {
                let b = d.visible_bounds();
                let dl = f32::from(b.origin.x);
                let dr = dl + f32::from(b.size.width);
                let right_room = dr - more_card_screen.right();
                let left_room = more_card_screen.x - dl;
                // Reserve space for both L2 (252) and L3 (240) so cascading doesn't hit the right edge.
                if right_room < card_w + 256.0 && left_room > right_room {
                    -1
                } else {
                    0
                }
            })
            .unwrap_or(0);
        let (pos, side) = Self::side_popover_origin(
            more_card_screen,
            row_center_y_screen,
            card_w,
            card_h,
            26.0,
            pref_l2,
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
        let preferred_side = self.bridge.display_popover_side.load(Ordering::SeqCst);
        let (pos, side) = Self::side_popover_origin(
            parent_card_screen,
            color_row_center_y_screen,
            card_w,
            card_h,
            selected_row_center_in_card,
            preferred_side,
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
        let preferred_side = self.bridge.display_popover_side.load(Ordering::SeqCst);
        let (pos, side) = Self::side_popover_origin(
            parent_card_screen,
            row_center_y_screen,
            card_w,
            card_h,
            align_y,
            preferred_side,
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

/// Resolves a saved desktop-lyrics origin across all currently connected displays.
/// If the saved rectangle falls outside every visible work area (e.g. a monitor was
/// disconnected), safely falls back to the primary display's default position.
pub fn resolve_restored_origin(
    saved: Option<(f32, f32)>,
    w: f32,
    h: f32,
    orientation: DesktopOrientation,
    window: &Window,
    cx: &App,
) -> ((f32, f32), Option<DisplayId>) {
    let displays = cx.displays();
    if let Some((sx, sy)) = saved {
        let mut best: Option<(f32, (f32, f32), DisplayId)> = None;
        for d in &displays {
            let vb = d.visible_bounds();
            let dx = f32::from(vb.origin.x);
            let dy = f32::from(vb.origin.y);
            let dw = f32::from(vb.size.width).max(1.0);
            let dh = f32::from(vb.size.height).max(1.0);
            let ox = (sx + w).min(dx + dw) - sx.max(dx);
            let oy = (sy + h).min(dy + dh) - sy.max(dy);
            if ox >= 48.0 && oy >= 32.0 {
                let area = ox * oy;
                let max_x = (dx + dw - w).max(dx);
                let max_y = (dy + dh - h).max(dy);
                let clamped = (sx.clamp(dx, max_x), sy.clamp(dy, max_y));
                if best.is_none_or(|(best_area, _, _)| area > best_area) {
                    best = Some((area, clamped, d.id()));
                }
            }
        }
        if let Some((_, pos, did)) = best {
            return (pos, Some(did));
        }
    }
    if let Some(primary) = cx.primary_display().or_else(|| window.display(cx)) {
        let vb = primary.visible_bounds();
        let dx = f32::from(vb.origin.x);
        let dy = f32::from(vb.origin.y);
        let dw = f32::from(vb.size.width).max(1.0);
        let dh = f32::from(vb.size.height).max(1.0);
        let (def_x, def_y) = match orientation {
            DesktopOrientation::Horizontal => (dx + (dw - w) * 0.5, dy + dh - h - 48.0),
            DesktopOrientation::Vertical => (dx + dw - w - 28.0, dy + (dh - h) * 0.5),
        };
        let max_x = (dx + dw - w).max(dx);
        let max_y = (dy + dh - h).max(dy);
        return ((def_x.clamp(dx, max_x), def_y.clamp(dy, max_y)), Some(primary.id()));
    }
    let def = default_origin_for(orientation, window, cx);
    (clamp_origin(def.0, def.1, w, h, window, cx), window.display(cx).map(|d| d.id()))
}

fn word_progress(pos_ms: f64, start_ms: f64, end_ms: f64) -> f32 {
    if end_ms <= start_ms {
        return if pos_ms >= start_ms { 1.0 } else { 0.0 };
    }
    ((pos_ms - start_ms) / (end_ms - start_ms)).clamp(0.0, 1.0) as f32
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
    for (i, tok) in tokens.iter().enumerate() {
        let next_is_latin = matches!(tokens.get(i + 1), Some(VerticalToken::RotatedRun { .. }));
        let prev_is_latin = i > 0 && matches!(tokens.get(i - 1), Some(VerticalToken::RotatedRun { .. }));
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
                // Adjacent RotatedRuns without Space belong to the same multi-syllable English word:
                // keep exact glyph width with zero extra gap so long words never split apart.
                if next_is_latin || prev_is_latin {
                    gw.max(1.0)
                } else {
                    gw.max(fs * 0.45) + fs * 0.08
                }
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
    premeasured: Option<&(Vec<VerticalToken>, Vec<f32>, f32)>,
) {
    let owned_measured;
    let (tokens, steps, total_h): (&[VerticalToken], &[f32], f32) = if let Some((t, s, h)) = premeasured {
        if t.is_empty() {
            return;
        }
        (t.as_slice(), s.as_slice(), *h)
    } else {
        let t = tokenize_vertical_row(row);
        if t.is_empty() {
            return;
        }
        let (s, h) = measure_vertical_steps(window, &t, base_font, display.weight);
        owned_measured = (t, s, h);
        (&owned_measured.0, &owned_measured.1, owned_measured.2)
    };

    let fs = base_font;
    let vis_top = 18.0_f32;
    let vis_bottom = (win_h - 18.0).max(vis_top + 60.0);
    let vis_h = (vis_bottom - vis_top).max(60.0);
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
    let word_timed = row.word_timed || shaped.is_some_and(|s| s.word_timed);
    let dark_a = (display.unplayed.max(0.52) * base_alpha * fade).clamp(0.0, 1.0);
    let bright_a = (base_alpha * fade).clamp(0.0, 1.0);
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
                        let ea = if overflowing {
                            edge_fade(rot_y)
                        } else {
                            1.0
                        };
                        if ea <= 0.01 {
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
                            // Suppress drop shadow on very faint edge glyphs so they never appear as dark blobs.
                            if alpha >= 0.14 {
                                let mut st = transform;
                                st.translation[1] += if dark_text { 1.0 * sf } else { 1.5 * sf };
                                let sa = (shadow_alpha_scale * alpha * alpha.min(1.0)).clamp(0.0, 0.85);
                                let _ = window.paint_lyric_glyph(
                                    origin,
                                    r.font_id,
                                    g.id,
                                    px(run_fs),
                                    Hsla::from(gpui_kit::rgb(shadow_rgb)).opacity(sa),
                                    st,
                                    [0.0; 4],
                                    [0.0; 4],
                                    2,
                                );
                            }
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
    let (fs, tw) = if !auto_scroll && raw_w > max_w && raw_w > 1.0 {
        let shrunk = (font_size * (max_w / raw_w)).clamp(11.0, font_size);
        let (w, _) = chrome::measure(window, trimmed, shrunk, 500.0);
        (shrunk, w)
    } else {
        (font_size, raw_w)
    };
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
        let now = self.now_ms();
        let dt = (now - self.last_ms).clamp(1.0, 33.0);
        self.last_ms = now;

        // Throttle cross-window parent sync to ~20Hz (50ms) so high-refresh local interpolation
        // stays silky smooth without thrashing the main window on every 120Hz frame.
        if now - self.last_parent_tick_ms >= 50.0 {
            self.last_parent_tick_ms = now;
            let parent = self.parent.clone();
            window.defer(cx, move |_, cx| {
                let _ = parent.update(cx, |pv, _| {
                    pv.tick_live_for_desktop();
                });
            });
        }

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
                PopoverAction::DismissCascadeIfInactive => {
                    let any_active = [
                        self.more_popover,
                        self.display_popover,
                        self.color_popover,
                        self.lines_popover,
                    ]
                    .into_iter()
                    .flatten()
                    .any(|h| h.update(cx, |_, w, _| w.is_window_active()).unwrap_or(false));
                    if !any_active {
                        self.close_popovers(cx);
                    }
                }
                PopoverAction::ReturnToMain => {
                    let wb = window.bounds();
                    self.cfg
                        .set_pos(f32::from(wb.origin.x), f32::from(wb.origin.y));
                    self.cfg
                        .set_size(f32::from(wb.size.width), f32::from(wb.size.height));
                    self.bridge.set_cfg(self.cfg.clone());
                    let saved_cfg = self.cfg.clone();
                    self.close_popovers(cx);
                    let parent = self.parent.clone();
                    let _ = parent.update(cx, |pv, _| {
                        pv.save_desktop_lyrics_cfg(&saved_cfg);
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
        // High-refresh local clock interpolation so word sweep & springs run at native 60/120/144/240Hz
        // even when background snapshot sync ticks at 80ms.
        let live_pos_ms = if snap.playing {
            let elapsed = snap.sampled_at.elapsed().as_secs_f64() * 1000.0;
            snap.position_ms + elapsed.clamp(0.0, 250.0)
        } else {
            snap.position_ms
        };
        let track_diff =
            snap.track_title != self.last_track_title || snap.track_artist != self.last_track_artist;
        let track_changed =
            (!self.last_track_title.is_empty() || !self.last_track_artist.is_empty()) && track_diff;
        if track_diff {
            self.last_track_title.clear();
            self.last_track_title.push_str(&snap.track_title);
            self.last_track_artist.clear();
            self.last_track_artist.push_str(&snap.track_artist);
        }
        let mut amll_snap_instant = track_changed;
        if track_changed {
            self.last_live_pos_ms = None;
            self.amll_row_springs.clear();
            self.amll_bg_alpha.clear();
            self.amll_row_scales.clear();
            self.amll_prev_rows.clear();
            self.amll_exiting.clear();
            self.vert_col_springs.clear();
            self.vert_col_alpha.clear();
            self.vert_col_scrolls.clear();
            self.vert_multi_cache.clear();
            self.vert_prev_cols.clear();
            self.vert_exiting_cols.clear();
        }
        if let Some(prev_pos) = self.last_live_pos_ms {
            if (live_pos_ms - prev_pos).abs() > 1500.0 {
                amll_snap_instant = true;
            }
        }
        self.last_live_pos_ms = Some(live_pos_ms);
        if snap.active_row_index != self.last_row_idx {
            let prev_idx = self.last_row_idx;
            let first = prev_idx.is_none();
            self.last_row_idx = snap.active_row_index;
            self.line_fade_start_ms = if first { now - 1000.0 } else { now };
            self.vert_main_scroll = 0.0;
            self.vert_trans_scroll = 0.0;
            self.horiz_main_scroll = 0.0;
            self.horiz_trans_scroll = 0.0;
            if first && self.amll_prev_rows.is_empty() && self.vert_prev_cols.is_empty() {
                amll_snap_instant = true;
            }
        }
        let line_fade = ((now - self.line_fade_start_ms) / 180.0).clamp(0.0, 1.0) as f32;

        self.play_morph
            .set_target_position(if snap.playing { 1.0 } else { 0.0 });
        self.play_morph.update(dt / 1000.0);
        let play_morph = self.play_morph.position().clamp(0.0, 1.0) as f32;

        self.interlude_dots.set_playing(snap.playing);
        self.interlude_dots.sync_clock(live_pos_ms);
        if snap.active_row.is_none() {
            if let Some((s, e)) = snap.interlude_range {
                if self.last_interlude != Some((s, e)) {
                    self.last_interlude = Some((s, e));
                    self.interlude_dots
                        .set_interlude(s, e, live_pos_ms, false, 0);
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
                self.close_display_popover(cx);
            }
        }
        if let Some(h) = self.display_popover {
            if !open_wins.contains(&h) {
                self.display_popover = None;
                self.bridge.display_popover_open.store(false, Ordering::SeqCst);
                self.bridge.display_popover_side.store(0, Ordering::SeqCst);
                self.close_color_popover(cx);
                self.close_lines_popover(cx);
            }
        }
        if let Some(h) = self.color_popover {
            if !open_wins.contains(&h) {
                self.color_popover = None;
                self.bridge.color_popover_open.store(false, Ordering::SeqCst);
                self.bridge.color_popover_side.store(0, Ordering::SeqCst);
            }
        }
        if let Some(h) = self.lines_popover {
            if !open_wins.contains(&h) {
                self.lines_popover = None;
                self.bridge.lines_popover_open.store(false, Ordering::SeqCst);
                self.bridge.lines_popover_side.store(0, Ordering::SeqCst);
            }
        }
        if let Some(h) = self.picker_popover {
            if !open_wins.contains(&h) {
                self.picker_popover = None;
            }
        }
        let any_popover_open = self.volume_popover.is_some()
            || self.more_popover.is_some()
            || self.display_popover.is_some()
            || self.color_popover.is_some()
            || self.lines_popover.is_some()
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
        let shaped_active = if orient == DesktopOrientation::Vertical {
            snap.active_row.as_ref().and_then(|row| {
                self.shape_single_line(row, &snap.display, font_size, horiz_avail_w, auto_scroll)
            })
        } else {
            None
        };
        // Reuse main window's AMLL multi-line layout: full row list with harmony (`is_bg`),
        // duet left/right alignment (`r.duet`), and per-line staggered `Spring` matching `springFeel`.
        // Keep lyric bounds fixed on hover so hovering never shrinks desktop lyrics.
        let amll_has_duet = snap.rows.iter().any(|r| r.is_duet);
        let amll_area_top = 8.0_f32;
        let amll_area_h = (win_h - 16.0).max(52.0);
        struct AmllMultiRow {
            idx: usize,
            row: Row,
            shaped: Arc<RowLines>,
            y: f32,
            bg_alpha: f32,
            scale: f32,
            exiting: bool,
            exit_dim: f32,
        }
        // Inline interlude dots Y coordinate for multi-line horizontal AMLL (squeezed between lines, never clearing screen).
        let mut amll_inline_interlude_y: Option<f32> = None;
        let in_interlude = interlude_snap.is_some() || snap.interlude_range.is_some();
        let amll_multi: Vec<AmllMultiRow> = if orient == DesktopOrientation::Horizontal
            && !snap.rows.is_empty()
            && (snap.active_row.is_some() || (display_lines > 1 && in_interlude))
        {
            if amll_snap_instant || !snap.display.enable_spring {
                self.amll_exiting.clear();
                self.amll_prev_rows.clear();
            }
            let max_lines = display_lines.clamp(1, 3) as usize;
            let selected_indices = if max_lines > 1 && in_interlude {
                select_interlude_multi_indices(&snap.rows, snap.active_row_index, live_pos_ms, max_lines)
            } else if max_lines == 1 {
                snap.active_row_index
                    .filter(|&i| i < snap.rows.len() && !snap.rows[i].text.trim().is_empty())
                    .map(|i| vec![i])
                    .unwrap_or_else(|| {
                        select_amll_multi_indices(&snap.rows, snap.active_row_index, live_pos_ms, 1)
                    })
            } else {
                select_amll_multi_indices(&snap.rows, snap.active_row_index, live_pos_ms, max_lines)
            };
            // Stable fit_scale based on fixed slot budget so font size never jumps on line transitions.
            let fit_scale = if max_lines == 1 {
                1.0
            } else {
                let per_row_slots = if show_trans { 1.62 } else { 1.08 };
                let avail_per_slot = (amll_area_h / (max_lines as f32 * per_row_slots)).max(18.0);
                (avail_per_slot / (font_size * 1.25)).clamp(0.42, 1.0)
            };
            let bg_scale = snap.display.bg_scale.clamp(0.55, 0.90);
            let gap = 6.0_f32;
            let spring_params = desktop_spring_params(snap.display.spring_feel);
            let base_row_fs = (font_size * fit_scale).clamp(13.0, MAX_FONT);

            let mut shaped_window: Vec<(usize, Row, Arc<RowLines>, f32, f32, f32)> =
                Vec::with_capacity(selected_indices.len());
            for &idx in &selected_indices {
                let r = &snap.rows[idx];
                if r.text.trim().is_empty() {
                    continue;
                }
                let is_singing = live_pos_ms >= r.start_ms - 80.0 && live_pos_ms <= r.end_ms + 260.0;
                let is_active = Some(idx) == snap.active_row_index || is_singing;
                let bg_target = 1.0_f32;
                let init_alpha = if amll_snap_instant || !snap.display.enable_spring || self.amll_prev_rows.is_empty() {
                    1.0
                } else {
                    0.0
                };
                let bg_alpha = {
                    let bg_a = self.amll_bg_alpha.entry(idx).or_insert(init_alpha);
                    let step = (dt as f32 / 190.0).clamp(0.0, 1.0);
                    *bg_a = (*bg_a + step).min(bg_target);
                    smoothstep01(*bg_a)
                };
                let target_scale = if r.is_bg {
                    bg_scale
                } else if is_active || max_lines == 1 {
                    1.0
                } else {
                    0.90
                };
                let direct_sc = amll_snap_instant || !snap.display.enable_spring;
                let row_scale = {
                    let sc = self
                        .amll_row_scales
                        .entry(idx)
                        .or_insert_with(|| crate::player::model::RowMotion::new(target_scale as f64));
                    sc.step(target_scale as f64, direct_sc, 0.0, dt, spring_params) as f32
                };
                // Keep `auto_scroll` invariant across active/inactive transitions so shaper cache hits 100% (zero frame drop).
                if let Some(s) = self.shape_single_line(r, &snap.display, base_row_fs, horiz_avail_w, auto_scroll) {
                    let row_tfs = (s.font_size * 0.58).clamp(10.5, 26.0);
                    let th = if show_trans && !r.translation.trim().is_empty() {
                        2.0 + row_tfs * 1.18
                    } else {
                        0.0
                    };
                    let total_row_h = s.line_height + th + gap;
                    shaped_window.push((idx, r.clone(), s, total_row_h, bg_alpha, row_scale));
                }
            }

            // If multi-line mode is in an interlude, reserve an inline slot for the 3-dot breather
            // between the last finished row and the next upcoming row instead of clearing lyrics.
            let inline_dot_slot = if max_lines > 1 {
                interlude_snap.as_ref().map(|isnap| {
                    let base_d = (base_row_fs * 0.28 * snap.display.dot_scale).clamp(7.0, 16.0);
                    let h = (base_d * 1.8 * isnap.opacity as f32).max(0.0);
                    let insert_pos = shaped_window
                        .iter()
                        .position(|(_, r, ..)| r.start_ms > live_pos_ms)
                        .unwrap_or(shaped_window.len());
                    (insert_pos, h)
                })
            } else {
                None
            };
            let extra_dot_h = inline_dot_slot.map(|(_, h)| h).unwrap_or(0.0);

            // Vertically center the strictly <= N selected rows (+ optional inline interlude slot).
            let total_stack_h: f32 = (shaped_window
                .iter()
                .map(|(_, _, _, h, _, _)| *h)
                .sum::<f32>()
                + extra_dot_h
                - gap)
                .max(0.0);
            let mut target_ys = vec![0.0_f32; shaped_window.len()];
            let mut y_cur = if max_lines == 1 {
                amll_area_top + ((amll_area_h - total_stack_h) * 0.5).max(0.0)
            } else {
                // Anchor multi-line stack using the fixed `max_lines` slot budget so adding/removing
                // a 2nd/3rd harmony row never jerks existing rows vertically.
                let nominal_lh = base_row_fs * snap.display.line_height_em.max(1.15);
                let nominal_th = if show_trans {
                    2.0 + (base_row_fs * 0.58).clamp(10.5, 26.0) * 1.18
                } else {
                    0.0
                };
                let budget_h =
                    (max_lines as f32 * (nominal_lh + nominal_th + gap) - gap).max(0.0);
                let centered = amll_area_top + ((amll_area_h - budget_h) * 0.5).max(0.0);
                let max_top = amll_area_top + (amll_area_h - total_stack_h).max(0.0);
                centered.min(max_top)
            };
            for (k, (_, _, _, h, _, _)) in shaped_window.iter().enumerate() {
                if let Some((ins_k, dot_h)) = inline_dot_slot {
                    if k == ins_k && dot_h > 0.5 {
                        amll_inline_interlude_y = Some(y_cur + dot_h * 0.45);
                        y_cur += dot_h;
                    }
                }
                target_ys[k] = y_cur;
                y_cur += *h;
            }
            if let Some((ins_k, dot_h)) = inline_dot_slot {
                if ins_k >= shaped_window.len() && dot_h > 0.5 {
                    amll_inline_interlude_y = Some(y_cur + dot_h * 0.45);
                }
            }

            // Keep any removed row briefly so top rows glide upward and middle/trailing rows fade in place.
            let prev_first_idx = self.amll_prev_rows.first().map(|(i, ..)| *i);
            let first_new_idx = selected_indices.first().copied().unwrap_or(0);
            let first_target_y = target_ys.first().copied().unwrap_or(amll_area_top);
            if !amll_snap_instant && snap.display.enable_spring {
                let prev = std::mem::take(&mut self.amll_prev_rows);
                let drift_frac = if max_lines == 1 { 0.36 } else { 0.56 };
                for (p_idx, p_shaped, p_h, p_dim) in prev {
                    if !selected_indices.contains(&p_idx)
                        && !self.amll_exiting.iter().any(|(ei, ..)| *ei == p_idx)
                    {
                        if let Some(p_row) = snap.rows.get(p_idx) {
                            let exit_y = if p_idx < first_new_idx {
                                first_target_y - p_h * drift_frac
                            } else {
                                self.amll_row_springs
                                    .get(&p_idx)
                                    .map(|m| m.position() as f32)
                                    .unwrap_or(first_target_y)
                            };
                            self.amll_exiting.push((
                                p_idx,
                                p_row.clone(),
                                p_shaped,
                                exit_y,
                                1.0,
                                p_dim,
                            ));
                        }
                    }
                }
            }
            self.amll_exiting.retain(|(ei, ..)| !selected_indices.contains(ei));
            self.amll_prev_rows = shaped_window
                .iter()
                .map(|(i, r, s, h, bg_a, _)| {
                    let is_act = Some(*i) == snap.active_row_index
                        || (live_pos_ms >= r.start_ms - 80.0 && live_pos_ms <= r.end_ms + 260.0);
                    let base_d = if is_act {
                        if r.is_bg { 0.88 } else { 1.0 }
                    } else if r.is_bg {
                        0.50
                    } else {
                        snap.display.bg_opacity.max(0.54).clamp(0.54, 0.72)
                    };
                    (*i, Arc::clone(s), *h, base_d * *bg_a)
                })
                .collect();

            let mut out = Vec::with_capacity(shaped_window.len() + self.amll_exiting.len());
            for (e_idx, e_row, e_shaped, e_ty, e_alpha, e_start_dim) in &mut self.amll_exiting {
                *e_alpha = (*e_alpha - (dt as f32 / 360.0)).max(0.0);
                let sp = self
                    .amll_row_springs
                    .entry(*e_idx)
                    .or_insert_with(|| crate::player::model::RowMotion::new(*e_ty as f64));
                let cur_y = sp.step(*e_ty as f64, false, 0.0, dt, spring_params) as f32;
                let eased_a = smoothstep01(*e_alpha);
                if eased_a > 0.01 {
                    let base_sc = self
                        .amll_row_scales
                        .get(e_idx)
                        .map(|m| m.position() as f32)
                        .unwrap_or(0.96);
                    let sc = base_sc * (0.94 + 0.06 * eased_a);
                    out.push(AmllMultiRow {
                        idx: *e_idx,
                        row: e_row.clone(),
                        shaped: Arc::clone(e_shaped),
                        y: cur_y,
                        bg_alpha: eased_a,
                        scale: sc,
                        exiting: true,
                        exit_dim: *e_start_dim,
                    });
                }
            }
            self.amll_exiting.retain(|(_, _, _, _, a, _)| *a > 0.01);

            for (k, (idx, row, shaped, row_h, bg_alpha, row_scale)) in shaped_window.into_iter().enumerate() {
                let ty = target_ys[k] as f64;
                let sp = self.amll_row_springs.entry(idx).or_insert_with(|| {
                    let start_y = if amll_snap_instant || !snap.display.enable_spring {
                        ty
                    } else if prev_first_idx.is_some_and(|pf| idx < pf) {
                        ty - (row_h as f64) * 0.45
                    } else {
                        ty + (row_h as f64) * 0.45
                    };
                    crate::player::model::RowMotion::new(start_y)
                });
                let direct = amll_snap_instant || !snap.display.enable_spring;
                let delay = if direct || max_lines <= 1 { 0.0 } else { (k as f64) * 0.018 };
                let cur_y = sp.step(ty, direct, delay, dt, spring_params) as f32;
                out.push(AmllMultiRow {
                    idx,
                    row,
                    shaped,
                    y: cur_y,
                    bg_alpha,
                    scale: row_scale,
                    exiting: false,
                    exit_dim: 1.0,
                });
            }
            let exiting_ids: Vec<usize> = self.amll_exiting.iter().map(|(i, ..)| *i).collect();
            self.amll_row_springs
                .retain(|i, _| selected_indices.contains(i) || exiting_ids.contains(i));
            self.amll_bg_alpha
                .retain(|i, _| selected_indices.contains(i));
            self.amll_row_scales
                .retain(|i, _| selected_indices.contains(i) || exiting_ids.contains(i));
            out
        } else {
            self.amll_exiting.clear();
            self.amll_prev_rows.clear();
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
            let active_horiz = if !amll_multi.is_empty() {
                amll_multi
                    .iter()
                    .find(|m| Some(m.idx) == snap.active_row_index && !m.row.is_bg)
                    .map(|m| (&m.row, &m.shaped))
            } else {
                snap.active_row.as_ref().zip(shaped_active.as_ref())
            };
            if let Some((row, shaped)) = active_horiz {
                if auto_scroll {
                    let natural_w = shaped
                        .lines
                        .first()
                        .map(|l| f32::from(l.layout.width))
                        .unwrap_or(0.0);
                    let max_sx = (natural_w - horiz_avail_w).max(0.0);
                    let prog = paint::line_progress(live_pos_ms, row.start_ms, row.duration_ms())
                        .clamp(0.0, 1.0);
                    let adj = ((prog - 0.10) / 0.78).clamp(0.0, 1.0);
                    let target_sx = max_sx * smoothstep01(adj);
                    self.horiz_main_scroll += (target_sx - self.horiz_main_scroll) * scroll_alpha;
                    if (self.horiz_main_scroll - target_sx).abs() < 0.2 {
                        self.horiz_main_scroll = target_sx;
                    }
                    horiz_main_scroll = self.horiz_main_scroll;

                    if show_trans && !row.translation.trim().is_empty() {
                        let trans_fs = if !amll_multi.is_empty() {
                            (shaped.font_size * 0.58).clamp(10.5, 26.0)
                        } else {
                            (shaped.font_size * 0.62).clamp(12.0, 40.0)
                        };
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

        // Precompute vertical multi-line column positions (with smooth horizontal slide springs
        // and independent per-column overflow scroll using live_pos_ms).
        struct VertMultiCol {
            idx: usize,
            row: Row,
            slot_left: f32,
            slot_w: f32,
            col_cx: f32,
            trans_cx: Option<f32>,
            col_fs: f32,
            col_tfs: f32,
            alpha: f32,
            scroll_y: f32,
            measured_main: Arc<(Vec<VerticalToken>, Vec<f32>, f32)>,
            measured_trans: Option<Arc<(Vec<VerticalToken>, Vec<f32>, f32)>>,
        }
        let mut vert_inline_interlude_x: Option<f32> = None;
        let vert_multi_cols: Vec<VertMultiCol> = if orient == DesktopOrientation::Vertical
            && display_lines > 1
            && !snap.rows.is_empty()
        {
            if amll_snap_instant || !snap.display.enable_spring {
                self.vert_exiting_cols.clear();
                self.vert_prev_cols.clear();
            }
            let max_lines = display_lines.clamp(1, 3) as usize;
            let v_indices = if in_interlude {
                select_interlude_multi_indices(&snap.rows, snap.active_row_index, live_pos_ms, max_lines)
            } else {
                select_amll_multi_indices(&snap.rows, snap.active_row_index, live_pos_ms, max_lines)
            };
            let v_area_left = 8.0_f32;
            let v_area_w = (win_w - 16.0).max(80.0);
            // Fixed slot width derived from `max_lines` (not `v_indices.len()`) so column count
            // changes (2 <-> 3) never jump font size, invalidate measurement cache, or jerk all columns.
            let slot_w = (v_area_w / max_lines as f32).max(28.0);
            // Screen-side adaptive column direction:
            // - Window on right half (`trans_right_t >= 0.5`): LEFTMOST col = current active row, upcoming -> right.
            // - Window on left half (`trans_right_t < 0.5`): RIGHTMOST col = current active row, upcoming -> left.
            let on_right_half = trans_right_t >= 0.5;
            let col_fs = (font_size.min(slot_w * if show_trans { 0.42 } else { 0.64 }))
                .clamp(13.0, MAX_FONT);
            let col_tfs = (col_fs * 0.58).clamp(11.0, 24.0);
            let spring_params = desktop_spring_params(snap.display.spring_feel);
            let vis_h = (win_h - 36.0).max(60.0);
            let prev_first_idx = self.vert_prev_cols.first().map(|(i, ..)| *i);
            let first_new_idx = v_indices.first().copied().unwrap_or(0);
            let first_slot_cx = if on_right_half {
                v_area_left + slot_w * 0.5
            } else {
                v_area_left + v_area_w - slot_w * 0.5
            };
            if !amll_snap_instant && snap.display.enable_spring {
                let prev = std::mem::take(&mut self.vert_prev_cols);
                for (p_idx, p_fs, p_tfs, p_scroll, p_sw, p_alpha, p_measured) in prev {
                    if !v_indices.contains(&p_idx)
                        && !self.vert_exiting_cols.iter().any(|(ei, ..)| *ei == p_idx)
                    {
                        if let Some(p_row) = snap.rows.get(p_idx) {
                            let exit_x = if p_idx < first_new_idx {
                                first_slot_cx + if on_right_half { -p_sw * 0.42 } else { p_sw * 0.42 }
                            } else {
                                self.vert_col_springs
                                    .get(&p_idx)
                                    .map(|m| m.position() as f32)
                                    .unwrap_or(first_slot_cx)
                            };
                            self.vert_exiting_cols.push((
                                p_idx,
                                p_row.clone(),
                                exit_x,
                                p_fs,
                                p_tfs,
                                p_scroll,
                                1.0,
                                p_alpha,
                                p_measured,
                            ));
                        }
                    }
                }
            }
            self.vert_exiting_cols.retain(|(ei, ..)| !v_indices.contains(ei));

            let mut cols = Vec::with_capacity(v_indices.len() + self.vert_exiting_cols.len());
            for (e_idx, e_row, e_tx, e_fs, e_tfs, e_scroll, e_alpha, e_start_a, e_measured) in
                &mut self.vert_exiting_cols
            {
                *e_alpha = (*e_alpha - (dt as f32 / 360.0)).max(0.0);
                let sp = self
                    .vert_col_springs
                    .entry(*e_idx)
                    .or_insert_with(|| crate::player::model::RowMotion::new(*e_tx as f64));
                let cur_cx = sp.step(*e_tx as f64, false, 0.0, dt, spring_params) as f32;
                let eased_a = smoothstep01(*e_alpha);
                if eased_a > 0.01 {
                    cols.push(VertMultiCol {
                        idx: *e_idx,
                        row: e_row.clone(),
                        slot_left: cur_cx - slot_w * 0.5,
                        slot_w,
                        col_cx: cur_cx,
                        trans_cx: None,
                        col_fs: *e_fs,
                        col_tfs: *e_tfs,
                        alpha: eased_a * *e_start_a,
                        scroll_y: *e_scroll,
                        measured_main: Arc::clone(e_measured),
                        measured_trans: None,
                    });
                }
            }
            self.vert_exiting_cols.retain(|(_, _, _, _, _, _, a, _, _)| *a > 0.01);

            let mut next_prev = Vec::with_capacity(v_indices.len());
            let direct = amll_snap_instant || !snap.display.enable_spring;
            for (k, &v_idx) in v_indices.iter().enumerate() {
                let r = &snap.rows[v_idx];
                if r.text.trim().is_empty() {
                    continue;
                }
                let is_active = Some(v_idx) == snap.active_row_index
                    || (live_pos_ms >= r.start_ms - 80.0 && live_pos_ms <= r.end_ms + 260.0);
                let init_alpha = if direct || prev_first_idx.is_none() { 1.0 } else { 0.0 };
                let fade_in = {
                    let va = self.vert_col_alpha.entry(v_idx).or_insert(init_alpha);
                    let step = (dt as f32 / 190.0).clamp(0.0, 1.0);
                    *va = (*va + step).min(1.0);
                    smoothstep01(*va)
                };
                let alpha = (if is_active {
                    if r.is_bg { 0.88 } else { 1.0 }
                } else if r.is_bg {
                    0.48
                } else {
                    snap.display.bg_opacity.max(0.54).clamp(0.54, 0.72)
                }) * fade_in;
                let slot_left = if on_right_half {
                    v_area_left + k as f32 * slot_w
                } else {
                    v_area_left + v_area_w - (k + 1) as f32 * slot_w
                };
                let target_cx = slot_left + slot_w * 0.5;
                if interlude_snap.is_some() && vert_inline_interlude_x.is_none() && r.start_ms > live_pos_ms {
                    vert_inline_interlude_x = Some(if k == 0 {
                        target_cx + if on_right_half { -slot_w * 0.42 } else { slot_w * 0.42 }
                    } else {
                        target_cx + if on_right_half { -slot_w * 0.5 } else { slot_w * 0.5 }
                    });
                }
                let sp = self.vert_col_springs.entry(v_idx).or_insert_with(|| {
                    let start_x = if direct {
                        target_cx as f64
                    } else {
                        let from_leading = prev_first_idx.is_some_and(|pf| v_idx < pf);
                        let delta = if from_leading { -slot_w * 0.42 } else { slot_w * 0.42 };
                        (target_cx + if on_right_half { delta } else { -delta }) as f64
                    };
                    crate::player::model::RowMotion::new(start_x)
                });
                let delay = if direct { 0.0 } else { (k as f64) * 0.018 };
                let cur_cx = sp.step(target_cx as f64, direct, delay, dt, spring_params) as f32;
                let has_trans = show_trans && !r.translation.trim().is_empty() && slot_w >= 46.0;
                let (main_cx, trans_cx) = if has_trans {
                    let sep = (slot_w * 0.21).max(col_fs * 0.62).min(slot_w * 0.30);
                    if on_right_half {
                        (cur_cx - sep * 0.85, Some(cur_cx + sep * 0.95))
                    } else {
                        (cur_cx + sep * 0.85, Some(cur_cx - sep * 0.95))
                    }
                } else {
                    (cur_cx, None)
                };
                // Keep main column font size invariant across active/inactive transitions so
                // vertical measurement & glyph caches hit 100% (zero frame drops on column shift).
                let eff_fs = if r.is_bg {
                    (col_fs * snap.display.bg_scale.clamp(0.55, 0.90)).max(11.0)
                } else {
                    col_fs
                };
                // Cached vertical tokenization & measurement per (row_idx, font_bits, weight_bits).
                let fs_bits = eff_fs.to_bits();
                let tfs_bits = col_tfs.to_bits();
                let wt_bits = snap.display.weight.to_bits();
                let entry = self.vert_multi_cache.entry(v_idx).or_insert_with(|| {
                    let toks = tokenize_vertical_row(r);
                    let (st, th) = measure_vertical_steps(window, &toks, eff_fs, snap.display.weight);
                    (fs_bits, wt_bits, Arc::new((toks, st, th)), None)
                });
                if entry.0 != fs_bits || entry.1 != wt_bits {
                    let toks = tokenize_vertical_row(r);
                    let (st, th) = measure_vertical_steps(window, &toks, eff_fs, snap.display.weight);
                    *entry = (fs_bits, wt_bits, Arc::new((toks, st, th)), None);
                }
                let measured_main = Arc::clone(&entry.2);
                let measured_trans = if has_trans {
                    let need_t = entry.3.as_ref().is_none_or(|(tb, _)| *tb != tfs_bits);
                    if need_t {
                        let trans_row = Row {
                            group: r.group,
                            text: r.translation.clone(),
                            words: Vec::new(),
                            translation: String::new(),
                            romanization: String::new(),
                            start_ms: r.start_ms,
                            end_ms: r.end_ms,
                            word_timed: false,
                            is_bg: false,
                            is_duet: false,
                        };
                        let t_toks = tokenize_vertical_row(&trans_row);
                        let (t_st, t_th) =
                            measure_vertical_steps(window, &t_toks, col_tfs, snap.display.weight);
                        entry.3 = Some((tfs_bits, Arc::new((t_toks, t_st, t_th))));
                    }
                    entry.3.as_ref().map(|(_, m)| Arc::clone(m))
                } else {
                    None
                };
                let target_scroll = if auto_scroll && measured_main.2 > vis_h {
                    compute_vertical_scroll_target(
                        &measured_main.0,
                        &measured_main.1,
                        measured_main.2,
                        vis_h,
                        live_pos_ms,
                        r.start_ms,
                        r.duration_ms(),
                        r.word_timed,
                    )
                } else {
                    0.0
                };
                let sc_entry = self.vert_col_scrolls.entry(v_idx).or_insert(target_scroll);
                *sc_entry += (target_scroll - *sc_entry) * scroll_alpha;
                if (*sc_entry - target_scroll).abs() < 0.2 {
                    *sc_entry = target_scroll;
                }
                next_prev.push((
                    v_idx,
                    eff_fs,
                    col_tfs,
                    *sc_entry,
                    slot_w,
                    alpha,
                    Arc::clone(&measured_main),
                ));
                cols.push(VertMultiCol {
                    idx: v_idx,
                    row: r.clone(),
                    slot_left: cur_cx - slot_w * 0.5,
                    slot_w,
                    col_cx: main_cx,
                    trans_cx,
                    col_fs: eff_fs,
                    col_tfs,
                    alpha,
                    scroll_y: *sc_entry,
                    measured_main,
                    measured_trans,
                });
            }
            self.vert_prev_cols = next_prev;
            let exit_v_ids: Vec<usize> = self.vert_exiting_cols.iter().map(|(i, ..)| *i).collect();
            self.vert_col_springs
                .retain(|i, _| v_indices.contains(i) || exit_v_ids.contains(i));
            self.vert_col_alpha.retain(|i, _| v_indices.contains(i));
            self.vert_col_scrolls.retain(|i, _| v_indices.contains(i));
            self.vert_multi_cache.retain(|i, _| v_indices.contains(i));
            cols
        } else {
            self.vert_col_springs.clear();
            self.vert_col_alpha.clear();
            self.vert_col_scrolls.clear();
            self.vert_exiting_cols.clear();
            self.vert_prev_cols.clear();
            Vec::new()
        };

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
        let amll_animating = !self.amll_exiting.is_empty()
            || !self.vert_exiting_cols.is_empty()
            || self
                .amll_row_springs
                .values()
                .any(crate::player::model::RowMotion::pending)
            || self
                .amll_row_scales
                .values()
                .any(crate::player::model::RowMotion::pending)
            || self
                .vert_col_springs
                .values()
                .any(crate::player::model::RowMotion::pending);
        let animating = snap.playing
            || hovering
            || self.hover_alpha > 0.001
            || (self.bar_side_blend - target_bar_side).abs() > 0.001
            || (self.trans_right_blend - target_trans_right).abs() > 0.001
            || line_fade < 0.999
            || amll_animating
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

                        // 2. Native AMLL horizontal/vertical lyrics + inline InterludeDots OR fallback
                        let render_horiz_multi =
                            orient == DesktopOrientation::Horizontal && !amll_multi.is_empty();
                        let render_vert_multi =
                            orient == DesktopOrientation::Vertical && !vert_multi_cols.is_empty();
                        if render_horiz_multi {
                                        let pad_x = 24.0_f32;
                                        let origin_x_box = (win_w - horiz_avail_w) * 0.5;
                                        window.with_content_mask(
                                            Some(ContentMask {
                                                bounds: gpui_kit::bounds(
                                                    point(px(0.0), px(0.0)),
                                                    gpui_kit::size(px(win_w), px(win_h)),
                                                ),
                                            }),
                                            |window| {
                                                for item in &amll_multi {
                                                    let r = &item.row;
                                                    let s = &item.shaped;
                                                    let cur_y = item.y;
                                                    // Stable translation font size (matches layout measurement) so GPUI's
                                                    // text layout & glyph caches hit 100% during scale spring transitions.
                                                    let row_tfs = (s.font_size * 0.58).clamp(10.5, 26.0);
                                                    let th = if show_trans && !r.translation.trim().is_empty() {
                                                        2.0 + row_tfs * 1.18
                                                    } else {
                                                        0.0
                                                    };
                                                    let row_bot = cur_y + s.line_height + th;
                                                    if row_bot < -8.0 || cur_y > win_h + 8.0 {
                                                        continue;
                                                    }
                                                    let edge_fade_top = if cur_y < 0.0 {
                                                        ((cur_y + 36.0) / 36.0).clamp(0.0, 1.0)
                                                    } else {
                                                        1.0
                                                    };
                                                    let edge_fade_bot = if row_bot > win_h {
                                                        (((win_h + 36.0) - row_bot) / 36.0).clamp(0.0, 1.0)
                                                    } else {
                                                        1.0
                                                    };
                                                    let edge_fade = edge_fade_top.min(edge_fade_bot);
                                                    let is_main_active = !item.exiting && Some(item.idx) == snap.active_row_index && !r.is_bg;
                                                    let is_active = !item.exiting
                                                        && (Some(item.idx) == snap.active_row_index
                                                            || (live_pos_ms >= r.start_ms - 80.0
                                                                && live_pos_ms <= r.end_ms + 260.0));
                                                    let dim = if item.exiting {
                                                        item.exit_dim * item.bg_alpha * edge_fade
                                                    } else {
                                                        (if is_active {
                                                            if r.is_bg { 0.88 } else { 1.0 }
                                                        } else if r.is_bg {
                                                            0.50
                                                        } else {
                                                            snap.display.bg_opacity.max(0.54).clamp(0.54, 0.72)
                                                        }) * item.bg_alpha
                                                            * edge_fade
                                                    };
                                                    if dim < 0.02 {
                                                        continue;
                                                    }
                                                    let mut row_light_val = paint::build_row_light(
                                                        r,
                                                        s,
                                                        live_pos_ms,
                                                        is_active,
                                                        dim,
                                                        &snap.display,
                                                    );
                                                    // Keep active desktop lyric highlights crisp pure color (1.0 alpha),
                                                    // and raise unplayed portion slightly so upcoming words stay readable.
                                                    if is_active && !r.is_bg {
                                                        row_light_val.bright = dim.clamp(0.0, 1.0);
                                                        row_light_val.dark = row_light_val.dark.max(0.52 * dim);
                                                        row_light_val.base = row_light_val.dark;
                                                    }
                                                    let natural_w = s
                                                        .lines
                                                        .first()
                                                        .map(|l| f32::from(l.layout.width))
                                                        .unwrap_or(0.0);
                                                    let row_overflow = is_main_active && auto_scroll && natural_w > horiz_avail_w;
                                                    let base_align = if display_lines > 1 && amll_has_duet {
                                                        if r.is_duet { LineAlign::Right } else { LineAlign::Left }
                                                    } else {
                                                        LineAlign::Center
                                                    };
                                                    let (rx, rw, ralign) = if row_overflow {
                                                        (pad_x - horiz_main_scroll, natural_w + 32.0, LineAlign::Left)
                                                    } else {
                                                        (origin_x_box, horiz_avail_w, base_align)
                                                    };
                                                    let draw_row = |window: &mut Window| {
                                                        paint::paint_row_with_shadow(
                                                            window,
                                                            s,
                                                            rx,
                                                            cur_y,
                                                            s.line_height,
                                                            s.font_size,
                                                            &s.words,
                                                            &row_light_val,
                                                            live_pos_ms,
                                                            item.scale,
                                                            WrapperTransform::default(),
                                                            -10000.0,
                                                            10000.0,
                                                            1.0,
                                                            rw,
                                                            ralign,
                                                            0.0,
                                                            DESKTOP_DROP_SHADOW,
                                                            Some(text_color),
                                                        );
                                                    };
                                                    if row_overflow {
                                                        window.with_content_mask(
                                                            Some(ContentMask {
                                                                bounds: gpui_kit::bounds(
                                                                    point(px(pad_x), px(amll_area_top)),
                                                                    gpui_kit::size(px(horiz_avail_w), px(amll_area_h)),
                                                                ),
                                                            }),
                                                            draw_row,
                                                        );
                                                    } else {
                                                        draw_row(window);
                                                    }
                                                    if show_trans && !r.translation.trim().is_empty() {
                                                        let trans_a = (snap.display.trans_current.max(0.78) * dim)
                                                            .clamp(0.0, 1.0);
                                                        if display_lines > 1 && amll_has_duet && !row_overflow {
                                                            let t_text = r.translation.trim();
                                                            let (tw, _) = chrome::measure(window, t_text, row_tfs, 500.0);
                                                            let tx = if r.is_duet {
                                                                (origin_x_box + horiz_avail_w - tw).max(origin_x_box)
                                                            } else {
                                                                origin_x_box
                                                            };
                                                            let col = Hsla::from(gpui_kit::rgb(text_color)).opacity(trans_a);
                                                            chrome::paint_text(window, t_text, tx, cur_y + s.line_height + 2.0, row_tfs, 500.0, col);
                                                        } else {
                                                            paint_horiz_translation(
                                                                window,
                                                                &r.translation,
                                                                win_w,
                                                                cur_y + s.line_height + 2.0,
                                                                row_tfs,
                                                                text_color,
                                                                trans_a,
                                                                is_main_active && auto_scroll,
                                                                if is_main_active { horiz_trans_scroll } else { 0.0 },
                                                            );
                                                        }
                                                    }
                                                }
                                                // Inline interlude breather squeezed between contextual rows in multi-line mode.
                                                if let (Some(iy), Some(isnap)) = (amll_inline_interlude_y, interlude_snap.as_ref()) {
                                                    let base_d = (font_size * 0.24 * snap.display.dot_scale).clamp(6.0, 14.0);
                                                    let base_g = base_d * 0.72;
                                                    let dark_text = is_dark_color(text_color);
                                                    let shadow_rgb = if dark_text { 0xFFFFFF } else { 0x000000 };
                                                    paint::paint_interlude_strip(
                                                        window,
                                                        win_w * 0.5,
                                                        iy,
                                                        base_d * isnap.scale as f32,
                                                        base_g * isnap.scale as f32,
                                                        [
                                                            isnap.dot_opacities[0] as f32,
                                                            isnap.dot_opacities[1] as f32,
                                                            isnap.dot_opacities[2] as f32,
                                                        ],
                                                        isnap.opacity as f32,
                                                        false,
                                                        Some(text_color),
                                                        Some((shadow_rgb, if dark_text { 0.0 } else { 1.5 })),
                                                    );
                                                }
                                            },
                                        );
                        } else if render_vert_multi {
                            for col in &vert_multi_cols {
                                let clip_left = col.slot_left.max(0.0);
                                let clip_right = (col.slot_left + col.slot_w).min(win_w);
                                if clip_right <= clip_left + 1.0 {
                                    continue;
                                }
                                let edge_fade_l = if col.slot_left < 0.0 {
                                    ((col.slot_left + 36.0) / 36.0).clamp(0.0, 1.0)
                                } else {
                                    1.0
                                };
                                let edge_fade_r = if col.slot_left + col.slot_w > win_w {
                                    (((win_w + 36.0) - (col.slot_left + col.slot_w)) / 36.0)
                                        .clamp(0.0, 1.0)
                                } else {
                                    1.0
                                };
                                let col_alpha = col.alpha * edge_fade_l.min(edge_fade_r);
                                if col_alpha < 0.02 {
                                    continue;
                                }
                                let col_mask = ContentMask {
                                    bounds: gpui_kit::bounds(
                                        point(px(clip_left), px(0.0)),
                                        gpui_kit::size(px((clip_right - clip_left).max(1.0)), px(win_h)),
                                    ),
                                };
                                window.with_content_mask(Some(col_mask), |window| {
                                    paint_vertical_text_column(
                                        window,
                                        col.col_cx,
                                        win_h,
                                        &col.row,
                                        if Some(col.idx) == snap.active_row_index {
                                            shaped_active.as_deref()
                                        } else {
                                            None
                                        },
                                        &snap.display,
                                        live_pos_ms,
                                        col.col_fs,
                                        text_color,
                                        col_alpha,
                                        1.0,
                                        col.scroll_y,
                                        Some(col.measured_main.as_ref()),
                                    );
                                    if let Some(trans_cx) = col.trans_cx {
                                        let trans_row = Row {
                                            group: col.row.group,
                                            text: col.row.translation.clone(),
                                            words: Vec::new(),
                                            translation: String::new(),
                                            romanization: String::new(),
                                            start_ms: col.row.start_ms,
                                            end_ms: col.row.end_ms,
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
                                            live_pos_ms,
                                            col.col_tfs,
                                            text_color,
                                            (snap.display.trans_current.max(0.78) * col_alpha)
                                                .clamp(0.0, 1.0),
                                            1.0,
                                            col.scroll_y,
                                            col.measured_trans.as_deref(),
                                        );
                                    }
                                });
                            }
                            if let (Some(ix), Some(isnap)) = (vert_inline_interlude_x, interlude_snap.as_ref()) {
                                let base_d = (font_size * 0.24 * snap.display.dot_scale).clamp(6.0, 14.0);
                                let base_g = base_d * 0.72;
                                let dark_text = is_dark_color(text_color);
                                let shadow_rgb = if dark_text { 0xFFFFFF } else { 0x000000 };
                                paint::paint_interlude_strip(
                                    window,
                                    ix.clamp(vert_area_left + 12.0, vert_area_left + vert_area_w - 12.0),
                                    win_h * 0.5,
                                    base_d * isnap.scale as f32,
                                    base_g * isnap.scale as f32,
                                    [
                                        isnap.dot_opacities[0] as f32,
                                        isnap.dot_opacities[1] as f32,
                                        isnap.dot_opacities[2] as f32,
                                    ],
                                    isnap.opacity as f32,
                                    true,
                                    Some(text_color),
                                    Some((shadow_rgb, if dark_text { 0.0 } else { 1.5 })),
                                );
                            }
                        } else if let (Some(row), Some(shaped)) = (snap.active_row.as_ref(), shaped_active.as_ref()) {
                            if orient == DesktopOrientation::Vertical {
                                    let has_trans =
                                        show_trans && !row.translation.trim().is_empty();
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
                                        live_pos_ms,
                                        vert_eff_main_fs,
                                        text_color,
                                        1.0,
                                        line_fade,
                                        vert_main_scroll,
                                        None,
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
                                            live_pos_ms,
                                            vert_eff_trans_fs,
                                            text_color,
                                            snap.display.trans_current.max(0.75),
                                            line_fade,
                                            vert_trans_scroll,
                                            None,
                                        );
                                    }
                            }
                        } else {
                                if let Some(isnap) = interlude_snap.as_ref() {
                                    let (ix, iy) = match orient {
                                        DesktopOrientation::Horizontal => {
                                            (win_w * 0.5, horiz_area_top + horiz_area_h * 0.5)
                                        }
                                        DesktopOrientation::Vertical => {
                                            (vert_area_left + vert_area_w * 0.5, win_h * 0.5)
                                        }
                                    };
                                    let base_d = (font_size * 0.28 * snap.display.dot_scale).clamp(7.0, 18.0);
                                    let base_g = base_d * 0.72;
                                    let dark_text = is_dark_color(text_color);
                                    let shadow_rgb = if dark_text { 0xFFFFFF } else { 0x000000 };
                                    paint::paint_interlude_strip(
                                        window,
                                        ix,
                                        iy,
                                        base_d * isnap.scale as f32,
                                        base_g * isnap.scale as f32,
                                        [
                                            isnap.dot_opacities[0] as f32,
                                            isnap.dot_opacities[1] as f32,
                                            isnap.dot_opacities[2] as f32,
                                        ],
                                        isnap.opacity as f32,
                                        orient == DesktopOrientation::Vertical,
                                        Some(text_color),
                                        Some((shadow_rgb, if dark_text { 0.0 } else { 1.5 })),
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
                                                None,
                                            );
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

pub(crate) fn snap_and_clamp_drag(
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



/// Strictly forward-monotonic sliding window of at most `max_lines` (1..=3) visible rows:
/// 1. Keeps currently active rows (lead vocal + overlapping next lead + active harmony).
/// 2. Fills remaining slots strictly forward with upcoming rows (never re-inserting an
///    already-finished row back at the top when overlapping vocals finish or at end-of-track).
pub(super) fn select_amll_multi_indices(
    rows: &[Row],
    active_idx: Option<usize>,
    pos_ms: f64,
    max_lines: usize,
) -> Vec<usize> {
    let cap = max_lines.clamp(1, 3);
    if rows.is_empty() {
        return Vec::new();
    }
    let mut selected: Vec<usize> = Vec::with_capacity(cap + 2);
    for (idx, r) in rows.iter().enumerate() {
        if r.text.trim().is_empty() {
            continue;
        }
        // Unified lead/tail window for main & harmony rows so a harmony row never
        // activates earlier than its parent main line and evicts it.
        let active = pos_ms >= r.start_ms - 120.0 && pos_ms <= r.end_ms + 300.0;
        if active {
            if r.is_bg {
                // Ensure an active harmony row never orphans/skips its immediately preceding
                // parent main line (e.g., if TTML harmony starts at or slightly before main).
                if let Some(parent_idx) = rows[..idx]
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, pr)| !pr.is_bg && !pr.text.trim().is_empty())
                    .filter(|(_, pr)| pr.group == r.group || (r.start_ms - pr.start_ms).abs() <= 1500.0)
                    .map(|(pi, _)| pi)
                {
                    if !selected.contains(&parent_idx) && pos_ms <= rows[parent_idx].end_ms + 300.0 {
                        selected.push(parent_idx);
                    }
                }
            }
            if !selected.contains(&idx) {
                selected.push(idx);
            }
        }
    }
    // Only inject `active_idx` if nothing is actively singing, OR if `active_idx` itself is
    // still within its active tail (prevents stale `prev_before_next` fallback from resurrecting
    // an already-finished line once the next line is active).
    if let Some(ai) = active_idx {
        if ai < rows.len()
            && !rows[ai].text.trim().is_empty()
            && !selected.contains(&ai)
            && (selected.is_empty() || pos_ms <= rows[ai].end_ms + 300.0)
        {
            selected.push(ai);
            selected.sort_unstable();
        }
    }
    if selected.is_empty() {
        if let Some(first) = rows.iter().position(|r| !r.is_bg && !r.text.trim().is_empty()) {
            selected.push(first);
        } else {
            return Vec::new();
        }
    }
    if selected.len() >= cap {
        let start = selected.len() - cap;
        return selected[start..].to_vec();
    }

    let last_sel = *selected.last().unwrap();

    // Fill remaining slots strictly forward with upcoming rows (main rows + imminent harmony).
    // If an upcoming harmony row isn't imminent yet (>1.5s away), STOP instead of jumping over
    // it to pull a later main line that would just get pushed back out when the harmony arrives.
    for (idx, r) in rows.iter().enumerate().skip(last_sel + 1) {
        if selected.len() >= cap {
            break;
        }
        if r.text.trim().is_empty() {
            continue;
        }
        if r.is_bg && r.start_ms > pos_ms + 1500.0 {
            break;
        }
        selected.push(idx);
    }

    selected.sort_unstable();
    selected.dedup();
    selected.truncate(cap);
    selected
}

fn desktop_spring_params(feel: crate::player::model::SpringFeel) -> SpringParams {
    match feel {
        crate::player::model::SpringFeel::Lively => SpringParams::new(185.0, 27.0),
        crate::player::model::SpringFeel::Soft => SpringParams::new(92.0, 19.5),
        crate::player::model::SpringFeel::Standard => SpringParams::new(125.0, 22.5),
    }
}

/// Frames `[prev_main (if any), next_main, next_main+1]` during an interlude in multi-line mode
/// so the 3-dot breather sits cleanly between the finished line and the upcoming line.
fn select_interlude_multi_indices(
    rows: &[Row],
    active_idx: Option<usize>,
    pos_ms: f64,
    max_lines: usize,
) -> Vec<usize> {
    let next_idx = rows
        .iter()
        .enumerate()
        .find(|(_, r)| !r.is_bg && !r.text.trim().is_empty() && r.start_ms > pos_ms)
        .map(|(i, _)| i);
    let prev_idx = rows
        .iter()
        .enumerate()
        .rev()
        .find(|(_, r)| !r.is_bg && !r.text.trim().is_empty() && r.end_ms <= pos_ms + 300.0)
        .map(|(i, _)| i);
    let mut v = Vec::with_capacity(max_lines);
    if let Some(pi) = prev_idx {
        v.push(pi);
    }
    if let Some(ni) = next_idx {
        if !v.contains(&ni) && v.len() < max_lines {
            v.push(ni);
        }
        for (idx, r) in rows.iter().enumerate().skip(ni + 1) {
            if v.len() >= max_lines {
                break;
            }
            if r.is_bg || r.text.trim().is_empty() {
                continue;
            }
            v.push(idx);
        }
    }
    if v.is_empty() {
        select_amll_multi_indices(rows, active_idx, pos_ms, max_lines)
    } else {
        v
    }
}
