use super::*;

#[derive(Clone, Debug)]
pub(in crate::player) struct DesktopSnapshot {
    pub playing: bool,
    pub track_title: String,
    pub track_artist: String,
    pub display: Display,
    pub position_ms: f64,
    pub active_row: Option<Row>,
    pub active_row_index: Option<usize>,
    pub rows: Arc<Vec<Row>>,
    pub interlude_range: Option<(f64, f64)>,
    pub sync_offset_ms: i64,
    pub volume_pct: f64,
    pub volume_enabled: bool,
    pub volume_status: String,
    pub volume_label: String,
    pub candidates: Arc<Vec<PickerEntry>>,
    pub refresh_searching: bool,
}

impl Default for DesktopSnapshot {
    fn default() -> Self {
        Self {
            playing: false,
            track_title: String::new(),
            track_artist: String::new(),
            display: Display::default(),
            position_ms: 0.0,
            active_row: None,
            active_row_index: None,
            rows: Arc::new(Vec::new()),
            interlude_range: None,
            sync_offset_ms: 0,
            volume_pct: 100.0,
            volume_enabled: true,
            volume_status: "100%".into(),
            volume_label: "播放器内部音量".into(),
            candidates: Arc::new(Vec::new()),
            refresh_searching: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum DesktopCommand {
    Prev,
    PlayPause,
    Next,
    SyncNudge(i64),
    OpenVolume,
    SetVolumePct(f64),
    PickCandidate(usize),
    ToggleSmartTranslation(usize),
    RefreshLyrics,
    ImportLyrics,
    ReturnToMain,
    Close,
    SaveConfig(DesktopLyricsConfig),
}

#[derive(Clone, Debug)]
pub enum PopoverAction {
    SetOrientation(DesktopOrientation),
    FontDelta(f32),
    SetColor(u32),
    ToggleAutoScroll,
    SetDisplayLines(u8),
    OpenDisplayPopover(Box4, f32),
    CloseDisplayPopover,
    OpenColorPopover(Box4, f32),
    CloseColorPopover,
    OpenLinesPopover(Box4, f32),
    CloseLinesPopover,
    OpenLyricsPicker,
    DismissCascadeIfInactive,
    ReturnToMain,
}

#[derive(Default)]
pub struct SharedDesktopBridge {
    snapshot: Mutex<Arc<DesktopSnapshot>>,
    commands: Mutex<Vec<DesktopCommand>>,
    cfg: Mutex<DesktopLyricsConfig>,
    popover_actions: Mutex<Vec<PopoverAction>>,
    pub display_popover_open: AtomicBool,
    pub display_popover_side: AtomicI8,
    pub color_popover_open: AtomicBool,
    /// `0` = closed, `1` = opened on right side, `-1` = opened on left side.
    pub color_popover_side: AtomicI8,
    pub lines_popover_open: AtomicBool,
    pub lines_popover_side: AtomicI8,
    volume_closed_at: Mutex<Option<Instant>>,
    more_closed_at: Mutex<Option<Instant>>,
    display_closed_at: Mutex<Option<Instant>>,
    color_closed_at: Mutex<Option<Instant>>,
    lines_closed_at: Mutex<Option<Instant>>,
}

impl SharedDesktopBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn mark_closed(slot: &Mutex<Option<Instant>>) {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    fn take_recently_closed(slot: &Mutex<Option<Instant>>, window_ms: u128) -> bool {
        let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
        let recent = g.is_some_and(|t| t.elapsed().as_millis() <= window_ms);
        if recent {
            *g = None;
        }
        recent
    }

    pub fn mark_volume_closed(&self) {
        Self::mark_closed(&self.volume_closed_at);
    }

    pub fn mark_more_closed(&self) {
        Self::mark_closed(&self.more_closed_at);
    }

    pub fn mark_display_closed(&self) {
        self.display_popover_open.store(false, Ordering::SeqCst);
        self.display_popover_side.store(0, Ordering::SeqCst);
        Self::mark_closed(&self.display_closed_at);
    }

    pub fn mark_color_closed(&self) {
        self.color_popover_open.store(false, Ordering::SeqCst);
        self.color_popover_side.store(0, Ordering::SeqCst);
        Self::mark_closed(&self.color_closed_at);
    }

    pub fn mark_lines_closed(&self) {
        self.lines_popover_open.store(false, Ordering::SeqCst);
        self.lines_popover_side.store(0, Ordering::SeqCst);
        Self::mark_closed(&self.lines_closed_at);
    }

    pub fn was_volume_recently_closed(&self) -> bool {
        Self::take_recently_closed(&self.volume_closed_at, 260)
    }

    pub fn was_more_recently_closed(&self) -> bool {
        Self::take_recently_closed(&self.more_closed_at, 260)
    }

    pub fn was_display_recently_closed(&self) -> bool {
        Self::take_recently_closed(&self.display_closed_at, 260)
    }

    pub fn was_color_recently_closed(&self) -> bool {
        Self::take_recently_closed(&self.color_closed_at, 260)
    }

    pub fn was_lines_recently_closed(&self) -> bool {
        Self::take_recently_closed(&self.lines_closed_at, 260)
    }

    pub fn push(&self, cmd: DesktopCommand) {
        self.commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(cmd);
    }

    pub fn drain_commands(&self) -> Vec<DesktopCommand> {
        let mut q = self.commands.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    }

    pub fn push_popover(&self, act: PopoverAction) {
        self.popover_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(act);
    }

    pub fn drain_popover(&self) -> Vec<PopoverAction> {
        let mut q = self
            .popover_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *q)
    }

    pub fn set_cfg(&self, cfg: DesktopLyricsConfig) {
        *self.cfg.lock().unwrap_or_else(|e| e.into_inner()) = cfg;
    }

    pub fn read_cfg(&self) -> DesktopLyricsConfig {
        self.cfg.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(in crate::player) fn update_snapshot(&self, snap: DesktopSnapshot) {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(snap);
    }

    pub(in crate::player) fn read_snapshot(&self) -> Arc<DesktopSnapshot> {
        Arc::clone(&self.snapshot.lock().unwrap_or_else(|e| e.into_inner()))
    }
}
