//! Full immersive desktop wallpaper window attached to Windows `WorkerW`.
//!
//! Renders dynamic GPU background + cover art + song title/artist + AMLL lyrics
//! (reusing the main window's `PlayerScene`, `rows::Shaper`, `model::RowMotion`,
//! and `paint::paint_row_with_shadow`), with NO bottom transport controls, and
//! automatic fullscreen / lockscreen / pause-timeout sleep (0% GPU when hidden).

use std::collections::HashMap;
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

use gpui_kit::{
    Bounds, ContentMask, Context, CustomSurfaceHandle, Hsla, IntoElement, ParentElement, Render,
    Styled, TextSystem, Window, canvas, div, hsla, point, px, rgb, size,
};
use oly_core::scroll::SpringParams;
use oly_render::mask::position_at;
use oly_ui::chrome;
use oly_ui::win32_window;

use super::SharedWallpaperBridge;
use super::win32_workerw::{
    attach_to_workerw, detach_from_workerw_and_refresh, is_desktop_occluded_or_locked,
};
use crate::player::model::{self, Box4, Row, RowMotion, SpringFeel, row_light};
use crate::player::paint::{self, LineAlign, RowLight, WrapperTransform};
use crate::player::rows::{RowLines, Shaper};
use crate::player::scene::PlayerScene;

const PAUSE_SLEEP_TIMEOUT_MS: f64 = 30_000.0;
const OCCLUSION_POLL_INTERVAL_MS: f64 = 260.0;

pub struct WallpaperBgView {
    bridge: Arc<SharedWallpaperBridge>,
    target_bounds_logical: (f32, f32, f32, f32),
    hwnd: isize,
    attached_gen: u64,
    scene: Option<PlayerScene>,
    bg_surface: Option<CustomSurfaceHandle>,
    bg_size: (u32, u32),
    cover_surface: Option<CustomSurfaceHandle>,
    applied_cover_sig: u64,
    shaper: Shaper,
    row_motions: HashMap<usize, RowMotion>,
    bg_alphas: HashMap<usize, f32>,
    last_track_sig: String,
    last_active_idx: Option<usize>,
    started: Instant,
    last_ms: f64,
    paused_since_ms: Option<f64>,
    last_occlusion_check_ms: f64,
    occluded: bool,
    sleep_poll_pending: bool,
}

impl WallpaperBgView {
    pub fn new(
        bridge: Arc<SharedWallpaperBridge>,
        target_bounds_logical: (f32, f32, f32, f32),
        text_system: Arc<TextSystem>,
    ) -> Self {
        Self {
            bridge,
            target_bounds_logical,
            hwnd: 0,
            attached_gen: u64::MAX,
            scene: None,
            bg_surface: None,
            bg_size: (0, 0),
            cover_surface: None,
            applied_cover_sig: 0,
            shaper: Shaper::new(text_system, 128),
            row_motions: HashMap::new(),
            bg_alphas: HashMap::new(),
            last_track_sig: String::new(),
            last_active_idx: None,
            started: Instant::now(),
            last_ms: 0.0,
            paused_since_ms: None,
            last_occlusion_check_ms: -1000.0,
            occluded: false,
            sleep_poll_pending: false,
        }
    }

    fn now_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    fn target_rect_phys(&self, scale_factor: f32) -> (i32, i32, i32, i32) {
        let sf = scale_factor.max(1.0);
        let (lx, ly, lw, lh) = self.target_bounds_logical;
        (
            (lx * sf).round() as i32,
            (ly * sf).round() as i32,
            (lw * sf).round().max(1.0) as i32,
            (lh * sf).round().max(1.0) as i32,
        )
    }
}

impl Drop for WallpaperBgView {
    fn drop(&mut self) {
        if self.hwnd != 0 {
            self.bridge.unregister_hwnd(self.hwnd);
            detach_from_workerw_and_refresh(self.hwnd);
        }
    }
}

impl Render for WallpaperBgView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = self.now_ms();
        let dt = (now - self.last_ms).clamp(1.0, 80.0);
        self.last_ms = now;
        let sf = window.scale_factor().max(1.0);
        let phys_rect = self.target_rect_phys(sf);
        let taskbar_gen = self.bridge.taskbar_recreated_gen.load(Ordering::SeqCst);

        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if self.hwnd != hwnd {
                self.hwnd = hwnd;
                self.bridge.register_hwnd(hwnd);
            }
            if self.attached_gen != taskbar_gen {
                self.attached_gen = taskbar_gen;
                let (x, y, w, h) = phys_rect;
                window.defer(cx, move |_, _| {
                    attach_to_workerw(hwnd, x, y, w, h);
                });
            }
        }

        if self.scene.is_none() {
            if let Ok((dev, ctx)) = window.gpu_device_handle() {
                self.scene = Some(PlayerScene::new(dev, ctx));
            }
        }

        let snap = self.bridge.read_snapshot();
        let live_pos_ms = if snap.playing {
            snap.position_ms + snap.sampled_at.elapsed().as_secs_f64() * 1000.0
        } else {
            snap.position_ms
        };

        if snap.playing {
            self.paused_since_ms = None;
        } else if self.paused_since_ms.is_none() {
            self.paused_since_ms = Some(now);
        }

        if now - self.last_occlusion_check_ms >= OCCLUSION_POLL_INTERVAL_MS {
            self.last_occlusion_check_ms = now;
            let own = self.bridge.own_hwnds();
            self.occluded = is_desktop_occluded_or_locked(phys_rect, &own);
        }

        let pause_slept = self
            .paused_since_ms
            .is_some_and(|t| now - t >= PAUSE_SLEEP_TIMEOUT_MS);
        let sleeping = self.occluded || pause_slept;

        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let win_h = f32::from(vp.height).max(1.0);
        let phys_w = (win_w * sf).round().max(1.0) as u32;
        let phys_h = (win_h * sf).round().max(1.0) as u32;

        if self.applied_cover_sig != snap.cover_sig {
            self.applied_cover_sig = snap.cover_sig;
            if let Some(old) = self.cover_surface.take() {
                window.destroy_custom_surface(old);
            }
            if let Some(cover) = snap.cover.as_ref() {
                if let Some(scene) = self.scene.as_mut() {
                    let _ = scene.set_cover(cover);
                }
                if let Ok(handle) =
                    window.create_custom_surface(cover.width, cover.height, &cover.rgba.pixels)
                {
                    self.cover_surface = Some(handle);
                }
            } else if let Some(scene) = self.scene.as_mut() {
                scene.clear_cover();
            }
        }

        if let Some(scene) = self.scene.as_mut() {
            scene.set_beat(snap.beat);
            if !sleeping {
                scene.resize(phys_w, phys_h);
                let _ = scene.tick(now, live_pos_ms, snap.playing, true);
                if let Some(tex) = scene.background_texture() {
                    let sz = scene.background_size();
                    if self.bg_surface.is_none() || self.bg_size != sz {
                        if let Some(old) = self.bg_surface.take() {
                            window.destroy_custom_surface(old);
                        }
                        if let Ok(handle) = window.register_gpu_surface(tex) {
                            self.bg_surface = Some(handle);
                            self.bg_size = sz;
                        }
                    }
                }
            }
        }

        // Layout cover + meta + AMLL lyrics (without bottom transport/progress bar).
        let d = snap.display;
        let has_lyrics = !snap.rows.is_empty();
        let m = model::metrics(win_w, win_h, (win_w, win_h), false, &d);

        let cover_size = (win_h * 0.42).clamp(220.0, 440.0);
        let cover_rect = if has_lyrics {
            let left_col_cx = win_w * 0.27;
            let top_y = (win_h - cover_size - 78.0) * 0.5;
            Box4::new(left_col_cx - cover_size * 0.5, top_y, cover_size, cover_size)
        } else {
            let top_y = (win_h - cover_size - 78.0) * 0.5;
            Box4::new((win_w - cover_size) * 0.5, top_y, cover_size, cover_size)
        };
        let cover_radius = (d.cover_radius * (cover_size / 360.0)).clamp(10.0, cover_size * 0.5);

        let lyric_x = win_w * 0.48;
        let lyric_w = (win_w * 0.44).max(300.0);
        let lyric_top = win_h * 0.08;
        let lyric_h = win_h * 0.84;

        let track_sig = format!("{}\0{}", snap.track_title, snap.track_artist);
        let mut snap_instant = !self.last_track_sig.is_empty() && track_sig != self.last_track_sig;
        if track_sig != self.last_track_sig {
            self.last_track_sig = track_sig;
            self.row_motions.clear();
            self.bg_alphas.clear();
        }
        if snap.active_row_index != self.last_active_idx {
            let prev = self.last_active_idx;
            self.last_active_idx = snap.active_row_index;
            if let (Some(p), Some(n)) = (prev, snap.active_row_index) {
                if n < p || n.saturating_sub(p) > 4 {
                    snap_instant = true;
                }
            } else {
                snap_instant = true;
            }
        }

        struct PaintRow {
            idx: usize,
            row: Row,
            shaped: Arc<RowLines>,
            y: f32,
            bg_alpha: f32,
        }
        let mut paint_rows: Vec<PaintRow> = Vec::new();
        if has_lyrics && !sleeping {
            let anchor_idx = snap
                .active_row_index
                .unwrap_or(0)
                .min(snap.rows.len().saturating_sub(1));
            let win_start = anchor_idx.saturating_sub(5);
            let win_end = (anchor_idx + 10).min(snap.rows.len());
            let spring_params = match d.spring_feel {
                SpringFeel::Lively => SpringParams::new(260.0, 26.0),
                SpringFeel::Soft => SpringParams::new(125.0, 22.0),
                SpringFeel::Standard => SpringParams::new(180.0, 24.0),
            };
            let gap = (m.font * (0.42 + d.line_gap.max(0.0))).max(14.0);
            let mut shaped_win: Vec<(usize, Row, Arc<RowLines>, f32, f32)> = Vec::new();
            for idx in win_start..win_end {
                let r = &snap.rows[idx];
                if r.text.trim().is_empty() {
                    continue;
                }
                let bg_active = r.is_bg
                    && live_pos_ms >= r.start_ms - 400.0
                    && live_pos_ms <= r.end_ms + 450.0;
                let bg_target = if !r.is_bg || bg_active { 1.0_f32 } else { 0.0_f32 };
                let bg_alpha = {
                    let a = self.bg_alphas.entry(idx).or_insert(bg_target);
                    *a += (bg_target - *a) * (dt as f32 / 180.0).clamp(0.0, 1.0);
                    *a
                };
                if r.is_bg && bg_alpha < 0.02 {
                    continue;
                }
                let mut row_m = m.clone();
                if r.is_bg {
                    row_m.font = (m.font * d.bg_scale.clamp(0.55, 0.90)).max(16.0);
                    row_m.line_height = row_m.font * d.line_height_em.max(1.15);
                }
                if let Ok(s) = self.shaper.row(r, &row_m, &d, lyric_w, 0xA100) {
                    let h = (s.height + gap) * if r.is_bg { bg_alpha } else { 1.0 };
                    shaped_win.push((idx, r.clone(), s, h, bg_alpha));
                }
            }
            let anchor_pos = shaped_win
                .iter()
                .position(|(i, _, _, _, _)| *i == anchor_idx)
                .unwrap_or(0);
            let anchor_y = lyric_top + lyric_h * (d.top_offset / 100.0).clamp(0.18, 0.50);
            let mut target_ys = vec![0.0_f32; shaped_win.len()];
            if !shaped_win.is_empty() {
                target_ys[anchor_pos] = anchor_y;
                let mut y_down = anchor_y + shaped_win[anchor_pos].3;
                for k in (anchor_pos + 1)..shaped_win.len() {
                    target_ys[k] = y_down;
                    y_down += shaped_win[k].3;
                }
                let mut y_up = anchor_y;
                for k in (0..anchor_pos).rev() {
                    y_up -= shaped_win[k].3;
                    target_ys[k] = y_up;
                }
            }
            for (k, (idx, row, shaped, _, bg_alpha)) in shaped_win.into_iter().enumerate() {
                let ty = target_ys[k] as f64;
                let dist = idx.abs_diff(anchor_idx);
                let sp = self
                    .row_motions
                    .entry(idx)
                    .or_insert_with(|| RowMotion::new(ty));
                let direct = snap_instant || !d.enable_spring;
                let delay_s = (dist.min(6) as f64) * 0.036;
                let y = sp.step(ty, direct, delay_s, dt, spring_params) as f32;
                paint_rows.push(PaintRow {
                    idx,
                    row,
                    shaped,
                    y,
                    bg_alpha,
                });
            }
            self.row_motions
                .retain(|&i, _| i >= win_start.saturating_sub(2) && i <= win_end + 2);
            self.bg_alphas
                .retain(|&i, _| i >= win_start.saturating_sub(2) && i <= win_end + 2);
        }

        if sleeping {
            if !self.sleep_poll_pending {
                self.sleep_poll_pending = true;
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        view.sleep_poll_pending = false;
                        cx.notify();
                    });
                })
                .detach();
            }
        } else {
            window.request_animation_frame();
        }

        let bg = self
            .scene
            .as_ref()
            .is_some_and(PlayerScene::has_background)
            .then_some(self.bg_surface)
            .flatten();
        let cover_surface = self.cover_surface;
        let title = snap.track_title.clone();
        let artist = snap.track_artist.clone();
        let active_idx = snap.active_row_index;

        div()
            .size_full()
            .bg(hsla(0.0, 0.0, 0.06, 1.0))
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        chrome::set_light_theme(false);
                        // 1. Dynamic immersive GPU background
                        if let Some(surface) = bg {
                            window.paint_custom_surface_rounded(
                                Bounds {
                                    origin: point(px(0.0), px(0.0)),
                                    size: size(px(win_w), px(win_h)),
                                },
                                px(0.0),
                                surface,
                            );
                        }

                        // 2. Cover art + track title + artist (no bottom controls!)
                        if let Some(still) = cover_surface {
                            chrome::fill_round(
                                window,
                                cover_rect.x + 2.0,
                                cover_rect.y + 12.0,
                                cover_rect.w,
                                cover_rect.h,
                                cover_radius,
                                Hsla::from(rgb(0x000000)).opacity(0.38),
                            );
                            window.paint_custom_surface_rounded(
                                Bounds {
                                    origin: point(px(cover_rect.x), px(cover_rect.y)),
                                    size: size(px(cover_rect.w), px(cover_rect.h)),
                                },
                                px(cover_radius),
                                still,
                            );
                            chrome::stroke_round(
                                window,
                                cover_rect.x,
                                cover_rect.y,
                                cover_rect.w,
                                cover_rect.h,
                                cover_radius,
                                1.0,
                                hsla(0.0, 0.0, 1.0, 0.14),
                            );
                        }
                        let meta_y = cover_rect.bottom() + 20.0;
                        let title_str = if title.trim().is_empty() {
                            "OverLyrics"
                        } else {
                            title.trim()
                        };
                        if has_lyrics {
                            chrome::paint_text_fitted(
                                window,
                                title_str,
                                cover_rect.x,
                                meta_y,
                                cover_rect.w,
                                22.0,
                                700.0,
                                hsla(0.0, 0.0, 1.0, 0.96),
                            );
                            if !artist.trim().is_empty() {
                                chrome::paint_text_fitted(
                                    window,
                                    artist.trim(),
                                    cover_rect.x,
                                    meta_y + 30.0,
                                    cover_rect.w,
                                    15.0,
                                    500.0,
                                    hsla(0.0, 0.0, 1.0, 0.66),
                                );
                            }
                        } else {
                            let (tw, _) = chrome::measure(window, title_str, 24.0, 700.0);
                            chrome::paint_text(
                                window,
                                title_str,
                                (win_w - tw.min(cover_rect.w + 160.0)) * 0.5,
                                meta_y,
                                24.0,
                                700.0,
                                hsla(0.0, 0.0, 1.0, 0.96),
                            );
                            if !artist.trim().is_empty() {
                                let (aw, _) = chrome::measure(window, artist.trim(), 16.0, 500.0);
                                chrome::paint_text(
                                    window,
                                    artist.trim(),
                                    (win_w - aw.min(cover_rect.w + 160.0)) * 0.5,
                                    meta_y + 32.0,
                                    16.0,
                                    500.0,
                                    hsla(0.0, 0.0, 1.0, 0.66),
                                );
                            }
                        }

                        // 3. AMLL lyrics column
                        if !paint_rows.is_empty() {
                            let lyric_bot = lyric_top + lyric_h;
                            let feather = d.edge_feather.max(48.0);
                            window.with_content_mask(
                                Some(ContentMask {
                                    bounds: Bounds {
                                        origin: point(px(lyric_x - 12.0), px(lyric_top)),
                                        size: size(px(lyric_w + 24.0), px(lyric_h)),
                                    },
                                }),
                                |window| {
                                    for item in &paint_rows {
                                        let r = &item.row;
                                        let s = &item.shaped;
                                        if item.y + s.height < lyric_top - 16.0
                                            || item.y > lyric_bot + 16.0
                                        {
                                            continue;
                                        }
                                        let top_f = ((item.y - lyric_top) / feather).clamp(0.0, 1.0);
                                        let bot_f =
                                            ((lyric_bot - (item.y + s.height)) / feather).clamp(0.0, 1.0);
                                        let edge_f = top_f.min(bot_f);
                                        let is_active = Some(item.idx) == active_idx
                                            || (r.is_bg
                                                && live_pos_ms >= r.start_ms
                                                && live_pos_ms <= r.end_ms + 300.0);
                                        let dim = (if is_active {
                                            if r.is_bg { 0.86 } else { 1.0 }
                                        } else if r.is_bg {
                                            0.42
                                        } else {
                                            d.bg_opacity.clamp(0.26, 0.56)
                                        }) * item.bg_alpha
                                            * edge_f;
                                        if dim < 0.02 {
                                            continue;
                                        }
                                        let light = row_light(r, live_pos_ms, is_active, false, &d);
                                        let prog = paint::line_progress(
                                            live_pos_ms,
                                            r.start_ms,
                                            r.duration_ms(),
                                        );
                                        let masks: Vec<f32> = s
                                            .masks
                                            .iter()
                                            .map(|frames| position_at(frames, prog))
                                            .collect();
                                        let row_light_val = RowLight {
                                            base: (light.dark * dim).clamp(0.0, 1.0),
                                            dark: (light.dark * dim).clamp(0.0, 1.0),
                                            bright: (light.bright * dim).clamp(0.0, 1.0),
                                            mask: if is_active && light.sweep.is_some() {
                                                Some(masks)
                                            } else {
                                                None
                                            },
                                            glow: if is_active {
                                                light.glow.map(|mut g| {
                                                    for v in &mut g {
                                                        *v *= dim;
                                                    }
                                                    g
                                                })
                                            } else {
                                                None
                                            },
                                        };
                                        let align = if r.is_duet {
                                            LineAlign::Right
                                        } else {
                                            LineAlign::Left
                                        };
                                        paint::paint_row_with_shadow(
                                            window,
                                            s,
                                            lyric_x,
                                            item.y,
                                            s.line_height,
                                            s.font_size,
                                            &s.words,
                                            &row_light_val,
                                            live_pos_ms,
                                            1.0,
                                            WrapperTransform::default(),
                                            -10000.0,
                                            10000.0,
                                            1.0,
                                            lyric_w,
                                            align,
                                            0.0,
                                            0.45,
                                            None,
                                        );
                                    }
                                },
                            );
                        }
                    },
                )
                .size_full(),
            )
    }
}
