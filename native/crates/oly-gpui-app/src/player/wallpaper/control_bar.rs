//! Edge-docked auto-hide floating liquid-glass Island control pill for Wallpaper Mode.
//!
//! Directly reuses the no-cover floating Island pill layout (36x36 cover thumb +
//! title/artist + Prev/Play/Next + Volume + Exit icon button replacing More),
//! tucked neatly to the top-center screen edge until the cursor approaches.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{
    Bounds, Context, CustomSurfaceHandle, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Render, Styled, Window, canvas,
    div, hsla, point, px, rgb, size,
};
use oly_core::scroll::{Spring, SpringParams};
use oly_ui::chrome;
use oly_ui::interaction::{Feedback, state_icon_spring};
use oly_ui::win32_window;

use super::{SharedWallpaperBridge, WallpaperCommand};
use crate::player::icons::{self, Icon};
use crate::player::model::{Box4, ThemeMode};
use crate::player::view::paint_sidecar_glass_frame;

pub const CONTROL_BAR_W: f32 = 468.0;
pub const CONTROL_BAR_H: f32 = 72.0;
const PILL_W: f32 = 444.0;
const PILL_H: f32 = 52.0;
const PILL_TOP_SHOWN: f32 = 10.0;
const PILL_TOP_HIDDEN: f32 = -50.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BarHit {
    Prev,
    Play,
    Next,
    VolTrack,
    Exit,
}

pub struct WallpaperControlBar {
    bridge: Arc<SharedWallpaperBridge>,
    configured: bool,
    hwnd: isize,
    cover_surface: Option<CustomSurfaceHandle>,
    applied_cover_sig: u64,
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
            cover_surface: None,
            applied_cover_sig: 0,
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
        if self.applied_cover_sig != snap.cover_sig {
            self.applied_cover_sig = snap.cover_sig;
            if let Some(old) = self.cover_surface.take() {
                window.destroy_custom_surface(old);
            }
            if let Some(cover) = snap.cover.as_ref() {
                if let Ok(handle) =
                    window.create_custom_surface(cover.width, cover.height, &cover.rgba.pixels)
                {
                    self.cover_surface = Some(handle);
                }
            }
        }

        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let sf = window.scale_factor().max(0.5);
        let wb = window.bounds();

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

        // Island pill geometry (52px tall, 18px radius — identical to no-cover Island)
        let ir = Box4::new(
            (win_w - PILL_W) * 0.5,
            PILL_TOP_HIDDEN + (PILL_TOP_SHOWN - PILL_TOP_HIDDEN) * reveal,
            PILL_W,
            PILL_H,
        );
        let cy = ir.centre().1;
        let btn_s = 30.0_f32;
        let r_exit = Box4::new(ir.right() - 10.0 - btn_s, cy - btn_s * 0.5, btn_s, btn_s);
        let r_vol = Box4::new(r_exit.x - 66.0, cy - 10.0, 56.0, 20.0);
        let r_next = Box4::new(r_vol.x - 24.0 - btn_s, cy - btn_s * 0.5, btn_s, btn_s);
        let r_play = Box4::new(r_next.x - 4.0 - btn_s, cy - btn_s * 0.5, btn_s, btn_s);
        let r_prev = Box4::new(r_play.x - 4.0 - btn_s, cy - btn_s * 0.5, btn_s, btn_s);
        self.vol_track = r_vol;

        let hits = vec![
            (BarHit::Prev, r_prev),
            (BarHit::Play, r_play),
            (BarHit::Next, r_next),
            (BarHit::VolTrack, r_vol),
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
        let cover_surface = self.cover_surface;
        let title = if snap.track_title.trim().is_empty() {
            "OverLyrics".to_string()
        } else {
            snap.track_title.clone()
        };
        let artist = snap.track_artist.clone();

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
                        let ia = reveal;
                        paint_sidecar_glass_frame(window, ir, 18.0, snap.theme_mode, ia);
                        chrome::stroke_round(
                            window,
                            ir.x,
                            ir.y,
                            ir.w,
                            ir.h,
                            18.0,
                            1.0,
                            chrome::text(0.16 * ia),
                        );

                        // Cover thumbnail (36x36 rounded, matching no-cover Island)
                        let has_thumb = cover_surface.is_some();
                        if let Some(still) = cover_surface {
                            let thumb = Bounds {
                                origin: point(px(ir.x + 10.0), px(ir.y + 8.0)),
                                size: size(px(36.0), px(36.0)),
                            };
                            window.paint_custom_surface_rounded(thumb, px(8.0), still);
                            chrome::stroke_round(
                                window,
                                ir.x + 10.0,
                                ir.y + 8.0,
                                36.0,
                                36.0,
                                8.0,
                                0.75,
                                chrome::text(0.16 * ia),
                            );
                        }
                        let text_x = if has_thumb { ir.x + 54.0 } else { ir.x + 16.0 };
                        let meta_w = (r_prev.x - 10.0 - text_x).max(60.0);
                        chrome::paint_text_fitted(
                            window,
                            &title,
                            text_x,
                            ir.y + 8.0,
                            meta_w,
                            13.0,
                            600.0,
                            chrome::text(0.95 * ia),
                        );
                        chrome::paint_text_fitted(
                            window,
                            &artist,
                            text_x,
                            ir.y + 25.0,
                            meta_w,
                            11.0,
                            450.0,
                            chrome::text(0.62 * ia),
                        );

                        let draw_icon_btn = |window: &mut Window,
                                             cx: &mut gpui_kit::App,
                                             hit: BarHit,
                                             r: Box4,
                                             icon: Icon,
                                             danger_hover: bool| {
                            let f = fb_map.get(&hit).copied().unwrap_or_default();
                            if f.hover > 0.01 || f.press > 0.01 {
                                let bg = if danger_hover {
                                    Hsla::from(rgb(0xFF3B30))
                                        .opacity((0.18 * f.hover + 0.08 * f.press) * ia)
                                } else {
                                    chrome::text((0.10 * f.hover + 0.08 * f.press) * ia)
                                };
                                chrome::fill_round(window, r.x, r.y, r.w, r.h, r.h * 0.5, bg);
                            }
                            let col = if danger_hover && f.hover > 0.05 {
                                Hsla::from(rgb(0xFF453A)).opacity((0.92 + 0.06 * f.hover) * ia)
                            } else {
                                chrome::text((0.84 + 0.14 * f.hover) * ia)
                            };
                            icons::paint(
                                window,
                                cx,
                                icon,
                                Box4::new(r.x + 7.0, r.y + 7.0, 16.0, 16.0),
                                col,
                            );
                        };
                        draw_icon_btn(window, cx, BarHit::Prev, r_prev, Icon::Prev, false);
                        draw_icon_btn(
                            window,
                            cx,
                            BarHit::Play,
                            r_play,
                            if play_morph > 0.5 { Icon::Pause } else { Icon::Play },
                            false,
                        );
                        draw_icon_btn(window, cx, BarHit::Next, r_next, Icon::Next, false);

                        // Volume icon + inline slider
                        icons::paint(
                            window,
                            cx,
                            Icon::Volume,
                            Box4::new(r_vol.x - 19.0, cy - 7.0, 14.0, 14.0),
                            chrome::text(0.68 * ia),
                        );
                        let track_y = cy - 2.0;
                        chrome::fill_round(
                            window,
                            r_vol.x,
                            track_y,
                            r_vol.w,
                            4.0,
                            2.0,
                            chrome::text(0.18 * ia),
                        );
                        let accent = Hsla::from(rgb(snap.accent_hex));
                        let frac = (snap.volume_pct as f32 / 100.0).clamp(0.0, 1.0);
                        chrome::fill_round(
                            window,
                            r_vol.x,
                            track_y,
                            (r_vol.w * frac).max(4.0),
                            4.0,
                            2.0,
                            hsla(accent.h, accent.s.max(0.55), accent.l.clamp(0.38, 0.62), 0.92 * ia),
                        );

                        // Exit wallpaper icon button (replaces More `...`)
                        draw_icon_btn(window, cx, BarHit::Exit, r_exit, Icon::Close, true);
                    },
                )
                .size_full(),
            )
    }
}
