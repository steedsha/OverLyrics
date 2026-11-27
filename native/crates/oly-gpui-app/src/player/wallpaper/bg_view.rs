//! Pure GPU immersive background window attached to Windows `WorkerW`
//! with automatic fullscreen / maximized-occlusion / lockscreen / pause-timeout sleep.

use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

use gpui_kit::{
    Bounds, Context, CustomSurfaceHandle, IntoElement, ParentElement, Render, Styled, Window,
    canvas, div, hsla, point, px, size,
};
use oly_ui::win32_window;

use super::SharedWallpaperBridge;
use super::win32_workerw::{attach_to_workerw, detach_from_workerw_and_refresh, is_desktop_occluded_or_locked};
use crate::player::scene::PlayerScene;

const PAUSE_SLEEP_TIMEOUT_MS: f64 = 30_000.0;
const OCCLUSION_POLL_INTERVAL_MS: f64 = 260.0;

pub struct WallpaperBgView {
    bridge: Arc<SharedWallpaperBridge>,
    target_rect_phys: (i32, i32, i32, i32),
    hwnd: isize,
    attached_gen: u64,
    scene: Option<PlayerScene>,
    bg_surface: Option<CustomSurfaceHandle>,
    bg_size: (u32, u32),
    applied_cover_sig: u64,
    started: Instant,
    paused_since_ms: Option<f64>,
    last_occlusion_check_ms: f64,
    occluded: bool,
    sleep_poll_pending: bool,
}

impl WallpaperBgView {
    pub fn new(
        bridge: Arc<SharedWallpaperBridge>,
        target_rect_phys: (i32, i32, i32, i32),
    ) -> Self {
        Self {
            bridge,
            target_rect_phys,
            hwnd: 0,
            attached_gen: u64::MAX,
            scene: None,
            bg_surface: None,
            bg_size: (0, 0),
            applied_cover_sig: 0,
            started: Instant::now(),
            paused_since_ms: None,
            last_occlusion_check_ms: -1000.0,
            occluded: false,
            sleep_poll_pending: false,
        }
    }

    fn now_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
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
        let taskbar_gen = self.bridge.taskbar_recreated_gen.load(Ordering::SeqCst);

        if let Some(hwnd) = win32_window::hwnd_from_window(window) {
            if self.hwnd != hwnd {
                self.hwnd = hwnd;
                self.bridge.register_hwnd(hwnd);
            }
            if self.attached_gen != taskbar_gen {
                self.attached_gen = taskbar_gen;
                let (x, y, w, h) = self.target_rect_phys;
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

        // Track pause duration for auto-sleep.
        if snap.playing {
            self.paused_since_ms = None;
        } else if self.paused_since_ms.is_none() {
            self.paused_since_ms = Some(now);
        }

        if now - self.last_occlusion_check_ms >= OCCLUSION_POLL_INTERVAL_MS {
            self.last_occlusion_check_ms = now;
            let own = self.bridge.own_hwnds();
            self.occluded = is_desktop_occluded_or_locked(self.target_rect_phys, &own);
        }

        let pause_slept = self
            .paused_since_ms
            .is_some_and(|t| now - t >= PAUSE_SLEEP_TIMEOUT_MS);
        let sleeping = self.occluded || pause_slept;

        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let win_h = f32::from(vp.height).max(1.0);
        let sf = window.scale_factor().max(1.0);
        let phys_w = (win_w * sf).round().max(1.0) as u32;
        let phys_h = (win_h * sf).round().max(1.0) as u32;

        if let Some(scene) = self.scene.as_mut() {
            scene.set_beat(snap.beat);
            if self.applied_cover_sig != snap.cover_sig {
                self.applied_cover_sig = snap.cover_sig;
                if let Some(cover) = snap.cover.as_ref() {
                    let _ = scene.set_cover(cover);
                } else {
                    scene.clear_cover();
                }
            }
            if !sleeping {
                scene.resize(phys_w, phys_h);
                let _ = scene.tick(now, 0.0, snap.playing, true);
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

        if sleeping {
            // 0% GPU while occluded, locked, or pause-timed-out; check at 4Hz for instant wake.
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

        div()
            .size_full()
            .bg(hsla(0.0, 0.0, 0.06, 1.0))
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
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
                    },
                )
                .size_full(),
            )
    }
}
