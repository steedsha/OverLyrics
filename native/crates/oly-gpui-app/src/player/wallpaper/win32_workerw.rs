//! Win32 `WorkerW` desktop attachment, `TaskbarCreated` auto-reattach, and
//! occlusion / lockscreen detection for temporary immersive wallpaper mode.
//!
//! Never modifies any Windows wallpaper registry key or `SPI_SETDESKWALLPAPER`.

#![allow(unsafe_code)]

use std::sync::Arc;

use super::SharedWallpaperBridge;

/// Finds the desktop `WorkerW` host window below desktop icons, supporting both:
/// - Windows 10 / Windows 11 23H2 (sibling `WorkerW` after `SHELLDLL_DefView`'s parent)
/// - Windows 11 24H2+ (child `WorkerW` inside `Progman` beneath `SHELLDLL_DefView`)
#[cfg(target_os = "windows")]
unsafe fn find_workerw() -> Option<(windows::Win32::Foundation::HWND, Option<windows::Win32::Foundation::HWND>)> {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, FindWindowExW, FindWindowW, SMTO_NORMAL, SendMessageTimeoutW,
    };
    use windows::core::{BOOL, w};

    let progman = unsafe { FindWindowW(w!("Progman"), None).ok()? };
    if progman.0.is_null() {
        return None;
    }
    // Ask Explorer to spawn/ensure the desktop WorkerW layer behind SHELLDLL_DefView.
    let mut _res: usize = 0;
    let _ = unsafe {
        SendMessageTimeoutW(
            progman,
            0x052C,
            WPARAM(0x0D),
            LPARAM(0x01),
            SMTO_NORMAL,
            1000,
            Some(&mut _res),
        )
    };
    let _ = unsafe {
        SendMessageTimeoutW(
            progman,
            0x052C,
            WPARAM(0),
            LPARAM(0),
            SMTO_NORMAL,
            1000,
            Some(&mut _res),
        )
    };

    // 1. Classic Win10 / Win11 23H2: EnumWindows to find top-level window hosting SHELLDLL_DefView,
    //    then take the next top-level WorkerW sibling.
    struct EnumState {
        workerw: HWND,
        defview: HWND,
    }
    unsafe extern "system" fn enum_proc(top: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut EnumState) };
        let def = unsafe { FindWindowExW(Some(top), None, w!("SHELLDLL_DefView"), None) };
        if let Ok(d) = def {
            if !d.0.is_null() {
                state.defview = d;
                if let Ok(w) = unsafe { FindWindowExW(None, Some(top), w!("WorkerW"), None) } {
                    if !w.0.is_null() {
                        state.workerw = w;
                    }
                }
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    let mut st = EnumState {
        workerw: HWND(std::ptr::null_mut()),
        defview: HWND(std::ptr::null_mut()),
    };
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(&mut st as *mut EnumState as isize)) };
    if !st.workerw.0.is_null() {
        return Some((st.workerw, None));
    }

    // 2. Win11 24H2+: SHELLDLL_DefView is directly under Progman, and WorkerW is a child of Progman.
    if let Ok(child_w) = unsafe { FindWindowExW(Some(progman), None, w!("WorkerW"), None) } {
        if !child_w.0.is_null() {
            return Some((child_w, None));
        }
    }
    if !st.defview.0.is_null() {
        return Some((progman, Some(st.defview)));
    }
    Some((progman, None))
}

/// Attaches `hwnd` under the desktop `WorkerW` layer at physical screen rect `(x, y, w, h)`.
pub fn attach_to_workerw(hwnd: isize, x_phys: i32, y_phys: i32, w_phys: i32, h_phys: i32) -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::Foundation::{HWND, RECT};
        use windows::Win32::UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GWL_STYLE, GetSystemMetrics, GetWindowLongW, GetWindowRect, HWND_BOTTOM,
            SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_SHOWWINDOW,
            SetParent, SetWindowLongW, SetWindowPos, WS_CAPTION, WS_CHILD, WS_EX_APPWINDOW,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_THICKFRAME, WS_VISIBLE,
        };

        if hwnd == 0 {
            return false;
        }
        let Some((parent, defview_above)) = find_workerw() else {
            return false;
        };
        let h = HWND(hwnd as *mut core::ffi::c_void);

        let style = GetWindowLongW(h, GWL_STYLE);
        let new_style = (style
            & !((WS_POPUP.0 | WS_CAPTION.0 | WS_THICKFRAME.0) as i32))
            | (WS_CHILD.0 | WS_VISIBLE.0) as i32;
        let _ = SetWindowLongW(h, GWL_STYLE, new_style);

        let ex = GetWindowLongW(h, GWL_EXSTYLE);
        let new_ex = (ex | (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) as i32)
            & !(WS_EX_APPWINDOW.0 as i32);
        let _ = SetWindowLongW(h, GWL_EXSTYLE, new_ex);

        let _ = SetParent(h, Some(parent));

        let mut parent_rect = RECT::default();
        let (origin_x, origin_y) = if GetWindowRect(parent, &mut parent_rect).is_ok() {
            (parent_rect.left, parent_rect.top)
        } else {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
            )
        };
        let rel_x = x_phys - origin_x;
        let rel_y = y_phys - origin_y;

        let insert_after = defview_above.unwrap_or(HWND_BOTTOM);
        let _ = SetWindowPos(
            h,
            Some(insert_after),
            rel_x,
            rel_y,
            w_phys.max(1),
            h_phys.max(1),
            SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
        );
        true
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (hwnd, x_phys, y_phys, w_phys, h_phys);
        false
    }
}

/// Detaches `hwnd` from `WorkerW`, hides it, and forces Explorer to repaint the original wallpaper.
pub fn detach_from_workerw_and_refresh(hwnd: isize) {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::{
            RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW, RedrawWindow,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, SW_HIDE, SetParent, ShowWindow,
        };
        use windows::core::w;

        if hwnd != 0 {
            let h = HWND(hwnd as *mut core::ffi::c_void);
            let _ = ShowWindow(h, SW_HIDE);
            let _ = SetParent(h, None);
        }
        if let Some((workerw, _)) = find_workerw() {
            let _ = RedrawWindow(
                Some(workerw),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_FRAME | RDW_UPDATENOW,
            );
        }
        if let Ok(progman) = FindWindowW(w!("Progman"), None) {
            if !progman.0.is_null() {
                let _ = RedrawWindow(
                    Some(progman),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
                );
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = hwnd;
}

/// Returns `true` when the desktop is locked / on a secure desktop, OR when a visible
/// maximized / fullscreen application window completely occludes `target_rect_phys`
/// (`(x, y, w, h)` in physical screen coordinates).
pub fn is_desktop_occluded_or_locked(
    target_rect_phys: (i32, i32, i32, i32),
    own_hwnds: &[isize],
) -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::System::StationsAndDesktops::{
            CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, OpenInputDesktop,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetClassNameW, GetForegroundWindow, GetWindowRect, IsIconic, IsWindowVisible,
            IsZoomed,
        };

        // 1. Lockscreen / UAC / screensaver check: OpenInputDesktop fails when input desktop is secure.
        const DESKTOP_READOBJECTS: u32 = 0x0001;
        match OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(DESKTOP_READOBJECTS),
        ) {
            Ok(desk) => {
                let _ = CloseDesktop(desk);
            }
            Err(_) => return true,
        }

        // 2. Check foreground window occlusion of this target monitor.
        let fg = GetForegroundWindow();
        if fg.0.is_null() || !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
            return false;
        }
        let fg_raw = fg.0 as isize;
        if own_hwnds.contains(&fg_raw) {
            return false;
        }
        let mut cls_buf = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls_buf) as usize;
        let cls = String::from_utf16_lossy(&cls_buf[..n]);
        if matches!(
            cls.as_str(),
            "Progman"
                | "WorkerW"
                | "Shell_TrayWnd"
                | "Shell_SecondaryTrayWnd"
                | "NotifyIconOverflowWindow"
                | "Windows.UI.Core.CoreWindow"
        ) {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let (tx, ty, tw, th) = target_rect_phys;
        if tw <= 0 || th <= 0 {
            return false;
        }
        let ix = (r.right.min(tx + tw) - r.left.max(tx)).max(0);
        let iy = (r.bottom.min(ty + th) - r.top.max(ty)).max(0);
        let cov = (ix as f64 * iy as f64) / (tw as f64 * th as f64);
        if IsZoomed(fg).as_bool() && cov >= 0.85 {
            return true;
        }
        cov >= 0.96
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (target_rect_phys, own_hwnds);
        false
    }
}

/// Lightweight hidden Win32 message window that listens for Explorer's `TaskbarCreated`
/// broadcast and bumps `bridge.taskbar_recreated_gen` so wallpaper windows auto-reattach.
pub struct TaskbarCreatedListener {
    #[cfg(target_os = "windows")]
    hwnd: isize,
}

#[cfg(target_os = "windows")]
struct ListenerState {
    bridge: Arc<SharedWallpaperBridge>,
    taskbar_msg: u32,
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn listener_wnd_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, PostQuitMessage, WM_CLOSE,
        WM_DESTROY,
    };

    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const ListenerState;
    if !ptr.is_null() {
        let st = unsafe { &*ptr };
        if st.taskbar_msg != 0 && msg == st.taskbar_msg {
            st.bridge.notify_taskbar_created();
        }
    }
    match msg {
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl TaskbarCreatedListener {
    pub fn start(bridge: Arc<SharedWallpaperBridge>) -> Self {
        #[cfg(target_os = "windows")]
        {
            use std::sync::mpsc;
            let (tx, rx) = mpsc::channel::<isize>();
            let _ = std::thread::Builder::new()
                .name("wallpaper-taskbar-watch".into())
                .spawn(move || unsafe {
                    use windows::Win32::Foundation::{HINSTANCE, HWND};
                    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
                    use windows::Win32::UI::WindowsAndMessaging::{
                        CreateWindowExW, DispatchMessageW, GWLP_USERDATA, GetMessageW, MSG,
                        RegisterClassW, RegisterWindowMessageW, SetWindowLongPtrW,
                        TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
                    };
                    use windows::core::w;

                    let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
                    let cls_name = w!("OverLyricsWallpaperWatch");
                    let wc = WNDCLASSW {
                        lpfnWndProc: Some(listener_wnd_proc),
                        hInstance: hinst,
                        lpszClassName: cls_name,
                        ..Default::default()
                    };
                    let _ = RegisterClassW(&wc);
                    let hwnd = CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        cls_name,
                        w!(""),
                        WINDOW_STYLE(0),
                        0,
                        0,
                        0,
                        0,
                        None,
                        None,
                        Some(hinst),
                        None,
                    )
                    .unwrap_or(HWND(std::ptr::null_mut()));
                    if hwnd.0.is_null() {
                        let _ = tx.send(0);
                        return;
                    }
                    let taskbar_msg = RegisterWindowMessageW(w!("TaskbarCreated"));
                    let state = Box::new(ListenerState {
                        bridge,
                        taskbar_msg,
                    });
                    let state_ptr = Box::into_raw(state);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                    let _ = tx.send(hwnd.0 as isize);

                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(state_ptr));
                });
            let hwnd = rx.recv_timeout(std::time::Duration::from_millis(500)).unwrap_or(0);
            Self { hwnd }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = bridge;
            Self {}
        }
    }
}

impl Drop for TaskbarCreatedListener {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
            if self.hwnd != 0 {
                let _ = PostMessageW(
                    Some(HWND(self.hwnd as *mut core::ffi::c_void)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}
