//! Edge-docked auto-hide floating liquid-glass control bar for Wallpaper Mode (Plan A).
//!
//! Tucks neatly against the top-center edge of the primary display when idle,
//! leaving only a subtle 4px luminous handle. Moving the cursor to the top-center
//! edge slides out the full frosted glass pill with playback controls, inline
//! volume drag slider, Desktop Lyrics toggle, and Exit Wallpaper button.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{
    Context, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Render, Styled, Window, canvas, div, hsla, rgb,
};
use oly_core::scroll::{Spring, SpringParams};
use oly_ui::chrome;
use oly_ui::interaction::{Feedback, state_icon_spring};
use oly_ui::win32_window;

use super::{SharedWallpaperBridge, WallpaperCommand};
use crate::player::icons::{self, Icon};
use crate::player::model::{Box4, ThemeMode};
use crate::player::view::paint_sidecar_glass_frame;

pub const CONTROL_BAR_W: f32 = 432.0;
pub const CONTROL_BAR_H: f32 = 64.0;
const PILL_W: f32 = 408.0;
const PILL_H: f32 = 44.0;
const PILL_TOP_SHOWN: f32 = 10.0;
const PILL_TOP_HIDDEN: f32 = -42.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BarHit {
    Prev,
    Play,
    Next,
    VolTrack,
    DesktopLyrics,
    Exit,
}

pub struct WallpaperControlBar {
    bridge: Arc<SharedWallpaperBridge>,
    configured: bool,
    hwnd: isize,
    reveal_spring: Spring,
    play_morph: Spring,
    cursor: Option<(f32, f32)>,
    pressed: Option<BarHit>,
    vol_dragging: bool,
    feedback: HashMap<BarHit, Feedback>,
    hits: Vec<(BarHit, Box4)>,
    vol_track: Box4,
    started: Instant,
    last_ms: f64,
    idle_poll_pending: bool,
    applied_click_through: Option<bool>,
}

impl WallpaperControlBar {
    pub fn new(bridge: Arc<SharedWallpaperBridge>) -> Self {
        let mut reveal_spring = Spring::new(1.0);
        reveal_spring.update_params(SpringParams::new(260.0, 26.0));
        Self {
            bridge,
            configured: false,
            hwnd: 0,
            reveal_spring,
            play_morph: state_icon_spring(0.0),
            cursor: None,
            pressed: None,
            vol_dragging: false,
            feedback: HashMap::new(),
            hits: Vec::new(),
            vol_track: Box4::default(),
            started: Instant::now(),
            last_ms: 0.0,
            idle_poll_pending: false,
            applied_click_through: None,
        }
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<BarHit> {
        self.hits
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(h, _)| *h)
    }

    fn apply_volume_x(&self, x: f32) {
        let f = ((x - self.vol_track.x) / self.vol_track.w.max(1.0)).clamp(0.0, 1.0);
        self.bridge
            .push(WallpaperCommand::SetVolumePct((f as f64) * 100.0));
    }
}

impl Drop for WallpaperControlBar {
    fn drop(&mut self) {
        if self.hwnd != 0 {
            self.bridge.unregister_hwnd(self.hwnd);
        }
    }
}

impl Render for WallpaperControlBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if !self.configured {
                self.configured = true;
                self.hwnd = hwnd;
                self.bridge.register_hwnd(hwnd);
                window.defer(cx, move |_, _| {
                    win32_window::configure_desktop_overlay(hwnd);
                });
            }
        }

        let now = self.started.elapsed().as_secs_f64() * 1000.0;
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;

        let snap = self.bridge.read_snapshot();
        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let sf = window.scale_factor().max(0.5);
        let wb = window.bounds();

        // Check global OS cursor proximity to the top-center trigger zone so the bar
        // smoothly slides down even when tucked away.
        let mut cursor_near_top = false;
        let mut cursor_in_bar = false;
        if let Some((cx_p, cy_p)) = win32_window::cursor_screen_pos() {
            let lx = (cx_p as f32 / sf) - f32::from(wb.origin.x);
            let ly = (cy_p as f32 / sf) - f32::from(wb.origin.y);
            cursor_near_top = lx >= -24.0 && lx <= win_w + 24.0 && ly >= -6.0 && ly <= 16.0;
            cursor_in_bar = lx >= 0.0 && lx <= win_w && ly >= 0.0 && ly <= CONTROL_BAR_H;
            if cursor_in_bar || self.vol_dragging {
                self.cursor = Some((lx, ly));
            } else {
                self.cursor = None;
            }
        }
        if !win32_window::is_left_mouse_down() {
            self.vol_dragging = false;
            self.pressed = None;
        } else if self.vol_dragging {
            if let Some((lx, _)) = self.cursor {
                self.apply_volume_x(lx);
            }
        }

        let reveal_now = self.reveal_spring.position().clamp(0.0, 1.0) as f32;
        let want_open = cursor_near_top
            || self.vol_dragging
            || self.pressed.is_some()
            || (reveal_now > 0.25 && cursor_in_bar);
        self.reveal_spring
            .set_target_position(if want_open { 1.0 } else { 0.0 });
        self.reveal_spring.update(dt / 1000.0);
        let reveal = self.reveal_spring.position().clamp(0.0, 1.0) as f32;

        // When fully tucked away, keep window click-through so desktop clicks pass unobstructed.
        let click_through = reveal < 0.08 && !want_open;
        if self.hwnd != 0 && self.applied_click_through != Some(click_through) {
            self.applied_click_through = Some(click_through);
            let h = self.hwnd;
            window.defer(cx, move |_, _| {
                win32_window::set_click_through(h, click_through);
            });
        }

        self.play_morph
            .set_target_position(if snap.playing { 1.0 } else { 0.0 });
        self.play_morph.update(dt / 1000.0);
        let play_morph = self.play_morph.position().clamp(0.0, 1.0) as f32;

        let pill_x = (win_w - PILL_W) * 0.5;
        let pill_y = PILL_TOP_HIDDEN + (PILL_TOP_SHOWN - PILL_TOP_HIDDEN) * reveal;
        let pill = Box4::new(pill_x, pill_y, PILL_W, PILL_H);

        let cy = pill.centre().1;
        let btn_s = 28.0_f32;
        let r_prev = Box4::new(pill.x + 112.0, cy - btn_s * 0.5, btn_s, btn_s);
        let r_play = Box4::new(r_prev.right() + 4.0, cy - btn_s * 0.5, btn_s, btn_s);
        let r_next = Box4::new(r_play.right() + 4.0, cy - btn_s * 0.5, btn_s, btn_s);
        let r_vol = Box4::new(r_next.right() + 26.0, cy - 10.0, 64.0, 20.0);
        let r_desk = Box4::new(r_vol.right() + 10.0, cy - btn_s * 0.5, btn_s, btn_s);
        let r_exit = Box4::new(pill.right() - 78.0, cy - 14.0, 68.0, 28.0);
        self.vol_track = r_vol;

        let hits = vec![
            (BarHit::Prev, r_prev),
            (BarHit::Play, r_play),
            (BarHit::Next, r_next),
            (BarHit::VolTrack, r_vol),
            (BarHit::DesktopLyrics, r_desk),
            (BarHit::Exit, r_exit),
        ];
        self.hits = hits.clone();

        let mut animating = (reveal - if want_open { 1.0 } else { 0.0 }).abs() > 0.005;
        for (hit, rect) in &hits {
            let hot = !click_through && self.cursor.is_some_and(|(mx, my)| rect.contains(mx, my));
            let held = self.pressed == Some(*hit);
            let fb = self.feedback.entry(*hit).or_default();
            animating |= fb.step(hot, held, dt);
        }

        if animating || want_open {
            window.request_animation_frame();
        } else if !self.idle_poll_pending {
            // Low-power 12Hz hover check while tucked at top edge.
            self.idle_poll_pending = true;
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let _ = this.update(cx, |view, cx| {
                    view.idle_poll_pending = false;
                    cx.notify();
                });
            })
            .detach();
        }

        let fb_map = self.feedback.clone();
        let light = snap.theme_mode == ThemeMode::Light;
        let title = if snap.track_title.trim().is_empty() {
            "OverLyrics".to_string()
        } else {
            snap.track_title.clone()
        };

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.vol_dragging {
                    this.apply_volume_x(x);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    this.cursor = Some((x, y));
                    let hit = this.hit_at(x, y);
                    this.pressed = hit;
                    if hit == Some(BarHit::VolTrack) {
                        this.vol_dragging = true;
                        this.apply_volume_x(x);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    let pressed = this.pressed.take();
                    this.vol_dragging = false;
                    if pressed.is_some() && pressed == this.hit_at(x, y) {
                        match pressed.unwrap() {
                            BarHit::Prev => this.bridge.push(WallpaperCommand::Prev),
                            BarHit::Play => this.bridge.push(WallpaperCommand::PlayPause),
                            BarHit::Next => this.bridge.push(WallpaperCommand::Next),
                            BarHit::VolTrack => {}
                            BarHit::DesktopLyrics => {
                                this.bridge.push(WallpaperCommand::ToggleDesktopLyrics);
                            }
                            BarHit::Exit => this.bridge.push(WallpaperCommand::ExitWallpaper),
                        }
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        chrome::set_light_theme(light);
                        // Subtle top-edge handle when tucked away
                        let handle_a = (1.0 - reveal).clamp(0.0, 1.0) * 0.42;
                        if handle_a > 0.02 {
                            let hw = 64.0_f32;
                            chrome::fill_round(
                                window,
                                (win_w - hw) * 0.5,
                                2.0,
                                hw,
                                4.0,
                                2.0,
                                hsla(0.0, 0.0, 1.0, handle_a),
                            );
                        }
                        if reveal < 0.02 {
                            return;
                        }
                        let a = reveal;
                        paint_sidecar_glass_frame(window, pill, 22.0, snap.theme_mode, a);

                        let fg = |alpha: f32| chrome::text(alpha * a);
                        let accent = Hsla::from(rgb(snap.accent_hex));

                        // Truncated track title on left
                        let short_title: String = if title.chars().count() > 8 {
                            format!("{}…", title.chars().take(7).collect::<String>())
                        } else {
                            title.clone()
                        };
                        chrome::paint_text_vcentred(
                            window,
                            &short_title,
                            pill.x + 14.0,
                            cy,
                            11.5,
                            550.0,
                            fg(0.86),
                        );

                        // Transport buttons
                        let draw_icon_btn = |window: &mut Window,
                                             cx: &mut gpui_kit::App,
                                             hit: BarHit,
                                             r: Box4,
                                             icon: Icon,
                                             active: bool| {
                            let f = fb_map.get(&hit).copied().unwrap_or_default();
                            if f.hover > 0.01 || f.press > 0.01 {
                                chrome::fill_round(
                                    window,
                                    r.x,
                                    r.y,
                                    r.w,
                                    r.h,
                                    r.h * 0.5,
                                    chrome::text((0.10 * f.hover + 0.08 * f.press) * a),
                                );
                            }
                            let col = if active {
                                chrome::accent_rgb(snap.accent_hex, 0.95 * a)
                            } else {
                                fg(0.82 + 0.14 * f.hover)
                            };
                            icons::paint(
                                window,
                                cx,
                                icon,
                                Box4::new(r.x + 6.0, r.y + 6.0, 16.0, 16.0),
                                col,
                            );
                        };
                        draw_icon_btn(window, cx, BarHit::Prev, r_prev, Icon::Prev, false);
                        {
                            let f = fb_map.get(&BarHit::Play).copied().unwrap_or_default();
                            if f.hover > 0.01 || f.press > 0.01 {
                                chrome::fill_round(
                                    window,
                                    r_play.x,
                                    r_play.y,
                                    r_play.w,
                                    r_play.h,
                                    r_play.h * 0.5,
                                    chrome::text((0.12 * f.hover + 0.08 * f.press) * a),
                                );
                            }
                            let icon = if play_morph > 0.5 {
                                Icon::Pause
                            } else {
                                Icon::Play
                            };
                            icons::paint(
                                window,
                                cx,
                                icon,
                                Box4::new(r_play.x + 6.0, r_play.y + 6.0, 16.0, 16.0),
                                fg(0.95),
                            );
                        }
                        draw_icon_btn(window, cx, BarHit::Next, r_next, Icon::Next, false);

                        // Volume icon + mini track
                        icons::paint(
                            window,
                            cx,
                            Icon::Volume,
                            Box4::new(r_vol.x - 19.0, cy - 7.0, 14.0, 14.0),
                            fg(0.66),
                        );
                        let track_y = cy - 2.0;
                        chrome::fill_round(
                            window,
                            r_vol.x,
                            track_y,
                            r_vol.w,
                            4.0,
                            2.0,
                            chrome::text(0.18 * a),
                        );
                        let frac = (snap.volume_pct as f32 / 100.0).clamp(0.0, 1.0);
                        let fill_w = (r_vol.w * frac).max(4.0);
                        chrome::fill_round(
                            window,
                            r_vol.x,
                            track_y,
                            fill_w,
                            4.0,
                            2.0,
                            hsla(accent.h, accent.s.max(0.55), accent.l.clamp(0.38, 0.62), 0.92 * a),
                        );

                        // Desktop Lyrics toggle
                        draw_icon_btn(
                            window,
                            cx,
                            BarHit::DesktopLyrics,
                            r_desk,
                            Icon::DesktopLyrics,
                            snap.desktop_lyrics_open,
                        );

                        // Exit Wallpaper pill button
                        let f_exit = fb_map.get(&BarHit::Exit).copied().unwrap_or_default();
                        let exit_bg = hsla(
                            accent.h,
                            accent.s.max(0.55),
                            (accent.l + 0.04 * f_exit.hover - 0.05 * f_exit.press).clamp(0.30, 0.64),
                            (0.86 + 0.12 * f_exit.hover) * a,
                        );
                        chrome::fill_round(
                            window,
                            r_exit.x,
                            r_exit.y,
                            r_exit.w,
                            r_exit.h,
                            14.0,
                            exit_bg,
                        );
                        let label = "退出壁纸";
                        let (lw, _) = chrome::measure(window, label, 11.5, 600.0);
                        chrome::paint_text_vcentred(
                            window,
                            label,
                            r_exit.x + (r_exit.w - lw) * 0.5,
                            cy,
                            11.5,
                            600.0,
                            hsla(0.0, 0.0, 1.0, 0.96 * a),
                        );
                    },
                )
                .size_full(),
            )
    }
}
