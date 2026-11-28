//! Temporary immersive desktop wallpaper mode.
//!
//! Directly attaches the main `PlayerView` window under desktop icons via
//! Windows `WorkerW` (hiding window chrome & bottom controls while keeping the
//! existing GPU background, cover/motion art, track meta, and AMLL lyrics),
//! paired with a minimalist draggable 5-button floating glass control bar.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

pub mod control_bar;
pub mod win32_workerw;

#[derive(Clone, Debug)]
pub enum WallpaperCommand {
    OpenSettings,
    SwitchToDesktopLyrics,
    ToggleShowCover,
    ToggleMotionCover,
    SaveBarPos(f32, f32),
    ExitWallpaper,
}

pub struct SharedWallpaperBridge {
    commands: Mutex<Vec<WallpaperCommand>>,
    pub taskbar_recreated_gen: AtomicU64,
    own_hwnds: Mutex<Vec<isize>>,
}

impl Default for SharedWallpaperBridge {
    fn default() -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
            taskbar_recreated_gen: AtomicU64::new(0),
            own_hwnds: Mutex::new(Vec::new()),
        }
    }
}

impl SharedWallpaperBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn push(&self, cmd: WallpaperCommand) {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(cmd);
    }

    pub fn drain_commands(&self) -> Vec<WallpaperCommand> {
        let mut q = self.commands.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    }

    pub fn register_hwnd(&self, hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        let mut list = self.own_hwnds.lock().unwrap_or_else(|e| e.into_inner());
        if !list.contains(&hwnd) {
            list.push(hwnd);
        }
    }

    pub fn unregister_hwnd(&self, hwnd: isize) {
        let mut list = self.own_hwnds.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|&h| h != hwnd);
    }

    pub fn own_hwnds(&self) -> Vec<isize> {
        self.own_hwnds.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn notify_taskbar_created(&self) {
        self.taskbar_recreated_gen.fetch_add(1, Ordering::SeqCst);
    }
}
