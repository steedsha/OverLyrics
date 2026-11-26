//! Version check and single-executable self-updater.
//!
//! Update source order:
//! 1. Official website manifest (`https://overlyrics.steedsha.cn/latest.json`)
//! 2. GitHub Releases API fallback (`steedsha/OverLyrics`)
//!
//! When an update is installed, the new executable is downloaded to
//! `<exe_dir>/overlyrics.exe.new`, verified (Windows PE `MZ` header + optional
//! SHA-256), and swapped in by a hidden helper script once the current process
//! exits. The previous binary is backed up as `.old` and cleaned up on the next
//! launch. If the install directory is not writable or no portable `.exe` asset
//! is available, the user is guided to open the official website instead.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

pub const WEBSITE_URL: &str = "https://overlyrics.steedsha.cn";
pub const WEBSITE_MANIFEST_URL: &str = "https://overlyrics.steedsha.cn/latest.json";
pub const GITHUB_LATEST_API: &str =
    "https://api.github.com/repos/steedsha/OverLyrics/releases/latest";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub notes: String,
    pub download_url: Option<String>,
    pub sha256: Option<String>,
    pub release_url: String,
    pub source: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Downloading {
        version: String,
    },
    ReadyToRestart {
        version: String,
    },
    Error(String),
}

pub struct UpdateManager {
    state: Mutex<UpdateState>,
    notice: Mutex<Option<String>>,
    busy: AtomicBool,
    restart_ready: AtomicBool,
    silent_checked: AtomicBool,
}

impl UpdateManager {
    fn new() -> Self {
        Self {
            state: Mutex::new(UpdateState::Idle),
            notice: Mutex::new(None),
            busy: AtomicBool::new(false),
            restart_ready: AtomicBool::new(false),
            silent_checked: AtomicBool::new(false),
        }
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn take_notice(&self) -> Option<String> {
        self.notice.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn take_restart_ready(&self) -> bool {
        self.restart_ready.swap(false, Ordering::SeqCst)
    }

    pub fn version_badge(&self, base_version: &str) -> String {
        match self.state() {
            UpdateState::Available(info) => {
                format!("{base_version} · 新版本 v{}", normalize_version(&info.version))
            }
            UpdateState::Downloading { version } => {
                format!("{base_version} · 下载 v{}…", normalize_version(&version))
            }
            UpdateState::ReadyToRestart { version } => {
                format!("{base_version} · 即将重启至 v{}", normalize_version(&version))
            }
            _ => base_version.to_string(),
        }
    }

    pub fn button_label(&self) -> String {
        match self.state() {
            UpdateState::Idle | UpdateState::UpToDate => "最新".into(),
            UpdateState::Checking
            | UpdateState::Downloading { .. }
            | UpdateState::ReadyToRestart { .. } => "…".into(),
            UpdateState::Available(_) => "更新".into(),
            UpdateState::Error(_) => "重试".into(),
        }
    }

    pub fn is_update_available(&self) -> bool {
        matches!(self.state(), UpdateState::Available(_))
    }

    /// Triggers a single background silent update check per process lifetime.
    pub fn start_silent_check_once(self: &Arc<Self>) {
        if self.silent_checked.swap(true, Ordering::SeqCst) {
            return;
        }
        self.start_check(true);
    }

    pub fn start_check(self: &Arc<Self>, silent: bool) -> bool {
        if self.busy.swap(true, Ordering::SeqCst) {
            return false;
        }
        if !silent {
            *self.state.lock().unwrap_or_else(|e| e.into_inner()) = UpdateState::Checking;
        }
        let mgr = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("oly-update-check".into())
            .spawn(move || {
                let res = check_latest_sync();
                match res {
                    Ok(Some(info)) => {
                        let ver = normalize_version(&info.version).to_string();
                        *mgr.state.lock().unwrap_or_else(|e| e.into_inner()) =
                            UpdateState::Available(info);
                        if !silent {
                            *mgr.notice.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(format!("发现新版本 v{ver}，点击按钮即可更新或前往官网下载"));
                        }
                    }
                    Ok(None) => {
                        *mgr.state.lock().unwrap_or_else(|e| e.into_inner()) =
                            UpdateState::UpToDate;
                        if !silent {
                            *mgr.notice.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(format!("当前 v{CURRENT_VERSION} 已是最新版本"));
                        }
                    }
                    Err(err) => {
                        log::warn!("[updater] check failed: {err}");
                        if !silent {
                            *mgr.state.lock().unwrap_or_else(|e| e.into_inner()) =
                                UpdateState::Error(err);
                            *mgr.notice.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some("检查更新失败，可点击「官网」查看最新版本".into());
                        }
                    }
                }
                mgr.busy.store(false, Ordering::SeqCst);
            });
        true
    }

    pub fn start_install(self: &Arc<Self>, info: ReleaseInfo) -> bool {
        if self.busy.swap(true, Ordering::SeqCst) {
            return false;
        }
        let ver = normalize_version(&info.version).to_string();
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = UpdateState::Downloading {
            version: ver.clone(),
        };
        let mgr = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("oly-update-install".into())
            .spawn(move || {
                match download_and_apply_sync(&info) {
                    Ok(()) => {
                        *mgr.state.lock().unwrap_or_else(|e| e.into_inner()) =
                            UpdateState::ReadyToRestart { version: ver };
                        *mgr.notice.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some("新版本已就绪，正在重启更新…".into());
                        mgr.restart_ready.store(true, Ordering::SeqCst);
                    }
                    Err(err) => {
                        log::warn!("[updater] install failed: {err}");
                        *mgr.state.lock().unwrap_or_else(|e| e.into_inner()) =
                            UpdateState::Available(info);
                        *mgr.notice.lock().unwrap_or_else(|e| e.into_inner()) = Some(err);
                    }
                }
                mgr.busy.store(false, Ordering::SeqCst);
            });
        true
    }
}

static GLOBAL_UPDATER: OnceLock<Arc<UpdateManager>> = OnceLock::new();

pub fn global() -> &'static Arc<UpdateManager> {
    GLOBAL_UPDATER.get_or_init(|| Arc::new(UpdateManager::new()))
}

/// Removes any `.old` backup left over from a previous self-update.
pub fn cleanup_old_binary() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let old = PathBuf::from(format!("{}.old", exe.display()));
    if old.is_file() {
        let _ = std::fs::remove_file(&old);
    }
    let staged = PathBuf::from(format!("{}.new", exe.display()));
    if staged.is_file() {
        let _ = std::fs::remove_file(&staged);
    }
}

pub fn normalize_version(raw: &str) -> &str {
    raw.trim().trim_start_matches(['v', 'V'])
}

/// Parses `MAJOR.MINOR.PATCH` (ignoring leading `v` and any `-prerelease`/`+build`).
pub fn parse_semver(raw: &str) -> Option<(u64, u64, u64)> {
    let core = normalize_version(raw)
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .trim();
    if core.is_empty() {
        return None;
    }
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    Some((major, minor, patch))
}

pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse_semver(remote), parse_semver(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

pub fn parse_website_manifest(body: &str) -> Option<ReleaseInfo> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let version = v.get("version")?.as_str()?.trim().to_string();
    parse_semver(&version)?;
    let notes = v
        .get("notes")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let download_url = v
        .get("download_url")
        .or_else(|| v.get("downloadUrl"))
        .or_else(|| v.get("url"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
        .map(str::to_string);
    let sha256 = v
        .get("sha256")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_string);
    let release_url = v
        .get("release_url")
        .or_else(|| v.get("releaseUrl"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
        .unwrap_or(WEBSITE_URL)
        .to_string();
    Some(ReleaseInfo {
        version,
        notes,
        download_url,
        sha256,
        release_url,
        source: "website",
    })
}

pub fn parse_github_release(body: &str) -> Option<ReleaseInfo> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if v.get("draft").and_then(|x| x.as_bool()).unwrap_or(false) {
        return None;
    }
    let version = v.get("tag_name")?.as_str()?.trim().to_string();
    parse_semver(&version)?;
    let notes = v
        .get("body")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let release_url = v
        .get("html_url")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| s.starts_with("https://"))
        .unwrap_or(WEBSITE_URL)
        .to_string();
    let mut best_exe: Option<(i32, String)> = None;
    if let Some(assets) = v.get("assets").and_then(|x| x.as_array()) {
        for asset in assets {
            let Some(name) = asset.get("name").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(url) = asset
                .get("browser_download_url")
                .and_then(|x| x.as_str())
                .filter(|s| s.starts_with("https://"))
            else {
                continue;
            };
            let low = name.to_ascii_lowercase();
            if !low.ends_with(".exe") {
                continue;
            }
            let score = if low == "overlyrics.exe" {
                100
            } else if low.contains("overlyrics") && !low.contains("setup") && !low.contains("installer") {
                80
            } else if !low.contains("setup") && !low.contains("installer") {
                40
            } else {
                10
            };
            if best_exe.as_ref().is_none_or(|(s, _)| score > *s) {
                best_exe = Some((score, url.to_string()));
            }
        }
    }
    Some(ReleaseInfo {
        version,
        notes,
        download_url: best_exe.map(|(_, u)| u),
        sha256: None,
        release_url,
        source: "github",
    })
}

pub fn check_latest_sync() -> Result<Option<ReleaseInfo>, String> {
    let website_err = match oly_platform_win::http::get_text(
        WEBSITE_MANIFEST_URL,
        &[("Accept", "application/json")],
        6_000,
    ) {
        Ok(body) => {
            if let Some(info) = parse_website_manifest(&body) {
                return Ok(is_newer(&info.version, CURRENT_VERSION).then_some(info));
            }
            "invalid website manifest".to_string()
        }
        Err(e) => e,
    };
    log::info!("[updater] website check unavailable ({website_err}), falling back to GitHub");
    let gh_body = oly_platform_win::http::get_text(
        GITHUB_LATEST_API,
        &[
            ("Accept", "application/vnd.github+json"),
            ("User-Agent", concat!("OverLyrics/", env!("CARGO_PKG_VERSION"))),
        ],
        8_000,
    )
    .map_err(|e| format!("官网({website_err}) / GitHub({e})"))?;
    let info = parse_github_release(&gh_body).ok_or_else(|| "GitHub 发布信息解析失败".to_string())?;
    Ok(is_newer(&info.version, CURRENT_VERSION).then_some(info))
}

fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".oly-write-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub fn download_and_apply_sync(info: &ReleaseInfo) -> Result<(), String> {
    let Some(download_url) = info.download_url.as_deref() else {
        return Err("当前版本未提供自动更新包，请点击「官网」手动下载".into());
    };
    let current_exe =
        std::env::current_exe().map_err(|_| "无法定位当前程序路径，请前往官网下载".to_string())?;
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| "无法定位程序目录，请前往官网下载".to_string())?;
    if !dir_is_writable(exe_dir) {
        return Err("当前目录无写入权限，请点击「官网」手动下载更新".into());
    }

    let staged = PathBuf::from(format!("{}.new", current_exe.display()));
    oly_platform_win::http::download_exe_verified(
        download_url,
        &staged,
        info.sha256.as_deref(),
        120_000,
    )
    .map_err(|e| format!("下载失败({e})，可点击「官网」手动下载"))?;

    #[cfg(windows)]
    {
        spawn_windows_replacer(&current_exe, &staged)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = staged;
        Err("当前平台暂不支持自动替换，请前往官网下载".into())
    }
}

#[cfg(windows)]
fn spawn_windows_replacer(current_exe: &Path, staged_exe: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let pid = std::process::id();
    let backup_exe = PathBuf::from(format!("{}.old", current_exe.display()));
    let script_path = std::env::temp_dir().join(format!("overlyrics-update-{pid}.cmd"));

    let cur_s = current_exe.display().to_string();
    let new_s = staged_exe.display().to_string();
    let old_s = backup_exe.display().to_string();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let relaunch_args = args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{}\"", a.replace('"', ""))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let script = format!(
        "@echo off\r\n\
         setlocal\r\n\
         set \"CUR={cur_s}\"\r\n\
         set \"NEW={new_s}\"\r\n\
         set \"OLD={old_s}\"\r\n\
         for /L %%i in (1,1,60) do (\r\n\
           tasklist /FI \"PID eq {pid}\" 2>NUL | find /I \"{pid}\" >NUL\r\n\
           if errorlevel 1 goto replace\r\n\
           ping -n 2 127.0.0.1 >NUL\r\n\
         )\r\n\
         :replace\r\n\
         if exist \"%OLD%\" del /F /Q \"%OLD%\" >NUL 2>&1\r\n\
         move /Y \"%CUR%\" \"%OLD%\" >NUL 2>&1\r\n\
         if errorlevel 1 goto cleanup\r\n\
         move /Y \"%NEW%\" \"%CUR%\" >NUL 2>&1\r\n\
         if errorlevel 1 (\r\n\
           move /Y \"%OLD%\" \"%CUR%\" >NUL 2>&1\r\n\
           goto cleanup\r\n\
         )\r\n\
         start \"\" \"%CUR%\" {relaunch_args}\r\n\
         :cleanup\r\n\
         del /F /Q \"%~f0\" >NUL 2>&1\r\n"
    );

    std::fs::write(&script_path, script.as_bytes())
        .map_err(|e| format!("无法生成更新脚本({e})，请前往官网下载"))?;

    std::process::Command::new("cmd")
        .args(["/C", &script_path.display().to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("无法启动更新器({e})，请前往官网下载"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison_handles_v_prefix_and_prerelease() {
        assert!(is_newer("1.2.0", "1.1.0"));
        assert!(is_newer("v2.0.0", "1.1.0"));
        assert!(is_newer("v1.1.1", "1.1.0"));
        assert!(!is_newer("1.1.0", "1.1.0"));
        assert!(!is_newer("1.0.9", "1.1.0"));
        assert!(is_newer("1.2.0-beta.1", "1.1.0"));
    }

    #[test]
    fn parses_website_manifest_fields() {
        let json = r#"{
            "version": "1.2.0",
            "notes": "Fix volume and desktop lyrics",
            "download_url": "https://overlyrics.steedsha.cn/releases/overlyrics.exe",
            "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "release_url": "https://overlyrics.steedsha.cn"
        }"#;
        let info = parse_website_manifest(json).expect("manifest should parse");
        assert_eq!(info.version, "1.2.0");
        assert_eq!(info.source, "website");
        assert_eq!(
            info.download_url.as_deref(),
            Some("https://overlyrics.steedsha.cn/releases/overlyrics.exe")
        );
        assert!(info.sha256.is_some());
    }

    #[test]
    fn parses_github_release_and_prefers_portable_exe() {
        let json = r#"{
            "tag_name": "v1.2.3",
            "body": "Release notes",
            "html_url": "https://github.com/steedsha/OverLyrics/releases/tag/v1.2.3",
            "assets": [
                {
                    "name": "OverLyrics-Setup-1.2.3.exe",
                    "browser_download_url": "https://github.com/steedsha/OverLyrics/releases/download/v1.2.3/OverLyrics-Setup-1.2.3.exe"
                },
                {
                    "name": "overlyrics.exe",
                    "browser_download_url": "https://github.com/steedsha/OverLyrics/releases/download/v1.2.3/overlyrics.exe"
                }
            ]
        }"#;
        let info = parse_github_release(json).expect("github release should parse");
        assert_eq!(info.version, "v1.2.3");
        assert_eq!(info.source, "github");
        assert_eq!(
            info.download_url.as_deref(),
            Some("https://github.com/steedsha/OverLyrics/releases/download/v1.2.3/overlyrics.exe")
        );
    }
}
