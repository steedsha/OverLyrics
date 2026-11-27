//! Temporary immersive desktop wallpaper mode (Plan A).
//!
//! Spawns borderless GPU-rendered immersive background window(s) attached to the
//! Windows desktop `WorkerW` layer (behind desktop icons, without touching system
//! wallpaper registry keys), paired with an edge-docked auto-hide floating glass
//! control bar and fullscreen/lockscreen/pause auto-sleep (0% GPU when hidden).

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use super::model::ThemeMode;
use super::scene::{BeatConfig, Cover};

pub mod bg_view;
pub mod control_bar;
pub mod win32_workerw;

#[derive(Clone)]
pub struct WallpaperSnapshot {
    pub playing: bool,
    pub track_title: String,
    pub track_artist: String,
    pub cover: Option<Arc<Cover>>,
    pub cover_sig: u64,
    pub theme_mode: ThemeMode,
    pub corner_radius: f32,
    pub accent_hex: u32,
    pub beat: BeatConfig,
    pub volume_pct: f64,
    pub volume_enabled: bool,
    pub desktop_lyrics_open: bool,
}

impl Default for WallpaperSnapshot {
    fn default() -> Self {
        Self {
            playing: false,
            track_title: String::new(),
            track_artist: String::new(),
            cover: None,
            cover_sig: 0,
            theme_mode: ThemeMode::LiquidGlass,
            corner_radius: 18.0,
            accent_hex: 0x0A84FF,
            beat: BeatConfig::default(),
            volume_pct: 100.0,
            volume_enabled: true,
            desktop_lyrics_open: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum WallpaperCommand {
    Prev,
    PlayPause,
    Next,
    SetVolumePct(f64),
    ToggleDesktopLyrics,
    ExitWallpaper,
}

pub struct SharedWallpaperBridge {
    snapshot: Mutex<Arc<WallpaperSnapshot>>,
    commands: Mutex<Vec<WallpaperCommand>>,
    pub taskbar_recreated_gen: AtomicU64,
    own_hwnds: Mutex<Vec<isize>>,
}

impl Default for SharedWallpaperBridge {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(Arc::new(WallpaperSnapshot::default())),
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

    pub fn update_snapshot(&self, snap: WallpaperSnapshot) {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(snap);
    }

    pub fn read_snapshot(&self) -> Arc<WallpaperSnapshot> {
        Arc::clone(&self.snapshot.lock().unwrap_or_else(|e| e.into_inner()))
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
