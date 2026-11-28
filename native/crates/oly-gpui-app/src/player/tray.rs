//! Windows system tray icon and Liquid Glass context menu.
//!
//! - Left click: restore / focus the main window.
//! - Right click: open a compact Liquid Glass menu following the active theme
//!   (`Light` / `Dark` / `LiquidGlass`), grouped as:
//!   `显示主界面` / `桌面歌词` / `锁定桌面歌词` — `设置` / `检查更新` — `退出`.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicIsize, Ordering},
};
use std::time::Instant;

use gpui_kit::*;

use super::chrome;
use super::elastic::{Elastic, PressGesture};
use super::icons::{self, Icon};
use super::interaction::Feedback;
use super::model::{Box4, ThemeMode};
use super::panel_motion::PanelMotion;
use super::win32_window;

const TRAY_MENU_PAD: f32 = 12.0;
pub const TRAY_MENU_CARD_W: f32 = 216.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    ShowMain,
    ContextMenu(i32, i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrayAction {
    ShowMain,
    ToggleDesktopLyrics,
    ToggleDesktopLock,
    ToggleWallpaper,
    OpenSettings,
    Quit,
}

#[derive(Clone, Debug)]
pub struct TrayMenuSnapshot {
    pub theme_mode: ThemeMode,
    pub corner_radius: f32,
    pub accent_hex: u32,
    pub desktop_open: bool,
    pub desktop_locked: bool,
    pub wallpaper_open: bool,
}

impl Default for TrayMenuSnapshot {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::LiquidGlass,
            corner_radius: 16.0,
            accent_hex: 0x0A84FF,
            desktop_open: false,
            desktop_locked: false,
            wallpaper_open: false,
        }
    }
}

#[derive(Default)]
pub struct SharedTrayBridge {
    events: Mutex<Vec<TrayEvent>>,
    actions: Mutex<Vec<TrayAction>>,
    snapshot: Mutex<TrayMenuSnapshot>,
    pending: AtomicBool,
}

impl SharedTrayBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn push_event(&self, ev: TrayEvent) {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).push(ev);
        self.pending.store(true, Ordering::SeqCst);
    }

    pub fn push_action(&self, act: TrayAction) {
        self.actions.lock().unwrap_or_else(|e| e.into_inner()).push(act);
        self.pending.store(true, Ordering::SeqCst);
    }

    pub fn has_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }

    pub fn drain_events(&self) -> Vec<TrayEvent> {
        let out = std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()));
        if self.actions.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
            self.pending.store(false, Ordering::SeqCst);
        }
        out
    }

    pub fn drain_actions(&self) -> Vec<TrayAction> {
        let out = std::mem::take(&mut *self.actions.lock().unwrap_or_else(|e| e.into_inner()));
        if self.events.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
            self.pending.store(false, Ordering::SeqCst);
        }
        out
    }

    pub fn update_snapshot(&self, snap: TrayMenuSnapshot) {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = snap;
    }

    pub fn read_snapshot(&self) -> TrayMenuSnapshot {
        self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub struct TrayService {
    hwnd: Arc<AtomicIsize>,
    join: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "windows")]
static GLOBAL_TRAY_BRIDGE: Mutex<Option<Arc<SharedTrayBridge>>> = Mutex::new(None);
#[cfg(target_os = "windows")]
static TASKBAR_CREATED_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(target_os = "windows")]
const WM_TRAYICON: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
#[cfg(target_os = "windows")]
const TRAY_UID: u32 = 1;

#[cfg(target_os = "windows")]
unsafe fn register_tray_icon(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{IDI_APPLICATION, LoadIconW};
    use windows::core::PCWSTR;

    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let hicon = LoadIconW(Some(hinst.into()), PCWSTR(1usize as *const u16))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default();

        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_UID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: hicon,
            ..Default::default()
        };
        let tip: Vec<u16> = "OverLyrics".encode_utf16().chain(std::iter::once(0)).collect();
        let len = tip.len().min(nid.szTip.len());
        nid.szTip[..len].copy_from_slice(&tip[..len]);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

#[cfg(target_os = "windows")]
unsafe fn unregister_tray_icon(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::Shell::{NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn tray_wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{LRESULT, POINT};
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, GetCursorPos, PostQuitMessage, WM_CLOSE, WM_CONTEXTMENU,
        WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
    };

    unsafe {
        let taskbar_msg = TASKBAR_CREATED_MSG.load(Ordering::Relaxed);
        if taskbar_msg != 0 && msg == taskbar_msg {
            register_tray_icon(hwnd);
            return LRESULT(0);
        }

        match msg {
            WM_TRAYICON => {
                let ev = (lparam.0 & 0xFFFF) as u32;
                match ev {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                        if let Some(b) = GLOBAL_TRAY_BRIDGE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                            b.push_event(TrayEvent::ShowMain);
                        }
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => {
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        if let Some(b) = GLOBAL_TRAY_BRIDGE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                            b.push_event(TrayEvent::ContextMenu(pt.x, pt.y));
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                unregister_tray_icon(hwnd);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

impl TrayService {
    pub fn start(bridge: Arc<SharedTrayBridge>) -> Self {
        let hwnd_slot = Arc::new(AtomicIsize::new(0));
        let hwnd_bg = hwnd_slot.clone();

        #[cfg(target_os = "windows")]
        {
            *GLOBAL_TRAY_BRIDGE.lock().unwrap_or_else(|e| e.into_inner()) = Some(bridge);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = bridge;

        let join = std::thread::Builder::new()
            .name("overlyrics-tray".into())
            .spawn(move || {
                #[cfg(target_os = "windows")]
                unsafe {
                    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
                    use windows::Win32::UI::WindowsAndMessaging::{
                        CW_USEDEFAULT, CreateWindowExW, DispatchMessageW, GetMessageW, MSG,
                        RegisterClassW, RegisterWindowMessageW, TranslateMessage, WNDCLASSW,
                        WS_OVERLAPPED,
                    };
                    use windows::core::w;

                    let hinst = GetModuleHandleW(None).unwrap_or_default();
                    let class_name = w!("OverLyricsTrayHiddenWindow");
                    let wc = WNDCLASSW {
                        lpfnWndProc: Some(tray_wnd_proc),
                        hInstance: hinst.into(),
                        lpszClassName: class_name,
                        ..Default::default()
                    };
                    let _ = RegisterClassW(&wc);
                    let tb_msg = RegisterWindowMessageW(w!("TaskbarCreated"));
                    TASKBAR_CREATED_MSG.store(tb_msg, Ordering::Relaxed);

                    let Ok(hwnd) = CreateWindowExW(
                        Default::default(),
                        class_name,
                        w!("OverLyrics Tray"),
                        WS_OVERLAPPED,
                        CW_USEDEFAULT,
                        CW_USEDEFAULT,
                        0,
                        0,
                        None,
                        None,
                        Some(hinst.into()),
                        None,
                    ) else {
                        return;
                    };
                    hwnd_bg.store(hwnd.0 as isize, Ordering::SeqCst);
                    register_tray_icon(hwnd);

                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    hwnd_bg.store(0, Ordering::SeqCst);
                }
                #[cfg(not(target_os = "windows"))]
                let _ = hwnd_bg;
            })
            .ok();

        Self {
            hwnd: hwnd_slot,
            join,
        }
    }
}

impl Drop for TrayService {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        unsafe {
            let raw = self.hwnd.swap(0, Ordering::SeqCst);
            if raw != 0 {
                let hwnd = windows::Win32::Foundation::HWND(raw as *mut std::ffi::c_void);
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(hwnd),
                    windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    Default::default(),
                    Default::default(),
                );
            }
        }
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
        #[cfg(target_os = "windows")]
        {
            *GLOBAL_TRAY_BRIDGE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

/// Bring a Win32 window to the foreground, restoring it if minimized or hidden.
pub fn focus_main_hwnd(raw_hwnd: isize) {
    #[cfg(target_os = "windows")]
    unsafe {
        if raw_hwnd == 0 {
            return;
        }
        let hwnd = windows::Win32::Foundation::HWND(raw_hwnd as *mut std::ffi::c_void);
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
        );
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOW,
        );
        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = raw_hwnd;
}

/// Returns true when the given Win32 window is currently visible and not minimized.
pub fn is_hwnd_visible(raw_hwnd: isize) -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        if raw_hwnd == 0 {
            return false;
        }
        let hwnd = windows::Win32::Foundation::HWND(raw_hwnd as *mut std::ffi::c_void);
        windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool()
            && !windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = raw_hwnd;
        false
    }
}

/// Compact Liquid Glass tray context menu reusing `oly-ui` glass, switch, and elastic drag primitives.
pub struct TrayMenuPopover {
    bridge: Arc<SharedTrayBridge>,
    motion: PanelMotion,
    feedback: HashMap<TrayAction, Feedback>,
    elastics: HashMap<TrayAction, Elastic>,
    panel_elastic: Elastic,
    press_gesture: PressGesture,
    pressed: Option<TrayAction>,
    panel_dragging: bool,
    card_rect: Box4,
    was_active: bool,
    configured: bool,
    cursor: Option<(f32, f32)>,
    hits: Vec<(TrayAction, Box4)>,
    started: Instant,
    last_ms: f64,
}

impl TrayMenuPopover {
    pub fn new(bridge: Arc<SharedTrayBridge>) -> Self {
        Self {
            bridge,
            motion: PanelMotion::default(),
            feedback: HashMap::new(),
            elastics: HashMap::new(),
            panel_elastic: Elastic::default(),
            press_gesture: PressGesture::default(),
            pressed: None,
            panel_dragging: false,
            card_rect: Box4::default(),
            was_active: false,
            configured: false,
            cursor: None,
            hits: Vec::new(),
            started: Instant::now(),
            last_ms: 0.0,
        }
    }

    pub fn window_size(desktop_open: bool) -> (f32, f32) {
        let card_h = if desktop_open { 242.0 } else { 206.0 };
        (
            TRAY_MENU_CARD_W + TRAY_MENU_PAD * 2.0,
            card_h + TRAY_MENU_PAD * 2.0,
        )
    }
}

impl Render for TrayMenuPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        if !self.configured {
            if let Some(hwnd) = win32_window::hwnd_from_window(window) {
                self.configured = true;
                window.defer(cx, move |_, _| {
                    win32_window::configure_desktop_overlay(hwnd);
                });
            }
        }
        if window.is_window_active() {
            self.was_active = true;
        } else if self.was_active && !self.panel_dragging {
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

        let snap = self.bridge.read_snapshot();
        let vp = window.viewport_size();
        let win_w = f32::from(vp.width).max(1.0);
        let win_h = f32::from(vp.height).max(1.0);

        let raw_delta = self.press_gesture.delta();
        let panel_held = self.panel_dragging || (self.pressed.is_some() && self.press_gesture.dragged());
        self.panel_elastic.step(raw_delta, panel_held, dt);
        let pp = self.panel_elastic.pull();

        let card_w = (win_w - TRAY_MENU_PAD * 2.0).max(120.0);
        let card_h = (win_h - TRAY_MENU_PAD * 2.0).max(100.0);
        let card = Box4::new(TRAY_MENU_PAD + pp[0], TRAY_MENU_PAD + pp[1], card_w, card_h);
        self.card_rect = card;

        let row_x = card.x + 6.0;
        let row_w = (card.w - 12.0).max(40.0);
        let row_h = 35.0_f32;
        let r_show = Box4::new(row_x, card.y + 8.0, row_w, row_h);
        let r_desk = Box4::new(row_x, card.y + 44.0, row_w, row_h);
        let r_lock = snap
            .desktop_open
            .then(|| Box4::new(row_x, card.y + 80.0, row_w, row_h));
        let wall_y = if snap.desktop_open {
            card.y + 116.0
        } else {
            card.y + 80.0
        };
        let r_wall = Box4::new(row_x, wall_y, row_w, row_h);
        let div1_y = r_wall.bottom() + 4.0;
        let r_settings = Box4::new(row_x, div1_y + 6.0, row_w, row_h);
        let div2_y = r_settings.bottom() + 4.0;
        let r_quit = Box4::new(row_x, div2_y + 6.0, row_w, row_h);

        let mut hits = vec![
            (TrayAction::ShowMain, r_show),
            (TrayAction::ToggleDesktopLyrics, r_desk),
        ];
        if let Some(rl) = r_lock {
            hits.push((TrayAction::ToggleDesktopLock, rl));
        }
        hits.push((TrayAction::ToggleWallpaper, r_wall));
        hits.push((TrayAction::OpenSettings, r_settings));
        hits.push((TrayAction::Quit, r_quit));
        self.hits = hits.clone();

        for (act, rect) in &hits {
            let hot = self.cursor.is_some_and(|(mx, my)| rect.contains(mx, my));
            let held = self.pressed == Some(*act);
            let fb = self.feedback.entry(*act).or_default();
            fb.step(hot, held, dt);
            let el = self.elastics.entry(*act).or_default();
            el.step(raw_delta, held, dt);
            fb.pull = el.pull();
            fb.squeeze = el.squeeze();
            fb.press = el.pressure().clamp(0.0, 1.0);
        }

        let fb_map = self.feedback.clone();
        let surf_a = self.motion.surface_alpha();
        let cont_a = self.motion.content_alpha();
        let accent_hex = snap.accent_hex;

        div()
            .size_full()
            .bg(gpui_kit::transparent_black())
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                this.cursor = Some((x, y));
                if this.pressed.is_some() || this.panel_dragging {
                    this.press_gesture.update(x, y);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                    this.cursor = Some((x, y));
                    let hit = this
                        .hits
                        .iter()
                        .find(|(_, r)| r.contains(x, y))
                        .map(|(a, _)| *a);
                    this.press_gesture.begin(x, y);
                    if let Some(a) = hit {
                        this.pressed = Some(a);
                        this.panel_dragging = false;
                    } else if this.card_rect.contains(x, y) {
                        this.pressed = None;
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
                    let pressed = this.pressed.take();
                    let cur = this
                        .hits
                        .iter()
                        .find(|(_, r)| r.contains(x, y))
                        .map(|(a, _)| *a);
                    let same = pressed.is_some() && cur == pressed;
                    if this.press_gesture.release(same) {
                        if let Some(act) = pressed {
                            this.bridge.push_action(act);
                            if act != TrayAction::ToggleDesktopLock {
                                window.remove_window();
                            }
                        }
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, _, window, cx| {
                        let dark_card = snap.theme_mode == ThemeMode::Dark;
                        chrome::set_light_theme(!dark_card);
                        let card_bg = if dark_card {
                            hsla(0.0, 0.0, 0.125, 0.98 * surf_a)
                        } else {
                            hsla(0.0, 0.0, 0.985, 0.99 * surf_a)
                        };
                        let card_border = if dark_card {
                            hsla(0.0, 0.0, 1.0, 0.13 * surf_a)
                        } else {
                            hsla(0.0, 0.0, 0.0, 0.11 * surf_a)
                        };
                        let menu_r = 12.0_f32;
                        chrome::fill_round(window, card.x, card.y, card.w, card.h, menu_r, card_bg);
                        chrome::stroke_round(window, card.x, card.y, card.w, card.h, menu_r, 1.0, card_border);

                        let fg_main = if dark_card {
                            hsla(0.0, 0.0, 0.96, 0.94 * cont_a)
                        } else {
                            hsla(0.0, 0.0, 0.10, 0.92 * cont_a)
                        };
                        let fg_icon = if dark_card {
                            hsla(0.0, 0.0, 0.92, 0.78 * cont_a)
                        } else {
                            hsla(0.0, 0.0, 0.16, 0.78 * cont_a)
                        };
                        let div_col = if dark_card {
                            hsla(0.0, 0.0, 1.0, 0.10 * cont_a)
                        } else {
                            hsla(0.0, 0.0, 0.0, 0.08 * cont_a)
                        };

                        let paint_row = |window: &mut Window,
                                         cx: &mut App,
                                         act: TrayAction,
                                         r: Box4,
                                         icon: Icon,
                                         label: &str,
                                         checked: bool,
                                         danger: bool| {
                            let f = fb_map.get(&act).copied().unwrap_or_default();
                            if f.hover > 0.01 || f.press > 0.01 {
                                let hover_bg = if danger {
                                    Hsla::from(rgb(0xFF3B30)).opacity((0.12 * f.hover + 0.06 * f.press) * cont_a)
                                } else if dark_card {
                                    hsla(0.0, 0.0, 1.0, (0.09 * f.hover + 0.05 * f.press) * cont_a)
                                } else {
                                    hsla(0.0, 0.0, 0.0, (0.06 * f.hover + 0.04 * f.press) * cont_a)
                                };
                                chrome::fill_round(window, r.x, r.y, r.w, r.h, 8.0, hover_bg);
                            }
                            let red_hover = danger && f.hover > 0.05;
                            let icon_col = if red_hover {
                                Hsla::from(rgb(0xFF3B30)).opacity((0.90 + 0.08 * f.hover) * cont_a)
                            } else if checked {
                                chrome::accent_rgb(accent_hex, 0.95 * cont_a)
                            } else {
                                fg_icon
                            };
                            let label_col = if red_hover {
                                Hsla::from(rgb(0xFF3B30)).opacity((0.92 + 0.06 * f.hover) * cont_a)
                            } else {
                                fg_main
                            };
                            icons::paint(
                                window,
                                cx,
                                icon,
                                Box4::new(card.x + 14.0, r.y + (r.h - 16.0) * 0.5, 16.0, 16.0),
                                icon_col,
                            );
                            chrome::paint_text_vcentred(
                                window,
                                label,
                                card.x + 38.0,
                                r.centre().1,
                                12.5,
                                500.0,
                                label_col,
                            );
                            if checked {
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
                        };

                        paint_row(window, cx, TrayAction::ShowMain, r_show, Icon::CoverExpand, "显示界面", false, false);
                        paint_row(window, cx, TrayAction::ToggleDesktopLyrics, r_desk, Icon::DesktopLyrics, "桌面歌词", snap.desktop_open, false);

                        if let Some(rl) = r_lock {
                            paint_row(
                                window,
                                cx,
                                TrayAction::ToggleDesktopLock,
                                rl,
                                if snap.desktop_locked { Icon::Lock } else { Icon::Unlock },
                                "锁定歌词",
                                snap.desktop_locked,
                                false,
                            );
                        }

                        paint_row(
                            window,
                            cx,
                            TrayAction::ToggleWallpaper,
                            r_wall,
                            Icon::Fullscreen,
                            "桌面壁纸",
                            snap.wallpaper_open,
                            false,
                        );
                        paint_row(window, cx, TrayAction::OpenSettings, r_settings, Icon::Settings, "设置", false, false);
                        paint_row(window, cx, TrayAction::Quit, r_quit, Icon::Close, "退出", false, true);

                        for dy in [div1_y, div2_y] {
                            chrome::fill_round(
                                window,
                                card.x + 10.0,
                                dy,
                                (card.w - 20.0).max(20.0),
                                1.0,
                                0.5,
                                div_col,
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
