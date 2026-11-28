//! Minimalist draggable liquid-glass floating control bar for Wallpaper Mode.
//!
//! - Collapsed rest state: sleek glass progress capsule showing live track progress.
//! - Hover / active state: smoothly expands (~400ms) into a 7-button liquid-glass pill:
//!   `[Pin] [Volume] [Prev] [Play/Pause] [Next] [More] [Close]`
//! - Non-topmost by default; Pin button toggles `HWND_TOPMOST` with pure white highlight.
//! - Liquid-glass bubble feedback on buttons; dragging (>4px) moves the whole bar with
//!   edge/center snapping while keeping the bubble held.
//! - Reuses `DesktopVolumePopover` and `DesktopLyricsPickerPopover` (420px dynamic height)
//!   and adds bounded elastic drag (`PressGesture` + `Elastic`) to `WallpaperMorePopover`.

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use gpui_kit::{
    AnyWindowHandle, App, BorderStyle, BoxShadow, Context, Corners, Edges, Hsla,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Render, Styled, Window, bounds, canvas, div, linear_color_stop, linear_gradient,
    point, px, quad, rgb, size,
};
use oly_core::scroll::{Spring, SpringParams};
use oly_ui::chrome;
use oly_ui::color::smoothstep01;
use oly_ui::elastic::{Elastic, PressGesture};
use oly_ui::interaction::{Feedback, state_icon_spring};
use oly_ui::win32_window;

use super::{SharedWallpaperBridge, WallpaperCommand};
use crate::player::desktop_lyrics::{
    DesktopCommand, DesktopLyricsPickerPopover, DesktopLyricsView, DesktopVolumePopover,
    SharedDesktopBridge, snap_and_clamp_drag,
};
use crate::player::icons::{self, Icon};
use crate::player::model::{Box4, ThemeMode};
use crate::player::panel_motion::PanelMotion;
use crate::player::view::{VERSION_HEADER, VERSION_ROW, offset_label, paint_sidecar_glass_frame};

pub const CONTROL_BAR_W: f32 = 308.0;
pub const CONTROL_BAR_H: f32 = 58.0;
const PILL_PAD: f32 = 6.0;
const COLLAPSED_W: f32 = 156.0;
const COLLAPSED_H: f32 = 10.0;
const COLLAPSE_DELAY_MS: f64 = 220.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BarBtn {
    Pin,
    Volume,
    Prev,
    Play,
    Next,
    More,
    Close,
}

#[derive(Clone, Copy)]
struct DragState {
    start_cursor_phys: (i32, i32),
    start_origin_logical: (f32, f32),
    snapped_x: bool,
    snapped_y: bool,
    moved: bool,
}

pub struct WallpaperControlBar {
    wp_bridge: Arc<SharedWallpaperBridge>,
    dl_bridge: Arc<SharedDesktopBridge>,
    configured: bool,
    hwnd: isize,
    pinned: bool,
    expand_spring: Spring,
    hover_leave_ms: Option<f64>,
    play_morph: Spring,
    cursor: Option<(f32, f32)>,
    pressed: Option<BarBtn>,
    drag: Option<DragState>,
    feedback: HashMap<BarBtn, Feedback>,
    hits: Vec<(BarBtn, Box4)>,
    volume_popover: Option<AnyWindowHandle>,
    more_popover: Option<AnyWindowHandle>,
    picker_popover: Option<AnyWindowHandle>,
    cover_popover: Option<AnyWindowHandle>,
    pending_open_picker: Arc<AtomicBool>,
    pending_open_cover: Arc<AtomicBool>,
    more_closed_flag: Arc<AtomicBool>,
    idle_poll_pending: bool,
    started: Instant,
    last_ms: f64,
}

impl WallpaperControlBar {
    pub fn new(
        wp_bridge: Arc<SharedWallpaperBridge>,
        dl_bridge: Arc<SharedDesktopBridge>,
    ) -> Self {
        let mut expand_spring = Spring::new(0.0);
        // Smooth, balanced critically-damped expansion & collapse (~380ms settle).
        expand_spring.update_params(SpringParams::new(110.0, 21.0));
        Self {
            wp_bridge,
            dl_bridge,
            configured: false,
            hwnd: 0,
            pinned: false,
            expand_spring,
            hover_leave_ms: None,
            play_morph: state_icon_spring(0.0),
            cursor: None,
            pressed: None,
            drag: None,
            feedback: HashMap::new(),
            hits: Vec::new(),
            volume_popover: None,
            more_popover: None,
            picker_popover: None,
            cover_popover: None,
            pending_open_picker: Arc::new(AtomicBool::new(false)),
            pending_open_cover: Arc::new(AtomicBool::new(false)),
            more_closed_flag: Arc::new(AtomicBool::new(false)),
            idle_poll_pending: false,
            started: Instant::now(),
            last_ms: 0.0,
        }
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<BarBtn> {
        self.hits
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(b, _)| *b)
    }

    fn close_popovers(&mut self, cx: &mut App) {
        for slot in [
            &mut self.volume_popover,
            &mut self.more_popover,
            &mut self.picker_popover,
            &mut self.cover_popover,
        ] {
            if let Some(h) = slot.take() {
                let _ = h.update(cx, |_, w, _| w.remove_window());
            }
        }
    }

    fn any_popover_alive(&mut self, cx: &mut App) -> bool {
        let mut alive = false;
        for slot in [
            &mut self.volume_popover,
            &mut self.more_popover,
            &mut self.picker_popover,
            &mut self.cover_popover,
        ] {
            if let Some(h) = *slot {
                if h.update(cx, |_, _, _| ()).is_ok() {
                    alive = true;
                } else {
                    *slot = None;
                }
            }
        }
        alive
    }

    fn popover_origin(
        &self,
        anchor: Box4,
        pw: f32,
        ph: f32,
        window: &Window,
        cx: &App,
    ) -> (f32, f32) {
        let wb = window.bounds();
        let wx = f32::from(wb.origin.x);
        let wy = f32::from(wb.origin.y);
        let wh = f32::from(wb.size.height);
        let mut x = wx + anchor.x + (anchor.w - pw) * 0.5;
        let mut y = wy + wh + 4.0;
        if let Some(d) = window.display(cx) {
            let vb = d.visible_bounds();
            let dl = f32::from(vb.origin.x);
            let dt = f32::from(vb.origin.y);
            let dr = dl + f32::from(vb.size.width);
            let db = dt + f32::from(vb.size.height);
            if y + ph > db - 8.0 && wy - ph - 4.0 >= dt + 8.0 {
                y = wy - ph - 4.0;
            }
            x = x.clamp(dl + 8.0, (dr - pw - 8.0).max(dl + 8.0));
            y = y.clamp(dt + 8.0, (db - ph - 8.0).max(dt + 8.0));
        }
        (x, y)
    }

    fn toggle_volume(&mut self, anchor: Box4, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(h) = self.volume_popover.take() {
            if h.update(cx, |_, w, _| w.remove_window()).is_ok() {
                return;
            }
        }
        if self.dl_bridge.was_volume_recently_closed() {
            return;
        }
        self.close_popovers(cx);
        self.dl_bridge.push(DesktopCommand::OpenVolume);
        let (pw, ph) = (
            crate::player::desktop_lyrics::VOLUME_POPOVER_W,
            crate::player::desktop_lyrics::VOLUME_POPOVER_H,
        );
        let pos = self.popover_origin(anchor, pw, ph, window, cx);
        let bridge = self.dl_bridge.clone();
        self.volume_popover = DesktopLyricsView::spawn_glass_popover(
            "OverLyrics Wallpaper Volume",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopVolumePopover::new(bridge),
        );
    }

    fn toggle_more(&mut self, anchor: Box4, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(h) = self.more_popover.take() {
            if h.update(cx, |_, w, _| w.remove_window()).is_ok() {
                return;
            }
        }
        if self.more_closed_flag.swap(false, Ordering::SeqCst) {
            return;
        }
        self.close_popovers(cx);
        let (pw, ph) = (MORE_WIN_W, MORE_WIN_H);
        let pos = self.popover_origin(anchor, pw, ph, window, cx);
        let wp = self.wp_bridge.clone();
        let dl = self.dl_bridge.clone();
        let open_picker = self.pending_open_picker.clone();
        let open_cover = self.pending_open_cover.clone();
        let closed_flag = self.more_closed_flag.clone();
        self.more_popover = DesktopLyricsView::spawn_glass_popover(
            "OverLyrics Wallpaper More",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| WallpaperMorePopover::new(wp, dl, open_picker, open_cover, closed_flag),
        );
    }

    fn open_cover_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popovers(cx);
        let anchor = self
            .hits
            .iter()
            .find(|(b, _)| *b == BarBtn::More)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| Box4::new(CONTROL_BAR_W - 76.0, 6.0, 32.0, 44.0));
        let (pw, ph) = (228.0, 108.0);
        let pos = self.popover_origin(anchor, pw, ph, window, cx);
        let wp = self.wp_bridge.clone();
        let dl = self.dl_bridge.clone();
        self.cover_popover = DesktopLyricsView::spawn_glass_popover(
            "OverLyrics Wallpaper Cover",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| WallpaperCoverPopover::new(wp, dl),
        );
    }

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popovers(cx);
        let snap = self.dl_bridge.read_snapshot();
        let visible_rows = snap.candidates.len().clamp(1, 4);
        let pw = 420.0_f32;
        let ph = VERSION_HEADER + visible_rows as f32 * VERSION_ROW + 8.0;
        let anchor = self
            .hits
            .iter()
            .find(|(b, _)| *b == BarBtn::More)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| Box4::new(CONTROL_BAR_W - 76.0, 6.0, 32.0, 44.0));
        let pos = self.popover_origin(anchor, pw, ph, window, cx);
        let bridge = self.dl_bridge.clone();
        self.picker_popover = DesktopLyricsView::spawn_glass_popover(
            "OverLyrics Wallpaper Lyrics",
            pos,
            (pw, ph),
            window,
            cx,
            move |_, _| DesktopLyricsPickerPopover::new(bridge),
        );
    }
}

impl Drop for WallpaperControlBar {
    fn drop(&mut self) {
        if self.hwnd != 0 {
            self.wp_bridge.unregister_hwnd(self.hwnd);
        }
    }
}

impl Render for WallpaperControlBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if !self.configured {
                self.configured = true;
                self.hwnd = hwnd;
                self.wp_bridge.register_hwnd(hwnd);
                let pinned = self.pinned;
                window.defer(cx, move |_, _| {
                    win32_window::configure_desktop_overlay_zorder(hwnd, pinned);
                });
            }
        }

        if self.pending_open_picker.swap(false, Ordering::SeqCst) {
            let this = cx.entity().downgrade();
            window.defer(cx, move |w, cx| {
                let _ = this.update(cx, |bar, cx| bar.open_picker(w, cx));
            });
        }
        if self.pending_open_cover.swap(false, Ordering::SeqCst) {
            let this = cx.entity().downgrade();
            window.defer(cx, move |w, cx| {
                let _ = this.update(cx, |bar, cx| bar.open_cover_popover(w, cx));
            });
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 33.0);
        self.last_ms = now;
        let sf = window.scale_factor().max(1.0);
        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let win_h = f32::from(vp.height).max(1.0);

        // Only treat cursor as inside when this bar is directly under the cursor (not occluded by Settings/apps).
        let cursor_inside = win32_window::is_cursor_directly_over_hwnd(self.hwnd);
        if !cursor_inside && self.drag.is_none() {
            self.cursor = None;
        }
        let popover_open = self.any_popover_alive(cx);
        let raw_active = cursor_inside || popover_open || self.drag.is_some();
        let want_expanded = if raw_active {
            self.hover_leave_ms = None;
            true
        } else {
            let left_at = *self.hover_leave_ms.get_or_insert(now);
            now - left_at < COLLAPSE_DELAY_MS
        };
        self.expand_spring
            .set_target_position(if want_expanded { 1.0 } else { 0.0 });
        self.expand_spring.update(dt / 1000.0);
        let expand_t = self.expand_spring.position().clamp(0.0, 1.0) as f32;

        if let Some(mut drag) = self.drag {
            if win32_window::is_left_mouse_down() {
                if let (Some(hwnd), Some((cx_p, cy_p))) = (
                    win32_window::hwnd_from_window(window),
                    win32_window::cursor_screen_pos(),
                ) {
                    let dx = (cx_p - drag.start_cursor_phys.0) as f32 / sf;
                    let dy = (cy_p - drag.start_cursor_phys.1) as f32 / sf;
                    if !drag.moved && (dx.abs() > 4.0 || dy.abs() > 4.0) {
                        drag.moved = true;
                        self.close_popovers(cx);
                    }
                    if drag.moved {
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
                        let px_x = (nx * sf).round() as i32;
                        let px_y = (ny * sf).round() as i32;
                        let px_w = (win_w * sf).round() as i32;
                        let px_h = (win_h * sf).round() as i32;
                        cx.foreground_executor()
                            .spawn(async move {
                                win32_window::set_window_rect_physical(hwnd, px_x, px_y, px_w, px_h);
                            })
                            .detach();
                    }
                    self.drag = Some(drag);
                }
            } else {
                self.drag = None;
                self.pressed = None;
            }
        }

        let snap = self.dl_bridge.read_snapshot();
        let live_pos_ms = if snap.playing {
            snap.position_ms + snap.sampled_at.elapsed().as_secs_f64() * 1000.0
        } else {
            snap.position_ms
        };
        let progress_frac = if snap.duration_ms > 1.0 {
            (live_pos_ms / snap.duration_ms).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        self.play_morph
            .set_target_position(if snap.playing { 1.0 } else { 0.0 });
        self.play_morph.update(dt / 1000.0);
        let play_morph = self.play_morph.position() as f32;

        let full_w = (win_w - PILL_PAD * 2.0).max(1.0);
        let full_h = (win_h - PILL_PAD * 2.0).max(1.0);
        let cur_w = COLLAPSED_W + (full_w - COLLAPSED_W) * expand_t;
        let cur_h = COLLAPSED_H + (full_h - COLLAPSED_H) * expand_t;
        let pill = Box4::new(
            (win_w - cur_w) * 0.5,
            (win_h - cur_h) * 0.5,
            cur_w,
            cur_h,
        );

        let btns = [
            BarBtn::Pin,
            BarBtn::Volume,
            BarBtn::Prev,
            BarBtn::Play,
            BarBtn::Next,
            BarBtn::More,
            BarBtn::Close,
        ];
        let full_pill = Box4::new(PILL_PAD, PILL_PAD, full_w, full_h);
        let inner_pad = 10.0;
        let cell_w = (full_pill.w - inner_pad * 2.0) / btns.len() as f32;
        self.hits.clear();
        for (i, b) in btns.iter().enumerate() {
            let cx_b = full_pill.x + inner_pad + cell_w * (i as f32 + 0.5);
            let cy_b = full_pill.y + full_pill.h * 0.5;
            let sz = if *b == BarBtn::Play { 34.0 } else { 30.0 };
            self.hits
                .push((*b, Box4::new(cx_b - sz * 0.5, cy_b - sz * 0.5, sz, sz)));
        }

        let hovered = if expand_t > 0.45 {
            self.cursor.and_then(|(mx, my)| self.hit_at(mx, my))
        } else {
            None
        };
        for b in btns {
            let is_pressed = self.pressed == Some(b);
            self.feedback
                .entry(b)
                .or_default()
                .step(hovered == Some(b) || is_pressed, is_pressed, dt);
        }

        let animating = snap.playing
            || cursor_inside
            || popover_open
            || self.drag.is_some()
            || !self.expand_spring.arrived()
            || !self.play_morph.arrived()
            || self.hover_leave_ms.is_some()
            || self.feedback.values().any(|f| f.hover > 0.005 || f.press > 0.005);
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

        let hits = self.hits.clone();
        let feedback = self.feedback.clone();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let pinned = self.pinned;

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                this.cursor = Some((f32::from(ev.position.x), f32::from(ev.position.y)));
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    this.cursor = Some((x, y));
                    let expanded = this.expand_spring.position() > 0.45;
                    this.pressed = if expanded { this.hit_at(x, y) } else { None };
                    if let Some((cx_p, cy_p)) = win32_window::cursor_screen_pos() {
                        let wb = window.bounds();
                        this.drag = Some(DragState {
                            start_cursor_phys: (cx_p, cy_p),
                            start_origin_logical: (f32::from(wb.origin.x), f32::from(wb.origin.y)),
                            snapped_x: false,
                            snapped_y: false,
                            moved: false,
                        });
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, cx| {
                    let was_dragged = this.drag.take().is_some_and(|d| d.moved);
                    let down = this.pressed.take();
                    if was_dragged {
                        let wb = window.bounds();
                        this.wp_bridge.push(WallpaperCommand::SaveBarPos(
                            f32::from(wb.origin.x),
                            f32::from(wb.origin.y),
                        ));
                        return;
                    }
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    let up = this.hit_at(x, y);
                    if down.is_some() && down == up {
                        if let Some(btn) = up {
                            let anchor = this
                                .hits
                                .iter()
                                .find(|(b, _)| *b == btn)
                                .map(|(_, r)| *r)
                                .unwrap_or(Box4::new(x - 16.0, y - 16.0, 32.0, 32.0));
                            match btn {
                                BarBtn::Pin => {
                                    this.pinned = !this.pinned;
                                    let hwnd = this.hwnd;
                                    let on_top = this.pinned;
                                    window.defer(cx, move |_, _| {
                                        win32_window::set_always_on_top(hwnd, on_top);
                                    });
                                }
                                BarBtn::Volume => this.toggle_volume(anchor, window, cx),
                                BarBtn::Prev => this.dl_bridge.push(DesktopCommand::Prev),
                                BarBtn::Play => this.dl_bridge.push(DesktopCommand::PlayPause),
                                BarBtn::Next => this.dl_bridge.push(DesktopCommand::Next),
                                BarBtn::More => this.toggle_more(anchor, window, cx),
                                BarBtn::Close => {
                                    this.close_popovers(cx);
                                    this.wp_bridge.push(WallpaperCommand::ExitWallpaper);
                                }
                            }
                        }
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        chrome::set_light_theme(light_panel);
                        paint_sidecar_glass_frame(window, pill, pill.h * 0.5, theme_mode, 1.0);

                        // Collapsed rest state: main-window-style gradient progress bar with luminous endpoint.
                        let prog_alpha = 1.0 - smoothstep01((expand_t * 1.45).clamp(0.0, 1.0));
                        if prog_alpha > 0.01 {
                            let inset = 2.0;
                            let track_x = pill.x + inset;
                            let track_y = pill.y + inset;
                            let track_w = (pill.w - inset * 2.0).max(1.0);
                            let track_h = (pill.h - inset * 2.0).max(2.0);
                            let filled = (track_w * progress_frac).clamp(0.0, track_w);
                            if filled > 0.5 {
                                let (fill_start, fill_end) = if light_panel {
                                    (chrome::text(0.45 * prog_alpha), chrome::text(0.88 * prog_alpha))
                                } else {
                                    (chrome::text(0.42 * prog_alpha), chrome::text(prog_alpha))
                                };
                                window.paint_quad(quad(
                                    bounds(point(px(track_x), px(track_y)), size(px(filled), px(track_h))),
                                    Corners::all(px(track_h * 0.5)),
                                    linear_gradient(
                                        90.0,
                                        linear_color_stop(fill_start, 0.0),
                                        linear_color_stop(fill_end, 1.0),
                                    ),
                                    Edges::default(),
                                    Hsla::default(),
                                    BorderStyle::Solid,
                                ));
                                let glow_w = 24.0_f32.min(filled);
                                window.paint_quad(quad(
                                    bounds(
                                        point(px(track_x + filled - glow_w), px(track_y)),
                                        size(px(glow_w), px(track_h)),
                                    ),
                                    Corners::all(px(track_h * 0.5)),
                                    linear_gradient(
                                        90.0,
                                        linear_color_stop(chrome::text(0.0), 0.0),
                                        linear_color_stop(chrome::text(0.72 * prog_alpha), 1.0),
                                    ),
                                    Edges::default(),
                                    Hsla::default(),
                                    BorderStyle::Solid,
                                ));
                                window.paint_drop_shadows(
                                    bounds(
                                        point(px(track_x + filled - glow_w), px(track_y)),
                                        size(px(glow_w), px(track_h)),
                                    ),
                                    Corners::all(px(track_h * 0.5)),
                                    &[BoxShadow::new(px(0.0), px(0.0), chrome::text(0.32 * prog_alpha))
                                        .blur_radius(px(5.0))],
                                );
                            }
                        }

                        // Expanded state: 7 liquid-glass bubble buttons smoothly scaling & fading in.
                        let btn_alpha = smoothstep01(expand_t);
                        let icon_scale = 0.82 + 0.18 * btn_alpha;
                        if btn_alpha > 0.01 {
                            for (btn, rect) in &hits {
                                let fb = feedback.get(btn).copied().unwrap_or_default();
                                let swell = (fb.hover * 0.35 + fb.press * 0.65).clamp(0.0, 1.0);
                                if swell > 0.01 {
                                    chrome::paint_segmented_lens(
                                        window,
                                        *rect,
                                        rect.h * 0.5,
                                        swell,
                                        fb,
                                        btn_alpha,
                                    );
                                }
                                let (bx, by) = rect.centre();
                                let tone = chrome::text((0.86 + 0.12 * fb.hover) * btn_alpha);
                                match btn {
                                    BarBtn::Pin => {
                                        let d = 15.0 * icon_scale;
                                        let pin_tone = if pinned {
                                            Hsla::from(rgb(0xFFFFFF)).opacity(0.98 * btn_alpha)
                                        } else {
                                            chrome::text((0.64 + 0.24 * fb.hover) * btn_alpha)
                                        };
                                        icons::paint_elastic(
                                            window,
                                            cx,
                                            if pinned { Icon::PinFilled } else { Icon::Pin },
                                            Box4::new(bx - d * 0.5, by - d * 0.5, d, d),
                                            pin_tone,
                                            fb,
                                        );
                                    }
                                    BarBtn::Volume => {
                                        let d = 16.0 * icon_scale;
                                        icons::paint_elastic(
                                            window,
                                            cx,
                                            Icon::Volume,
                                            Box4::new(bx - d * 0.5, by - d * 0.5, d, d),
                                            tone,
                                            fb,
                                        );
                                    }
                                    BarBtn::Prev => {
                                        let d = 16.0 * icon_scale;
                                        icons::paint_elastic(
                                            window,
                                            cx,
                                            Icon::Prev,
                                            Box4::new(bx - d * 0.5, by - d * 0.5, d, d),
                                            tone,
                                            fb,
                                        );
                                    }
                                    BarBtn::Play => {
                                        let d = 20.0 * icon_scale;
                                        icons::paint_toggle(
                                            window,
                                            cx,
                                            Icon::Play,
                                            Icon::Pause,
                                            Box4::new(bx - d * 0.5, by - d * 0.5, d, d),
                                            chrome::text(0.96 * btn_alpha),
                                            fb,
                                            play_morph,
                                        );
                                    }
                                    BarBtn::Next => {
                                        let d = 16.0 * icon_scale;
                                        icons::paint_elastic(
                                            window,
                                            cx,
                                            Icon::Next,
                                            Box4::new(bx - d * 0.5, by - d * 0.5, d, d),
                                            tone,
                                            fb,
                                        );
                                    }
                                    BarBtn::More => {
                                        let d = 16.0 * icon_scale;
                                        icons::paint_elastic(
                                            window,
                                            cx,
                                            Icon::More,
                                            Box4::new(bx - d * 0.5, by - d * 0.5, d, d),
                                            tone,
                                            fb,
                                        );
                                    }
                                    BarBtn::Close => {
                                        let d = 14.0 * icon_scale;
                                        let close_tone = if fb.hover > 0.05 {
                                            Hsla::from(rgb(0xFF453A)).opacity(0.95 * btn_alpha)
                                        } else {
                                            chrome::text(0.76 * btn_alpha)
                                        };
                                        icons::paint_elastic(
                                            window,
                                            cx,
                                            Icon::Close,
                                            Box4::new(bx - d * 0.5, by - d * 0.5, d, d),
                                            close_tone,
                                            fb,
                                        );
                                    }
                                }
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

const MORE_CARD_W: f32 = 248.0;
const MORE_CARD_H: f32 = 194.0;
const MORE_PAD: f32 = 12.0;
const MORE_WIN_W: f32 = MORE_CARD_W + MORE_PAD * 2.0;
const MORE_WIN_H: f32 = MORE_CARD_H + MORE_PAD * 2.0;

/// Liquid-glass More popover for Wallpaper Mode with bounded elastic drag:
/// 1. 歌词同步 (- / offset / +, ±200ms matching main window)
/// 2. 歌词版本 (opens 420px shared DesktopLyricsPickerPopover)
/// 3. 封面状态 (opens compact Display Cover / Motion Cover switches)
/// 4. 桌面歌词 (coexists with wallpaper mode)
/// 5. 设置 (opens standalone Settings window)
struct WallpaperMorePopover {
    wp_bridge: Arc<SharedWallpaperBridge>,
    dl_bridge: Arc<SharedDesktopBridge>,
    open_picker_flag: Arc<AtomicBool>,
    open_cover_flag: Arc<AtomicBool>,
    closed_flag: Arc<AtomicBool>,
    motion: PanelMotion,
    press_gesture: PressGesture,
    panel_elastic: Elastic,
    panel_dragging: bool,
    pressed_row: Option<usize>,
    cursor: Option<(f32, f32)>,
    row_fb: [Feedback; 5],
    minus_fb: Feedback,
    plus_fb: Feedback,
    card_rect: Box4,
    was_active: bool,
    configured: bool,
    started: Instant,
    last_ms: f64,
}

impl WallpaperMorePopover {
    fn new(
        wp_bridge: Arc<SharedWallpaperBridge>,
        dl_bridge: Arc<SharedDesktopBridge>,
        open_picker_flag: Arc<AtomicBool>,
        open_cover_flag: Arc<AtomicBool>,
        closed_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            wp_bridge,
            dl_bridge,
            open_picker_flag,
            open_cover_flag,
            closed_flag,
            motion: PanelMotion::default(),
            press_gesture: PressGesture::default(),
            panel_elastic: Elastic::default(),
            panel_dragging: false,
            pressed_row: None,
            cursor: None,
            row_fb: [Feedback::default(); 5],
            minus_fb: Feedback::default(),
            plus_fb: Feedback::default(),
            card_rect: Box4::new(MORE_PAD, MORE_PAD, MORE_CARD_W, MORE_CARD_H),
            was_active: false,
            configured: false,
            started: Instant::now(),
            last_ms: 0.0,
        }
    }

    fn row_rect(card: Box4, idx: usize) -> Box4 {
        Box4::new(
            card.x + 7.0,
            card.y + 7.0 + idx as f32 * 36.0,
            (card.w - 14.0).max(20.0),
            34.0,
        )
    }

    fn stepper_rects(row: Box4) -> (Box4, Box4) {
        let btn_h = 22.0;
        let btn_y = row.y + (row.h - btn_h) * 0.5;
        let plus = Box4::new(row.right() - 28.0, btn_y, 24.0, btn_h);
        let minus = Box4::new(row.right() - 108.0, btn_y, 24.0, btn_h);
        (minus, plus)
    }
}

impl Render for WallpaperMorePopover {
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
            self.closed_flag.store(true, Ordering::SeqCst);
            window.remove_window();
            return div().into_any_element();
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;
        self.motion.step(true, dt);

        let raw_delta = self.press_gesture.delta();
        let held = self.panel_dragging || (self.pressed_row.is_some() && self.press_gesture.dragged());
        self.panel_elastic.step(raw_delta, held, dt);
        let pp = self.panel_elastic.pull();
        let card = Box4::new(MORE_PAD + pp[0], MORE_PAD + pp[1], MORE_CARD_W, MORE_CARD_H);
        self.card_rect = card;

        let snap = self.dl_bridge.read_snapshot();
        let row0 = Self::row_rect(card, 0);
        let (minus_r, plus_r) = Self::stepper_rects(row0);

        self.minus_fb.step(
            self.cursor.is_some_and(|(mx, my)| minus_r.contains(mx, my)),
            false,
            dt,
        );
        self.plus_fb.step(
            self.cursor.is_some_and(|(mx, my)| plus_r.contains(mx, my)),
            false,
            dt,
        );
        for i in 0..5 {
            let r = Self::row_rect(card, i);
            let hot = i > 0 && self.cursor.is_some_and(|(mx, my)| r.contains(mx, my));
            self.row_fb[i].step(hot, self.pressed_row == Some(i), dt);
        }

        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let panel_radius = snap.display.corner_radius.clamp(12.0, 22.0);
        let sync_text = offset_label(snap.sync_offset_ms);
        let row_fb = self.row_fb;
        let minus_fb = self.minus_fb;
        let plus_fb = self.plus_fb;

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.panel_dragging || this.pressed_row.is_some() {
                    this.press_gesture.update(x, y);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                    let (mx, my) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    this.cursor = Some((mx, my));
                    let c = this.card_rect;
                    let r0 = Self::row_rect(c, 0);
                    let (m_r, p_r) = Self::stepper_rects(r0);
                    if m_r.contains(mx, my) {
                        this.dl_bridge.push(DesktopCommand::SyncNudge(-200));
                        return;
                    }
                    if p_r.contains(mx, my) {
                        this.dl_bridge.push(DesktopCommand::SyncNudge(200));
                        return;
                    }
                    for i in 1..5 {
                        if Self::row_rect(c, i).contains(mx, my) {
                            this.pressed_row = Some(i);
                            this.press_gesture.begin(mx, my);
                            return;
                        }
                    }
                    if c.contains(mx, my) {
                        this.panel_dragging = true;
                        this.press_gesture.begin(mx, my);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, _| {
                    let dragged = this.press_gesture.dragged();
                    this.press_gesture.cancel();
                    this.panel_dragging = false;
                    let down = this.pressed_row.take();
                    if dragged {
                        return;
                    }
                    let (mx, my) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if let Some(i) = down {
                        if Self::row_rect(this.card_rect, i).contains(mx, my) {
                            match i {
                                1 => {
                                    this.open_picker_flag.store(true, Ordering::SeqCst);
                                }
                                2 => {
                                    this.open_cover_flag.store(true, Ordering::SeqCst);
                                }
                                3 => {
                                    this.wp_bridge.push(WallpaperCommand::SwitchToDesktopLyrics);
                                }
                                4 => {
                                    this.wp_bridge.push(WallpaperCommand::OpenSettings);
                                }
                                _ => {}
                            }
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
                        paint_sidecar_glass_frame(window, card, panel_radius, theme_mode, surf_a);
                        let rows: [(&str, Icon, bool); 5] = [
                            ("歌词同步", Icon::Sync, false),
                            ("歌词版本", Icon::List, true),
                            ("封面状态", Icon::MotionCover, true),
                            ("桌面歌词", Icon::DesktopLyrics, false),
                            ("设置", Icon::Settings, false),
                        ];
                        for (i, (label, icon, has_chevron)) in rows.iter().enumerate() {
                            let r = Self::row_rect(card, i);
                            let fb = row_fb[i];
                            if i == 4 {
                                chrome::fill_round(
                                    window,
                                    card.x + 12.0,
                                    r.y - 1.5,
                                    (card.w - 24.0).max(20.0),
                                    0.75,
                                    0.35,
                                    chrome::text(0.08 * cont_a),
                                );
                            }
                            if i > 0 {
                                chrome::control_surface_alpha_radius(
                                    window, r, fb, false, cont_a, 11.0,
                                );
                            }
                            let tone = chrome::text((0.88 + 0.10 * fb.hover) * cont_a);
                            let icon_tone = chrome::text((0.58 + 0.24 * fb.hover) * cont_a);
                            icons::paint(
                                window,
                                cx,
                                *icon,
                                Box4::new(r.x + 10.0, r.y + (r.h - 15.0) * 0.5, 15.0, 15.0),
                                icon_tone,
                            );
                            chrome::paint_text_vcentred(
                                window,
                                label,
                                r.x + 32.0,
                                r.centre().1,
                                12.0,
                                500.0,
                                tone,
                            );
                            if *has_chevron {
                                chrome::chevron(
                                    window,
                                    r.right() - 14.0,
                                    r.y + (r.h - 8.0) * 0.5,
                                    chrome::text((0.36 + 0.30 * fb.hover) * cont_a),
                                );
                            }
                            if i == 0 {
                                chrome::paint_stepper(
                                    window,
                                    minus_r,
                                    plus_r,
                                    minus_fb,
                                    plus_fb,
                                    &sync_text,
                                    true,
                                    cont_a,
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

/// Compact Liquid Glass popover for toggling Wallpaper cover visibility & motion cover.
struct WallpaperCoverPopover {
    wp_bridge: Arc<SharedWallpaperBridge>,
    dl_bridge: Arc<SharedDesktopBridge>,
    motion: PanelMotion,
    cursor: Option<(f32, f32)>,
    row_fb: [Feedback; 2],
    sw_springs: [Spring; 2],
    was_active: bool,
    configured: bool,
    started: Instant,
    last_ms: f64,
}

impl WallpaperCoverPopover {
    fn new(wp_bridge: Arc<SharedWallpaperBridge>, dl_bridge: Arc<SharedDesktopBridge>) -> Self {
        let snap = dl_bridge.read_snapshot();
        Self {
            wp_bridge,
            dl_bridge,
            motion: PanelMotion::default(),
            cursor: None,
            row_fb: [Feedback::default(); 2],
            sw_springs: [
                state_icon_spring(if snap.display.show_cover { 1.0 } else { 0.0 }),
                state_icon_spring(if snap.display.motion_cover { 1.0 } else { 0.0 }),
            ],
            was_active: false,
            configured: false,
            started: Instant::now(),
            last_ms: 0.0,
        }
    }

    fn row_rect(w: f32, idx: usize) -> Box4 {
        Box4::new(8.0, 12.0 + idx as f32 * 42.0, (w - 16.0).max(20.0), 38.0)
    }
}

impl Render for WallpaperCoverPopover {
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
        } else if self.was_active {
            window.remove_window();
            return div().into_any_element();
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 33.0);
        self.last_ms = now;
        self.motion.step(true, dt);

        let snap = self.dl_bridge.read_snapshot();
        let vp = window.viewport_size();
        let w = f32::from(vp.width).max(1.0);
        let h = f32::from(vp.height).max(1.0);
        let states = [snap.display.show_cover, snap.display.motion_cover];
        for i in 0..2 {
            let r = Self::row_rect(w, i);
            let hot = self.cursor.is_some_and(|(mx, my)| r.contains(mx, my));
            self.row_fb[i].step(hot, false, dt);
            self.sw_springs[i].set_target_position(if states[i] { 1.0 } else { 0.0 });
            self.sw_springs[i].update(dt / 1000.0);
        }

        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let theme_mode = snap.display.theme_mode;
        let light_panel = theme_mode == ThemeMode::Light;
        let panel_radius = snap.display.corner_radius.clamp(12.0, 22.0);
        let accent_hex = snap.display.accent_hex;
        let row_fb = self.row_fb;
        let fracs = [
            self.sw_springs[0].position().clamp(0.0, 1.0) as f32,
            self.sw_springs[1].position().clamp(0.0, 1.0) as f32,
        ];

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                this.cursor = Some((f32::from(ev.position.x), f32::from(ev.position.y)));
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                    let (mx, my) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    if Self::row_rect(w, 0).contains(mx, my) {
                        this.wp_bridge.push(WallpaperCommand::ToggleShowCover);
                    } else if Self::row_rect(w, 1).contains(mx, my) {
                        this.wp_bridge.push(WallpaperCommand::ToggleMotionCover);
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
                        let items = [
                            ("显示封面", Icon::CoverExpand, states[0], fracs[0]),
                            ("动态封面", Icon::MotionCover, states[1], fracs[1]),
                        ];
                        for (i, (label, icon, on, frac)) in items.iter().enumerate() {
                            let r = Self::row_rect(w, i);
                            let fb = row_fb[i];
                            chrome::control_surface_alpha_radius(window, r, fb, false, cont_a, 10.0);
                            icons::paint(
                                window,
                                cx,
                                *icon,
                                Box4::new(r.x + 10.0, r.y + (r.h - 15.0) * 0.5, 15.0, 15.0),
                                chrome::text((0.58 + 0.24 * fb.hover) * cont_a),
                            );
                            chrome::paint_text_vcentred(
                                window,
                                label,
                                r.x + 32.0,
                                r.centre().1,
                                12.0,
                                500.0,
                                chrome::text((0.88 + 0.10 * fb.hover) * cont_a),
                            );
                            chrome::paint_switch_with_accent(
                                window,
                                r.right() - 42.0,
                                r.y + (r.h - 19.0) * 0.5,
                                34.0,
                                19.0,
                                *on,
                                *frac,
                                fb,
                                accent_hex,
                                cont_a,
                            );
                        }
                        chrome::set_light_theme(false);
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}
