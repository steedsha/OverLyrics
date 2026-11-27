//! Native Windows file picker dialog for importing local lyric files.

#![allow(unsafe_code)]

use std::path::PathBuf;

/// Opens a native Windows file dialog restricted to `.lrc`, `.ttml`, `.qrc`, `.yrc`, `.txt`.
#[cfg(windows)]
pub fn pick_lyric_file() -> Option<PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    let filter: Vec<u16> = "歌词文件 (*.lrc;*.ttml;*.qrc;*.yrc;*.txt)\0*.lrc;*.ttml;*.qrc;*.yrc;*.txt\0\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "选择要导入的歌词文件\0".encode_utf16().collect();
    let mut file_buf = vec![0u16; 4096];

    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: windows::core::PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file_buf.as_mut_ptr()),
        nMaxFile: file_buf.len() as u32,
        lpstrTitle: windows::core::PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };

    let ok = unsafe { GetOpenFileNameW(&mut ofn).as_bool() };
    if !ok {
        return None;
    }
    let end = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
    let path = String::from_utf16_lossy(&file_buf[..end]);
    if path.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Opens a native Windows folder picker dialog (`IFileOpenDialog` + `FOS_PICKFOLDERS`).
#[cfg(windows)]
pub fn pick_lyrics_folder() -> Option<PathBuf> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog,
        IFileOpenDialog, SIGDN_FILESYSPATH,
    };

    unsafe {
        let init_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let need_uninit = init_hr.is_ok();
        let res = (|| -> Option<PathBuf> {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let opts = dialog.GetOptions().ok()?;
            dialog
                .SetOptions(
                    opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_NOCHANGEDIR,
                )
                .ok()?;
            let title: Vec<u16> = "选择本地歌词目录\0".encode_utf16().collect();
            let _ = dialog.SetTitle(windows::core::PCWSTR(title.as_ptr()));
            dialog.Show(None).ok()?;
            let item = dialog.GetResult().ok()?;
            let pwstr = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let text = pwstr.to_string().ok();
            CoTaskMemFree(Some(pwstr.0 as *const std::ffi::c_void));
            text.filter(|s| !s.trim().is_empty()).map(PathBuf::from)
        })();
        if need_uninit {
            CoUninitialize();
        }
        res
    }
}

#[cfg(not(windows))]
pub fn pick_lyric_file() -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn pick_lyrics_folder() -> Option<PathBuf> {
    None
}
