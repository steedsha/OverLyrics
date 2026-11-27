//! Win32 native window helpers (always-on-top, overlay styling, click-through, visibility, global hotkey).

use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[cfg(target_os = "windows")]
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
        DwmSetWindowAttribute,
    },
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_CONTROL, MOD_NOREPEAT,
            RegisterHotKey, UnregisterHotKey, VK_LBUTTON,
        },
        WindowsAndMessaging::{
            GWL_EXSTYLE, GetCursorPos, GetMessageW, GetWindowLongW, GetWindowRect, HWND_NOTOPMOST,
            HWND_TOPMOST, MSG, PostThreadMessageW, SW_HIDE, SW_RESTORE, SW_SHOW, SWP_FRAMECHANGED,
            SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
            SetWindowLongW, SetWindowPos, ShowWindow, WM_HOTKEY, WM_QUIT, WS_EX_APPWINDOW,
            WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
        },
    },
};

pub fn hwnd_from_window(window: &gpui_kit::Window) -> Option<isize> {
    #[cfg(target_os = "windows")]
    {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        None
    }
}

pub fn set_always_on_top(hwnd: isize, on_top: bool) {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 {
            return;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let insert_after = if on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
        let _ = SetWindowPos(
            h,
            Some(insert_after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (hwnd, on_top);
}

pub fn set_window_visible(hwnd: isize, visible: bool) {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 {
            return;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        if visible {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
        } else {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (hwnd, visible);
}

/// Disables Windows 11 DWM default 8px rounded corners and border stroke so custom-rounded transparent windows have crisp corners.
pub fn disable_dwm_corners_and_border(hwnd: isize) {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 {
            return;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            h,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&corner as *const _) as *const core::ffi::c_void,
            std::mem::size_of_val(&corner) as u32,
        );
        let border = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            h,
            DWMWA_BORDER_COLOR,
            (&border as *const _) as *const core::ffi::c_void,
            std::mem::size_of_val(&border) as u32,
        );
        let _ = SetWindowPos(
            h,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
    #[cfg(not(target_os = "windows"))]
    let _ = hwnd;
}

/// Configures a GPUI transparent window as a borderless, taskbar-hidden, topmost desktop lyrics overlay.
pub fn configure_desktop_overlay(hwnd: isize) {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 {
            return;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let ex = GetWindowLongW(h, GWL_EXSTYLE);
        let next = (ex | WS_EX_TOOLWINDOW.0 as i32) & !(WS_EX_APPWINDOW.0 as i32);
        let _ = SetWindowLongW(h, GWL_EXSTYLE, next);

        // Disable Windows 11 DWM rounded corner outline / border shadow on the invisible overlay rect.
        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            h,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&corner as *const _) as *const core::ffi::c_void,
            std::mem::size_of_val(&corner) as u32,
        );
        let border = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            h,
            DWMWA_BORDER_COLOR,
            (&border as *const _) as *const core::ffi::c_void,
            std::mem::size_of_val(&border) as u32,
        );

        let _ = SetWindowPos(
            h,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    #[cfg(not(target_os = "windows"))]
    let _ = hwnd;
}

/// Enables or disables Win32 cross-process mouse click-through (`WS_EX_LAYERED | WS_EX_TRANSPARENT`).
pub fn set_click_through(hwnd: isize, click_through: bool) {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 {
            return;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let ex = GetWindowLongW(h, GWL_EXSTYLE);
        let flags = (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as i32;
        let next = if click_through {
            ex | flags
        } else {
            ex & !flags
        };
        if next != ex {
            let _ = SetWindowLongW(h, GWL_EXSTYLE, next);
            let _ = SetWindowPos(
                h,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (hwnd, click_through);
}

/// Returns current OS cursor position in physical screen pixels.
pub fn cursor_screen_pos() -> Option<(i32, i32)> {
    #[cfg(target_os = "windows")]
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_ok() {
            return Some((pt.x, pt.y));
        }
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Returns true while the primary left mouse button is physically pressed.
pub fn is_left_mouse_down() -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Moves `hwnd` to physical screen coordinates `(x, y)` without changing size or Z-order.
pub fn move_window_physical(hwnd: isize, x: i32, y: i32) {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 {
            return;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let _ = SetWindowPos(
            h,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (hwnd, x, y);
}

/// Moves and resizes `hwnd` in physical screen coordinates without changing Z-order or activation.
pub fn set_window_rect_physical(hwnd: isize, x: i32, y: i32, w: i32, h: i32) {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 || w <= 0 || h <= 0 {
            return;
        }
        let handle = HWND(hwnd as *mut core::ffi::c_void);
        let _ = SetWindowPos(
            handle,
            None,
            x,
            y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (hwnd, x, y, w, h);
}

/// Returns `hwnd`'s current screen rectangle `(left, top, width, height)` in physical pixels.
pub fn window_rect_physical(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 {
            return None;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let mut rc = RECT::default();
        if GetWindowRect(h, &mut rc).is_ok() {
            Some((rc.left, rc.top, (rc.right - rc.left).max(1), (rc.bottom - rc.top).max(1)))
        } else {
            None
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = hwnd;
        None
    }
}

/// Returns true if the OS cursor is currently inside `hwnd`'s screen rectangle.
pub fn cursor_in_window(hwnd: isize) -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        if hwnd == 0 {
            return false;
        }
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let mut pt = POINT::default();
        let mut rc = RECT::default();
        if GetCursorPos(&mut pt).is_ok() && GetWindowRect(h, &mut rc).is_ok() {
            return pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom;
        }
        false
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = hwnd;
        false
    }
}

/// Background Win32 message loop listening for `Ctrl+L` to toggle desktop lyrics lock.
pub struct LockHotkeyListener {
    pub counter: Arc<AtomicU64>,
    thread_id: Arc<AtomicU32>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl LockHotkeyListener {
    pub fn start() -> Self {
        let counter = Arc::new(AtomicU64::new(0));
        let thread_id = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let tid = thread_id.clone();
        let join = std::thread::Builder::new()
            .name("desktop-lyrics-hotkey".into())
            .spawn(move || {
                #[cfg(target_os = "windows")]
                unsafe {
                    tid.store(GetCurrentThreadId(), Ordering::SeqCst);
                    const HOTKEY_ID: i32 = 0x4F4C; // "OL"
                    const VK_L: u32 = 0x4C;
                    let mods: HOT_KEY_MODIFIERS = MOD_CONTROL | MOD_NOREPEAT;
                    let registered = RegisterHotKey(None, HOTKEY_ID, mods, VK_L).is_ok();
                    if !registered {
                        log::warn!("[desktop-lyrics] RegisterHotKey Ctrl+Alt+L failed (may be in use)");
                        return;
                    }
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                            c.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    let _ = UnregisterHotKey(None, HOTKEY_ID);
                }
                #[cfg(not(target_os = "windows"))]
                let _ = (c, tid);
            })
            .ok();
        Self {
            counter,
            thread_id,
            join,
        }
    }
}

impl Drop for LockHotkeyListener {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        unsafe {
            let tid = self.thread_id.load(Ordering::SeqCst);
            if tid != 0 {
                let _ = PostThreadMessageW(tid, WM_QUIT, Default::default(), Default::default());
            }
        }
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}
