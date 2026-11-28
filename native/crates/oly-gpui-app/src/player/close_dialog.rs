//! Standalone close confirmation dialog styled to match the Settings panel,
//! supporting elastic panel dragging within the outer padding and concise copy.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui_kit::{
    App, Context, IntoElement, InteractiveElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Render, Styled, Window, div, point, px, size,
};
use oly_ui::chrome;
use oly_ui::elastic::{Elastic, PressGesture};
use oly_ui::interaction::Feedback;
use oly_ui::panel_motion::PanelMotion;
use oly_ui::win32_window;

use super::icons::{self, Icon};
use super::model::{Box4, ThemeMode};

const CARD_W: f32 = 260.0;
const CARD_H: f32 = 136.0;
const PAD: f32 = 10.0;

pub const CLOSE_DIALOG_W: f32 = CARD_W + PAD * 2.0;
pub const CLOSE_DIALOG_H: f32 = CARD_H + PAD * 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseChoice {
    MinimizeToTray,
    Quit,
}

impl CloseChoice {
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::MinimizeToTray => "minimize",
            Self::Quit => "quit",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CloseDialogResult {
    pub choice: CloseChoice,
    pub remember: bool,
}

#[derive(Default)]
pub struct SharedCloseDialogBridge {
    result: Mutex<Option<CloseDialogResult>>,
}

impl SharedCloseDialogBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn submit(&self, res: CloseDialogResult) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(res);
    }

    pub fn take_result(&self) -> Option<CloseDialogResult> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Minimize,
    Quit,
    Remember,
    CloseX,
}

pub struct CloseConfirmDialog {
    bridge: Arc<SharedCloseDialogBridge>,
    theme_mode: ThemeMode,
    corner_radius: f32,
    accent_hex: u32,
    remember: bool,
    remember_frac: f32,
    configured: bool,
    was_active: bool,
    cursor: Option<(f32, f32)>,
    pressed: Option<Hit>,
    panel_dragging: bool,
    press_gesture: PressGesture,
    panel_elastic: Elastic,
    fb_min: Feedback,
    fb_quit: Feedback,
    fb_rem: Feedback,
    fb_close: Feedback,
    motion: PanelMotion,
    started: Instant,
    last_ms: f64,
    card_rect: Box4,
    r_min: Box4,
    r_quit: Box4,
    r_rem: Box4,
    r_close: Box4,
}

impl CloseConfirmDialog {
    pub fn new(
        bridge: Arc<SharedCloseDialogBridge>,
        theme_mode: ThemeMode,
        corner_radius: f32,
        accent_hex: u32,
    ) -> Self {
        Self {
            bridge,
            theme_mode,
            corner_radius,
            accent_hex,
            remember: false,
            remember_frac: 0.0,
            configured: false,
            was_active: false,
            cursor: None,
            pressed: None,
            panel_dragging: false,
            press_gesture: PressGesture::default(),
            panel_elastic: Elastic::default(),
            fb_min: Feedback::default(),
            fb_quit: Feedback::default(),
            fb_rem: Feedback::default(),
            fb_close: Feedback::default(),
            motion: PanelMotion::default(),
            started: Instant::now(),
            last_ms: 0.0,
            card_rect: Box4::default(),
            r_min: Box4::default(),
            r_quit: Box4::default(),
            r_rem: Box4::default(),
            r_close: Box4::default(),
        }
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        if self.r_min.contains(x, y) {
            Some(Hit::Minimize)
        } else if self.r_quit.contains(x, y) {
            Some(Hit::Quit)
        } else if self.r_rem.contains(x, y) {
            Some(Hit::Remember)
        } else if self.r_close.contains(x, y) {
            Some(Hit::CloseX)
        } else {
            None
        }
    }
}

impl Render for CloseConfirmDialog {
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
        } else if self.was_active && self.pressed.is_none() && !self.panel_dragging {
            window.remove_window();
            return div().into_any_element();
        }
        if !win32_window::is_left_mouse_down() {
            if self.pressed.is_some() || self.panel_dragging {
                self.pressed = None;
                self.panel_dragging = false;
                self.press_gesture.cancel();
            }
        } else if self.panel_dragging || self.pressed.is_some() {
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

        let raw_delta = self.press_gesture.delta();
        let panel_held = self.panel_dragging || (self.pressed.is_some() && self.press_gesture.dragged());
        self.panel_elastic.step(raw_delta, panel_held, dt);
        let pp = self.panel_elastic.pull();

        let card = Box4::new(PAD + pp[0], PAD + pp[1], CARD_W, CARD_H);
        self.card_rect = card;
        let pad_x = 16.0_f32;
        let inner_w = card.w - pad_x * 2.0;
        let r_close = Box4::new(card.right() - pad_x - 26.0, card.y + 13.0, 26.0, 26.0);
        let r_rem = Box4::new(card.x + pad_x, card.y + 48.0, inner_w, 30.0);
        let btn_y = card.bottom() - 14.0 - 32.0;
        let btn_h = 32.0;
        let gap = 10.0;
        let btn_w = (inner_w - gap) * 0.5;
        let r_min = Box4::new(card.x + pad_x, btn_y, btn_w, btn_h);
        let r_quit = Box4::new(r_min.right() + gap, btn_y, btn_w, btn_h);
        self.r_min = r_min;
        self.r_quit = r_quit;
        self.r_rem = r_rem;
        self.r_close = r_close;

        let hot = self.cursor.and_then(|(x, y)| self.hit_at(x, y));
        self.fb_min
            .step(hot == Some(Hit::Minimize), self.pressed == Some(Hit::Minimize), dt);
        self.fb_quit
            .step(hot == Some(Hit::Quit), self.pressed == Some(Hit::Quit), dt);
        self.fb_rem
            .step(hot == Some(Hit::Remember), self.pressed == Some(Hit::Remember), dt);
        self.fb_close
            .step(hot == Some(Hit::CloseX), self.pressed == Some(Hit::CloseX), dt);
        let rem_target = if self.remember { 1.0_f32 } else { 0.0 };
        let rem_step = (dt as f32 / 140.0).clamp(0.0, 1.0);
        self.remember_frac += (rem_target - self.remember_frac) * (1.0 - (1.0 - rem_step).powi(2));
        if (rem_target - self.remember_frac).abs() < 0.005 {
            self.remember_frac = rem_target;
        }

        let theme_mode = self.theme_mode;
        let light = theme_mode == ThemeMode::Light;
        let radius = self.corner_radius.clamp(12.0, 18.0);
        let accent_hex = self.accent_hex;
        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let remember = self.remember;
        let remember_frac = self.remember_frac;
        let fb_min = self.fb_min;
        let fb_quit = self.fb_quit;
        let fb_rem = self.fb_rem;
        let fb_close = self.fb_close;

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.panel_dragging || this.pressed.is_some() {
                    this.press_gesture.update(x, y);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    this.cursor = Some((x, y));
                    let hit = this.hit_at(x, y);
                    this.pressed = hit;
                    if hit.is_some() {
                        this.press_gesture.begin(x, y);
                    } else if this.card_rect.contains(x, y) {
                        this.panel_dragging = true;
                        this.press_gesture.begin(x, y);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    let dragged = this.press_gesture.dragged();
                    this.press_gesture.cancel();
                    this.panel_dragging = false;
                    let pressed = this.pressed.take();
                    if !dragged && pressed.is_some() && pressed == this.hit_at(x, y) {
                        match pressed.unwrap() {
                            Hit::Minimize => {
                                this.bridge.submit(CloseDialogResult {
                                    choice: CloseChoice::MinimizeToTray,
                                    remember: this.remember,
                                });
                                window.remove_window();
                            }
                            Hit::Quit => {
                                this.bridge.submit(CloseDialogResult {
                                    choice: CloseChoice::Quit,
                                    remember: this.remember,
                                });
                                window.remove_window();
                            }
                            Hit::Remember => {
                                this.remember = !this.remember;
                            }
                            Hit::CloseX => {
                                window.remove_window();
                            }
                        }
                    }
                }),
            )
            .child(
                gpui_kit::canvas(
                    move |_, _, _| {},
                    move |_, _, window, cx| {
                        chrome::set_light_theme(light);
                        crate::player::view::paint_sidecar_glass_frame(
                            window,
                            card,
                            radius,
                            theme_mode,
                            surf_a,
                        );

                        let fg_primary = chrome::text(0.95 * cont_a);
                        let fg_sub = chrome::text(0.82 * cont_a);

                        // Header: title aligned with circular glass close button (matches Settings header)
                        chrome::paint_text_vcentred(
                            window,
                            "关闭窗口",
                            card.x + pad_x,
                            r_close.centre().1,
                            14.5,
                            650.0,
                            fg_primary,
                        );
                        chrome::paint_glass_button(
                            window,
                            r_close,
                            "",
                            accent_hex,
                            false,
                            false,
                            fb_close,
                            cont_a,
                        );
                        icons::paint_elastic(
                            window,
                            cx,
                            Icon::Close,
                            Box4::new(r_close.x + 6.5, r_close.y + 6.5, 13.0, 13.0),
                            chrome::text((0.75 + 0.22 * fb_close.hover) * cont_a),
                            fb_close,
                        );

                        // Middle option row: Settings-identical switch control for "记住选择"
                        let sw_w = 38.0_f32;
                        let sw_h = 22.0_f32;
                        let sw_x = r_rem.right() - sw_w;
                        let sw_y = r_rem.y + (r_rem.h - sw_h) * 0.5;
                        chrome::paint_text_vcentred(
                            window,
                            "记住选择",
                            r_rem.x,
                            r_rem.centre().1,
                            12.5,
                            500.0,
                            fg_sub,
                        );
                        chrome::paint_switch_with_accent(
                            window,
                            sw_x,
                            sw_y,
                            sw_w,
                            sw_h,
                            remember,
                            remember_frac,
                            fb_rem,
                            accent_hex,
                            cont_a,
                        );

                        // Subtle divider between option row and bottom action buttons
                        chrome::fill_round(
                            window,
                            card.x + pad_x,
                            btn_y - 9.0,
                            inner_w,
                            1.0,
                            0.5,
                            chrome::themed_border(0.08 * cont_a),
                        );

                        // Bottom action buttons: 100% aligned with Settings `Control::Action` glass material
                        chrome::paint_glass_button(
                            window,
                            r_min,
                            "最小化到托盘",
                            accent_hex,
                            false,
                            false,
                            fb_min,
                            cont_a,
                        );
                        chrome::paint_glass_button(
                            window,
                            r_quit,
                            "退出",
                            accent_hex,
                            false,
                            true,
                            fb_quit,
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

/// Computes a centered screen origin for the close confirmation dialog over `window`,
/// clamped to the active display's visible bounds so mini/album modes never clip it.
pub fn dialog_origin_for(window: &Window, cx: &App) -> (f32, f32) {
    let wb = window.bounds();
    let cx_win = f32::from(wb.origin.x) + f32::from(wb.size.width) * 0.5;
    let cy_win = f32::from(wb.origin.y) + f32::from(wb.size.height) * 0.5;
    let mut x = cx_win - CLOSE_DIALOG_W * 0.5;
    let mut y = cy_win - CLOSE_DIALOG_H * 0.5;
    if let Some(d) = window.display(cx) {
        let vb = d.visible_bounds();
        let min_x = f32::from(vb.origin.x) + 8.0;
        let min_y = f32::from(vb.origin.y) + 8.0;
        let max_x = (f32::from(vb.origin.x) + f32::from(vb.size.width) - CLOSE_DIALOG_W - 8.0).max(min_x);
        let max_y = (f32::from(vb.origin.y) + f32::from(vb.size.height) - CLOSE_DIALOG_H - 8.0).max(min_y);
        x = x.clamp(min_x, max_x);
        y = y.clamp(min_y, max_y);
    }
    (x, y)
}

pub fn dialog_bounds(origin: (f32, f32)) -> gpui_kit::Bounds<gpui_kit::Pixels> {
    gpui_kit::bounds(
        point(px(origin.0), px(origin.1)),
        size(px(CLOSE_DIALOG_W), px(CLOSE_DIALOG_H)),
    )
}
