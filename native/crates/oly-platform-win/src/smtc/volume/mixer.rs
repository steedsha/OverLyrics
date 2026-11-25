//! Windows per-application mixer volume (`ISimpleAudioVolume`).
//!
//! This is the channel that works for anything producing audio, but it can only
//! match an exe name or a packaged-app AUMID — never the SMTC display name. It
//! deliberately never touches the endpoint (master) volume: a lyric overlay that
//! changes the system volume when the user drags one player's slider is a bug
//! report, not a feature.

#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod imp {
    use windows::core::Interface;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::{
        eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceCollection,
        IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// `AudioSessionStateExpired`. An expired session still enumerates, and
    /// writing to it is a no-op that silently loses the drag.
    const STATE_EXPIRED: i32 = 2;

    pub fn read(app: &str) -> Option<f32> {
        visit(app, None)
    }

    pub fn write(app: &str, value: f32) -> Option<f32> {
        visit(app, Some(value.clamp(0.0, 1.0)))
    }

    fn visit(app: &str, value: Option<f32>) -> Option<f32> {
        if app.trim().is_empty() {
            return None;
        }
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let devices = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .ok()?;
            let count = devices.GetCount().ok()?;
            let mut best_state = -1i32;
            let mut result: Option<f32> = None;
            for i in 0..count {
                if let Some(level) = visit_device(&devices, i, app, value, &mut best_state) {
                    result = Some(level);
                }
            }
            result
        }
    }

    /// Walks one endpoint. A device can disappear mid-enumeration, so a failure
    /// is skipped rather than propagated.
    unsafe fn visit_device(
        devices: &IMMDeviceCollection,
        index: u32,
        app: &str,
        value: Option<f32>,
        best_state: &mut i32,
    ) -> Option<f32> {
        let device = devices.Item(index).ok()?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None).ok()?;
        let sessions = manager.GetSessionEnumerator().ok()?;
        let count = sessions.GetCount().ok()?;
        let mut result = None;
        for i in 0..count {
            let Ok(control) = sessions.GetSession(i) else {
                continue;
            };
            let Ok(session) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            let Ok(state) = session.GetState() else {
                continue;
            };
            if state.0 == STATE_EXPIRED {
                continue;
            }
            let Ok(pid) = session.GetProcessId() else {
                continue;
            };
            if !matches_app(pid, app) {
                continue;
            }
            let Ok(volume) = session.cast::<ISimpleAudioVolume>() else {
                continue;
            };
            if let Some(level) = value {
                let _ = volume.SetMasterVolume(level, std::ptr::null());
                // A nonzero drag should also lift a session the user muted in
                // the mixer; otherwise the slider moves and nothing is heard.
                if level > 0.0 {
                    let _ = volume.SetMute(false, std::ptr::null());
                }
            }
            let Ok(level) = volume.GetMasterVolume() else {
                continue;
            };
            let muted = volume.GetMute().map(|b| b.as_bool()).unwrap_or(false);
            // Several sessions can match (a browser spawns one per tab). The
            // most active one is the stream the user is hearing.
            if state.0 > *best_state {
                *best_state = state.0;
                result = Some(if muted { 0.0 } else { level });
            }
        }
        result
    }

    fn matches_app(pid: u32, app: &str) -> bool {
        if pid == 0 {
            return false;
        }
        let app = app.trim();
        if app.is_empty() {
            return false;
        }
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let matched = process_name(handle).is_some_and(|name| same_app(&name, app))
                || aumid(handle).is_some_and(|id| id.eq_ignore_ascii_case(app));
            let _ = CloseHandle(handle);
            matched
        }
    }

    /// SMTC names a browser session `MSEdge` while the process is `msedge.exe`,
    /// so a literal comparison would silently lose every volume drag for the
    /// most common case there is — a song playing in a browser tab. The `.exe`
    /// suffix is the only difference allowed.
    fn same_app(process_name: &str, app: &str) -> bool {
        fn stem(name: &str) -> &str {
            name.strip_suffix(".exe")
                .or_else(|| name.strip_suffix(".EXE"))
                .unwrap_or(name)
        }
        if process_name.eq_ignore_ascii_case(app)
            || stem(process_name).eq_ignore_ascii_case(stem(app))
        {
            return true;
        }
        let low_app = app.to_ascii_lowercase();
        let proc_stem = stem(process_name).to_ascii_lowercase();
        (low_app.starts_with("appleinc.applemusicwin_") && proc_stem == "applemusic")
            || (low_app.starts_with("spotifyab.spotifymusic_") && proc_stem == "spotify")
    }

    unsafe fn process_name(handle: windows::Win32::Foundation::HANDLE) -> Option<String> {
        let mut buf = [0u16; 512];
        let mut size = buf.len() as u32;
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .ok()?;
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        full.rsplit(['\\', '/']).next().map(str::to_string)
    }

    /// Packaged players (the Store builds of Spotify/Apple Music) expose an
    /// AUMID in SMTC and no useful exe name of their own.
    unsafe fn aumid(handle: windows::Win32::Foundation::HANDLE) -> Option<String> {
        let mut len = 0u32;
        // 122 = ERROR_INSUFFICIENT_BUFFER: the expected first answer.
        if GetApplicationUserModelId(handle, &mut len, None).0 != 122 || len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        if GetApplicationUserModelId(handle, &mut len, Some(windows::core::PWSTR(buf.as_mut_ptr())))
            .0
            != 0
        {
            return None;
        }
        Some(String::from_utf16_lossy(
            &buf[..(len as usize).saturating_sub(1)],
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::same_app;

        #[test]
        fn browser_aumids_match_their_process_name() {
            assert!(same_app("msedge.exe", "MSEdge"));
            assert!(same_app("MSEdge", "msedge.exe"));
            assert!(same_app("QQMusic.exe", "QQMusic.exe"));
            assert!(same_app("Spotify.exe", "spotify"));
        }

        #[test]
        fn distinct_apps_stay_distinct() {
            assert!(!same_app("msedge.exe", "chrome"));
            assert!(!same_app("QQMusic.exe", "KuGou.exe"));
            assert!(!same_app("spotify.exe", ""));
            // Only the suffix may be ignored; an inner ".exe" is part of the name.
            assert!(!same_app("a.exe.exe", "a"));
        }
    }
}

#[cfg(windows)]
pub use imp::{read, write};

#[cfg(not(windows))]
pub fn read(_app: &str) -> Option<f32> {
    None
}

#[cfg(not(windows))]
pub fn write(_app: &str, _value: f32) -> Option<f32> {
    None
}
