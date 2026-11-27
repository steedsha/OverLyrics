//! Temporary immersive desktop wallpaper mode (Plan A).
//!
//! Renders the full immersive desktop scene (dynamic GPU background + cover +
//! track title/artist + AMLL lyrics, without bottom transport controls) under
//! desktop icons via `WorkerW`, paired with a top-center auto-hide floating
//! liquid-glass Island pill (reusing the no-cover Island layout with an Exit
//! button) and fullscreen/lockscreen/pause auto-sleep.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use super::model::{Display, Row, ThemeMode};
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
    pub display: Display,
    pub rows: Arc<Vec<Row>>,
    pub active_row_index: Option<usize>,
    pub position_ms: f64,
    pub sampled_at: Instant,
    pub theme_mode: ThemeMode,
    pub corner_radius: f32,
    pub accent_hex: u32,
    pub beat: BeatConfig,
    pub volume_pct: f64,
    pub volume_enabled: bool,
}

impl Default for WallpaperSnapshot {
    fn default() -> Self {
        Self {
            playing: false,
            track_title: String::new(),
            track_artist: String::new(),
            cover: None,
            cover_sig: 0,
            display: Display::default(),
            rows: Arc::new(Vec::new()),
            active_row_index: None,
            position_ms: 0.0,
            sampled_at: Instant::now(),
            theme_mode: ThemeMode::LiquidGlass,
            corner_radius: 18.0,
            accent_hex: 0x0A84FF,
            beat: BeatConfig::default(),
            volume_pct: 100.0,
            volume_enabled: true,
        }
    }
}

#[derive(Clone, Debug)]
pub enum WallpaperCommand {
    Prev,
    PlayPause,
    Next,
    SetVolumePct(f64),
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
