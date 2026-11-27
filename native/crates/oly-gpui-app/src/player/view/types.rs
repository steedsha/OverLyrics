use super::*;

/// Where the experiment's playback numbers come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A frozen fixture replayed on the wall clock. Nothing is written to any
    /// real player, and the chrome says so.
    Replay,
    /// Real session, online lyrics and guarded user transport commands.
    Live,
}

/// Frame input that comes from the process arguments rather than the UI.
#[derive(Clone, Debug)]
pub struct ViewConfig {
    pub fixture: Option<PathBuf>,
    pub motion: Option<PathBuf>,
    pub cover: Option<PathBuf>,
    pub seconds: Option<u64>,
    pub evidence_dir: Option<PathBuf>,
    pub scale: f32,
    pub source: Source,
    /// Runs the scripted interaction pass instead of waiting for a pointer.
    /// Only the QA harness sets this; a normal run leaves it `None` and the
    /// player reacts to nothing but real input.
    pub scenario: Option<String>,
    /// An offscreen capture for viewports a screen grab cannot reach. The
    /// player freezes the replay clock at `capture_at_ms`, lets one full frame
    /// paint, then renders that frame's scene to a texture and writes a PNG
    /// here. Needs the `qa-capture` feature, which is what exposes
    /// `Window::render_to_image`.
    pub capture_rgba: Option<PathBuf>,
    pub capture_at_ms: Option<f64>,
    /// Pins the replay clock to this media time and pauses. The side-by-side
    /// stills compare the same song at the same instant, so the QA harness
    /// freezes instead of hoping two runs stop on the same frame.
    pub freeze_ms: Option<f64>,
    /// Starts the player directly in Mini mode (380x176) on normal launch.
    pub start_mini: bool,
    /// True for the child settings sidecar window (skips GPU background/font reload).
    pub settings_only: bool,
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            fixture: None,
            motion: None,
            cover: None,
            seconds: None,
            evidence_dir: None,
            scale: 1.0,
            source: Source::Replay,
            scenario: None,
            capture_rgba: None,
            capture_at_ms: None,
            freeze_ms: None,
            start_mini: false,
            settings_only: false,
        }
    }
}

/// One scripted interaction step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Act {
    HoverLyric,
    SettingsShot(bool),BackgroundShot(u8),FocusAway,FrameSample,FrameRate,VersionProbe,
    LensDragStart(u8),LensDragMove(u8),LensDragEnd,LensShot(&'static str),
    DetailsMode,DetailsShortcut,DetailsProbe,DetailsDrag,DetailsColor,DetailsColorProbe,DetailsClose,DetailsClosed,
    SetMode(u8),
    ProbeBounds(bool, f32, f32),
    Wheel(i32),
    Pause,
    Resume,
    ThumbDrag(i32),
    ThumbDown,
    ThumbMove(i32),
    ThumbUp,
    OpenMore,
    MoreRow(u8),
    Pick(usize),
    OpenVolume,
    Volume(f64),
    ToggleTranslation,
    ToggleBreak,
    ClosePanels,
    Resize(u32, u32),
}

/// The scripted interaction pass.
///
/// The delivery recording has to show the real code paths reacting, so every
/// step calls the same entry point the pointer or keyboard listener calls
/// (`on_scroll`, `on_down`, `on_drag`, `on_key`) and hits the same rects the
/// frame paints. Setting state fields directly would produce a video that
/// proves nothing about the wiring.
#[derive(Default)]
pub(super) struct Scenario {
    pub(super) steps: Vec<(f64, Act)>,
    pub(super) next: usize,
    pub(super) last_step: Option<(f64,f64)>,
}

impl Scenario {
    // Preserve each scheduled gap even after slow startup/GPU warm-up. Never
    // batch a menu open and its hit-test into one unpainted frame.
    pub(super) fn take_due(&mut self, now:f64)->Option<Act>{
        let (at,act)=*self.steps.get(self.next)?;
        let (elapsed,gap)=self.last_step.map(|(prev,wall)|(now-wall,at-prev)).unwrap_or((now,at));
        if elapsed<gap{return None;}
        self.next+=1;self.last_step=Some((at,now));Some(act)
    }
    pub(super) fn elapsed(&self,now:f64)->f64{self.last_step.map(|(planned,real)|now-real+planned).unwrap_or(now)}
    pub(super) fn named(name: &str) -> Option<Self> {
        if name=="settings-split"{return Some(Self{steps:vec![(100.,Act::OpenMore),(300.,Act::MoreRow(3)),(850.,Act::SettingsShot(false)),(1000.,Act::OpenMore),(1200.,Act::MoreRow(4)),(1750.,Act::SettingsShot(true)),(1900.,Act::ClosePanels),(2100.,Act::OpenMore),(2300.,Act::MoreRow(3)),(2900.,Act::VersionProbe)],next:0,last_step:None});}
        if name=="lens-drag"{return Some(Self{steps:vec![(100.,Act::OpenMore),(450.,Act::LensDragStart(0)),(520.,Act::LensDragMove(0)),(720.,Act::LensShot("more-drag.png")),(780.,Act::LensDragEnd),(900.,Act::MoreRow(4)),(1400.,Act::LensDragStart(1)),(1460.,Act::LensDragMove(1)),(1680.,Act::LensShot("app-drag-translate.png")),(1740.,Act::LensDragEnd),(1900.,Act::LensShot("app-translate-rest.png"))],next:0,last_step:None});}
        if name=="background-focus"{return Some(Self{steps:vec![(600.,Act::BackgroundShot(1)),(1400.,Act::BackgroundShot(2)),(1600.,Act::FocusAway),(1900.,Act::FrameSample),(3100.,Act::FrameRate)],next:0,last_step:None});}
        if name=="lyric-hover" {return Some(Self{steps:vec![(0.,Act::HoverLyric)],next:0,last_step:None});}
        if name=="settings-corners"{return Some(Self{steps:vec![(100.,Act::DetailsMode),(250.,Act::OpenMore),(500.,Act::MoreRow(4)),(1200.,Act::DetailsProbe)],next:0,last_step:None});}
        if name=="fullscreen-roundtrip"{
            return Some(Self{steps:vec![
                (100.,Act::SetMode(3)),
                (600.,Act::ProbeBounds(true,0.,0.)),
                (700.,Act::SetMode(2)),
                (1200.,Act::ProbeBounds(false,1272.,843.)),
                (1300.,Act::SetMode(1)),
                (1900.,Act::ProbeBounds(false,380.,650.)),
                (2000.,Act::SetMode(3)),
                (2500.,Act::ProbeBounds(true,0.,0.)),
                (2600.,Act::SetMode(2)),
                (3200.,Act::ProbeBounds(false,1272.,843.)),
                (3300.,Act::SetMode(3)),
                (3700.,Act::ProbeBounds(true,0.,0.)),
                (3800.,Act::SetMode(1)),
                (4700.,Act::ProbeBounds(false,380.,650.)),
            ],next:0,last_step:None});
        }
        if name=="compact-details"{return Some(Self{steps:vec![(100.,Act::DetailsMode),(250.,Act::OpenMore),(500.,Act::MoreRow(4)),(1200.,Act::DetailsProbe),(1600.,Act::DetailsDrag),(1750.,Act::DetailsColor),(2050.,Act::DetailsColorProbe),(2300.,Act::DetailsClose),(3100.,Act::DetailsClosed),(3200.,Act::DetailsShortcut),(3800.,Act::DetailsProbe),(4000.,Act::DetailsClose),(4600.,Act::DetailsClosed)],next:0,last_step:None});}
        if name=="scrollbar" {return Some(Self{steps:vec![(100.,Act::Pause),(200.,Act::ThumbDown),(700.,Act::ThumbMove(45)),(12000.,Act::ThumbUp)],next:0,last_step:None});}
        if name == "liquid" {
            return Some(Self {steps:vec![
                (100.0,Act::Pause),(500.0,Act::OpenMore),(570.0,Act::OpenMore),(620.0,Act::OpenMore),
                (1100.0,Act::ToggleTranslation),(1160.0,Act::ToggleTranslation),(1600.0,Act::MoreRow(4)),
                (2300.0,Act::OpenVolume),(2700.0,Act::ClosePanels),(2780.0,Act::OpenVolume),
                (3500.0,Act::ClosePanels),(4100.0,Act::OpenMore),(4800.0,Act::ClosePanels),
            ],next:0,last_step:None});
        }
        if name != "interaction" {
            return None;
        }
        let mut steps = Vec::new();
        let mut at = |ms: f64, act: Act| steps.push((ms, act));
        // The automatic per-word sweep runs first, untouched.
        at(6_000.0, Act::Pause);
        at(8_000.0, Act::Resume);
        // A fast wheel burst in both directions. The engine's own debounce and
        // resume window decide what the picture does; nothing here forces it.
        for i in 0..10 {
            at(10_000.0 + i as f64 * 55.0, Act::Wheel(240));
        }
        for i in 0..10 {
            at(11_200.0 + i as f64 * 55.0, Act::Wheel(-240));
        }
        // One thumb drag each way, off the painted scrollbar thumb.
        at(13_000.0, Act::ThumbDrag(-220));
        at(13_900.0, Act::ThumbDrag(140));
        // Panels, in the order the report lists them.
        at(15_200.0, Act::OpenMore);
        at(16_000.0, Act::MoreRow(3));
        at(16_800.0, Act::Pick(1));
        at(17_600.0, Act::OpenVolume);
        at(18_200.0, Act::Volume(35.0));
        at(19_000.0, Act::ClosePanels);
        // Typography toggles, both back to their starting value so the end of
        // the clip is comparable with the start.
        at(19_400.0, Act::ToggleTranslation);
        at(20_400.0, Act::ToggleTranslation);
        at(21_200.0, Act::ToggleBreak);
        at(22_200.0, Act::ToggleBreak);
        // Resize pressure, then back.
        at(23_000.0, Act::Resize(800, 500));
        at(25_000.0, Act::Resize(1272, 843));
        Some(Self { steps, next: 0, last_step:None })
    }
}

/// A pointer target. Hit rects are computed once per frame by the same code
/// that paints them, so what looks clickable is clickable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PickerKind {
    Lyrics,
    Media,
    Cover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hot {
    Prev,
    Play,
    Next,
    More,
    Volume,
    LyricsToggle,
    Fold,
    Progress,
    Thumb,
    MoreRow(u8),
    VolumeTrack,
    Candidate(usize),
    SmartTranslation(usize),
    CoverSwitch(u8),
    PickerRefresh,
    PickerImport,
    PickerPickFolder,
    AlbumEnter,
    AlbumReturn,
    SyncMinus,
    SyncPlus,
    MenuMode(u8),
    SettingsClose,
    SettingsPage(u8),
    SettingSlider(u8),
    SettingChoice(u8),
    DropdownOption(u8),
    ModalCancel,
    ModalSave,
    WindowPin,
    WindowMinimize,
    WindowFullscreen,
    WindowClose,
    LyricRow(usize),
}

pub(super) fn pointer_hit(hot: &[(Hot,Box4)], panels: &[Box4], x: f32, y: f32) -> Option<Hot> {
    let covered=panels.iter().any(|r| r.contains(x,y));
    hot.iter().rev().find(|(h,r)| r.contains(x,y) && (!covered || matches!(h,Hot::MoreRow(_) | Hot::Candidate(_) | Hot::SmartTranslation(_) | Hot::CoverSwitch(_) | Hot::PickerRefresh | Hot::PickerImport | Hot::PickerPickFolder | Hot::VolumeTrack | Hot::SettingsClose | Hot::SettingsPage(_) | Hot::SettingSlider(_) | Hot::SettingChoice(_) | Hot::DropdownOption(_) | Hot::ModalCancel | Hot::ModalSave | Hot::SyncMinus | Hot::SyncPlus | Hot::MenuMode(_)))).map(|(h,_)| *h)
}

/// The fixed chrome geometry for one frame.
#[derive(Clone, Debug, Default)]
pub(super) struct Chrome {
    pub(super) prev: Box4,
    pub(super) play: Box4,
    pub(super) next: Box4,
    pub(super) more: Box4,
    pub(super) volume: Box4,
    pub(super) lyrics_toggle: Box4,
    pub(super) progress: Box4,
    /// The lyric scrollbar: a thin track down the right edge of the lyric
    /// column and the thumb that reflects the shared scroll offset. It is not
    /// the song progress thumb above — the two measure different axes.
    pub(super) scroll_track: Box4,
    pub(super) scroll_thumb: Box4,
}

impl Chrome {
    /// `scroll` is `(offset, min, max)` from the shared [`ScrollSurface`]. The
    /// lyric scrollbar is the only chrome that depends on it; passing it here
    /// keeps the painted thumb and the draggable rect the same rectangle.
    pub(super) fn build(m: &Metrics, scroll: (f64, f64, f64)) -> Self {
        // Window / Fullscreen transport: Play/Pause sits strictly on the cover
        // center axis with Prev/Next symmetric around it, Volume on the left,
        // and More + LyricsToggle paired closely on the right.
        let bar = m.toolbar;
        let s = m.ui_scale;
        let cy = |size: f32| bar.y + (bar.h - size) * 0.5;
        let cx_box = |cx: f32, size: f32| Box4::new(cx - size * 0.5, cy(size), size, size);
        let center_x = bar.x + bar.w * 0.5;
        let transport_step = (bar.w * 0.165).clamp(32.0 * s, 66.0 * s).min((bar.w * 0.24).max(28.0 * s));
        let play = cx_box(center_x, 56.0 * s);
        let prev = cx_box(center_x - transport_step, 44.0 * s);
        let next = cx_box(center_x + transport_step, 44.0 * s);
        let pair_step = (bar.w * 0.105).clamp(28.0 * s, 44.0 * s);
        let lyrics_cx = (bar.right() - 18.0 * s).max(center_x + 32.0 * s);
        let lyrics_toggle = cx_box(lyrics_cx, 36.0 * s);
        let more_cx = (lyrics_cx - pair_step).max(center_x + 16.0 * s);
        let more = cx_box(more_cx, 36.0 * s);
        let volume = cx_box((bar.x + 18.0 * s).min(center_x - 32.0 * s), 36.0 * s);

        let mut progress = m.progress;
        progress.h = (18.0 * s).max(12.0);
        if m.cover.w<=56. && m.cover_meta_top==22. {progress.x+=46.;progress.w=(progress.w-92.).max(1.);progress.y+=6.;}
        // The lyric scrollbar hugs the right edge of the lyric column. Its
        // thumb height is the visible fraction of the document, so it grows
        // when a window shows most of the song and shrinks on a long one.
        let scroll_track = Box4::new(
            (m.win_w - 18.0).max(0.0),
            m.lyric_viewport.y + 8.0,
            4.0,
            (m.lyric_viewport.h - 16.0).max(1.0),
        );
        let span = (scroll.2 - scroll.1).max(1.0);
        let view = (m.lyrics.h as f64).max(1.0);
        let track_h = scroll_track.h as f64;
        let thumb_max = (track_h * 0.65).max(16.0_f64.min(track_h));
        let thumb_h = ((view / (span + view)) * track_h)
            .clamp(16.0_f64.min(track_h), thumb_max) as f32;
        // A larger offset means earlier content, which is *up* the document,
        // so the thumb fraction runs from the bottom back to the top.
        let normalised = ((scroll.0 - scroll.1) / span).clamp(0.0, 1.0) as f32;
        let scroll_thumb = Box4::new(
            scroll_track.x - 2.0,
            scroll_track.y + (1.0 - normalised) * (scroll_track.h - thumb_h),
            8.0,
            thumb_h,
        );
        Self {
            prev,
            play,
            next,
            more,
            volume,
            lyrics_toggle,
            progress,
            scroll_track,
            scroll_thumb,
        }
    }
}

/// One row ready to paint.
pub(super) struct PlanRow {
    pub(super) hover_region: Option<(usize, Box4)>,
    pub(super) lines: Arc<RowLines>,
    pub(super) bounds: Box4,
    pub(super) align: paint::LineAlign,
    pub(super) light: RowLight,
    pub(super) translation_alpha: f32,
    pub(super) roman_alpha: f32,
    pub(super) scale: f32,
    pub(super) wrapper: paint::WrapperTransform,
    pub(super) blur: f32,
    pub(super) top: f32,
}

/// Interlude three-dot indicator ready to paint.
pub(super) struct PlanInterlude {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) dot_diameter: f32,
    pub(super) dot_gap: f32,
    pub(super) scale: f32,
    pub(super) opacity: f32,
    pub(super) dot_opacities: [f32; 3],
}

/// Bottom credits line ready to paint.
pub(super) struct PlanBottom {
    pub(super) lines: Arc<crate::player::rows::BottomLines>,
    pub(super) x_base: f32,
    pub(super) wrap_width: f32,
    pub(super) align: paint::LineAlign,
    pub(super) top: f32,
    pub(super) alpha: f32,
    pub(super) blur: f32,
}

/// One glass panel ready to paint.
pub(super) struct PlanPanel {
    pub(super) content_alpha: f32,
    pub(super) surface_alpha: f32,
    pub(super) kind: PanelKind,
    pub(super) rect: GlassRect,
    pub(super) surface: CustomSurfaceHandle,
    pub(super) snapshot: CustomSurfaceHandle,
    pub(super) hook: PaintHook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PanelKind {
    More,
    Volume,
    Picker,
    Settings,
    Thumb,
    SliderLens,
    ButtonLens,
    SegmentedLens,
    TabLens,
    Dropdown,
    Island,
}

/// Everything the paint pass needs, computed before the closure is built so the
/// paint never reaches back into the entity.
#[derive(Clone, Debug)]
pub(in crate::player) struct PickerEntry {
    pub(in crate::player) title: String,
    pub(in crate::player) subtitle: String,
    pub(in crate::player) match_badge: String,
    pub(in crate::player) timing_badge: Option<&'static str>,
    pub(in crate::player) has_translation: bool,
    pub(in crate::player) smart_translation: bool,
    pub(in crate::player) smart_translation_key: String,
    pub(in crate::player) path: PathBuf,
    pub(in crate::player) source: String,
    pub(in crate::player) id: String,
    pub(in crate::player) candidate_index: usize,
    pub(in crate::player) selected: bool,
}
pub(in crate::player) const VERSION_HEADER: f32 = 42.0;
pub(in crate::player) const VERSION_ROW: f32 = 64.0;

pub(in crate::player) fn picker_smart_btn_rect(row: Box4) -> Box4 {
    Box4::new(row.right() - 96.0, row.y + 30.0, 90.0, 22.0)
}

pub(in crate::player) fn picker_refresh_rect(panel: GlassRect) -> Box4 {
    Box4::new(panel.left as f32 + panel.width as f32 - 36.0, panel.top as f32 + 7.0, 26.0, 26.0)
}

pub(in crate::player) fn picker_import_rect(panel: GlassRect) -> Box4 {
    Box4::new(panel.left as f32 + panel.width as f32 - 66.0, panel.top as f32 + 7.0, 26.0, 26.0)
}

pub(super) fn album_return_rect() -> Box4 {
    Box4::new(10.0, 10.0, 22.0, 22.0)
}

pub(super) struct Plan {
    pub(super) settings_only: bool,
    pub(super) immersion: bool,
    pub(super) metrics: Metrics,
    pub(super) chrome: Chrome,
    pub(super) bg: Option<CustomSurfaceHandle>,
    pub(super) cover: Option<CustomSurfaceHandle>,
    pub(super) prev_cover: Option<CustomSurfaceHandle>,
    pub(super) cover_fade: f32,
    pub(super) motion: Option<CustomSurfaceHandle>,
    pub(super) motion_alpha: f32,
    pub(super) rows: Vec<PlanRow>,
    pub(super) lyric_hover: Option<(Box4, f32)>,
    pub(super) interlude: Option<PlanInterlude>,
    pub(super) bottom: Option<PlanBottom>,
    pub(super) panels: Vec<PlanPanel>,
    pub(super) position_ms: f64,
    pub(super) duration_ms: f64,
    pub(super) play_morph: f32,
    pub(super) translation: bool,
    pub(super) badge: Option<String>,
    pub(super) status: String,
    pub(super) track_title: String,
    pub(super) track_artist: String,
    pub(super) cursor: Option<(f32, f32)>,
    pub(super) drag_thumb: Option<(f32, f32)>,
    pub(super) scroll_glass: Option<PlanPanel>,
    pub(super) scroll_line: Option<(Box4, f32)>,
    pub(super) slider_lens: Option<PlanPanel>,
    pub(super) button_lens: Option<(Hot, PlanPanel, Box4, Feedback)>,
    pub(super) window_controls_alpha: f32,
    pub(super) mode_well_rect: Box4,
    pub(super) mode_lens_rect: Box4,
    pub(super) mode_swell: f32,
    pub(super) mode_lens_glass: Option<PlanPanel>,
    pub(super) active_mode: u8,
    pub(super) feedback: Vec<(Hot, Feedback)>,
    pub(super) volume_enabled: bool,
    pub(super) lyric_position_ms: f64,
    pub(super) sync_enabled: bool,
    pub(super) sync_label: String,
    pub(super) volume_label: String,
    pub(super) volume_available: bool,
    pub(super) mode: Mode,
    pub(super) fullscreen: bool,
    pub(super) always_on_top: bool,
    pub(super) chrome_visible: bool,
    pub(super) content_alpha: f32,
    pub(super) chrome_alpha: f32,
    pub(super) compact_chrome_alpha: f32,
    pub(super) album_meta_alpha: f32,
    pub(super) cover_scale: f32,
    pub(super) cover_vis: f32,
    pub(super) picker_scroll_px: f32,
    pub(super) picker_kind: PickerKind,
    pub(super) show_media_row: bool,
    pub(super) show_wallpaper_row: bool,
    pub(super) picker_refresh_angle: f32,
    pub(super) control_pending: bool,
    pub(super) lyrics_visible: bool,
    pub(super) lyrics_alpha: f32,
    pub(super) smart_switch_fractions: Vec<(String, f32)>,
    pub(super) cover_switch_fractions: [f32; 2],
    pub(super) empty_notice: Option<String>,
    pub(super) island: Option<(Box4, f32)>,
    pub(super) island_glass_panel: Option<PlanPanel>,
    pub(super) display: Display,
    pub(super) settings_paint: Option<settings::Presentation>,
}


pub(super) struct SettingsHost {pub(super) parent:WeakEntity<PlayerView>,pub(super) side:crate::player::details::PanelSide,pub(super) last_config:serde_json::Value}

#[derive(Clone)]
pub(super) struct InlineSwitchDrag {
    pub(super) key: String,
    pub(super) track: Box4,
    pub(super) origin: f32,
    pub(super) fraction: f32,
    pub(super) moved: bool,
}

pub(super) const COVER_SWITCH_ROW: f32 = 44.0;

pub(super) fn cover_switch_row_rect(panel_left: f32, panel_top: f32, panel_w: f32, i: u8) -> Box4 {
    Box4::new(
        panel_left + 8.0,
        panel_top + VERSION_HEADER + i as f32 * COVER_SWITCH_ROW,
        panel_w - 16.0,
        COVER_SWITCH_ROW - 4.0,
    )
}

pub(super) fn cover_switch_track_rect(row: Box4) -> Box4 {
    Box4::new(row.right() - 42.0, row.y + (row.h - 19.0) * 0.5, 34.0, 19.0)
}

pub(super) fn picker_smart_switch_rect(row: Box4) -> Box4 {
    let btn = picker_smart_btn_rect(row);
    Box4::new(btn.right() - 34.0, btn.y + 2.5, 34.0, 19.0)
}

pub(super) fn window_caption_rects(win_w: f32, compact: bool) -> (Box4, [Box4; 4]) {
    if compact {
        let zone = Box4::new((win_w - 138.0).max(0.0), 0.0, 138.0, 48.0);
        let pin_b = Box4::new(win_w - 110.0, 10.0, 22.0, 22.0);
        let min_b = Box4::new(win_w - 84.0, 10.0, 22.0, 22.0);
        let fs_b = Box4::new(win_w - 58.0, 10.0, 22.0, 22.0);
        let close_b = Box4::new(win_w - 32.0, 10.0, 22.0, 22.0);
        (zone, [pin_b, min_b, fs_b, close_b])
    } else {
        let zone = Box4::new((win_w - 194.0).max(0.0), 0.0, 194.0, 74.0);
        let pin_b = Box4::new(win_w - 144.0, 14.0, 28.0, 28.0);
        let min_b = Box4::new(win_w - 110.0, 14.0, 28.0, 28.0);
        let fs_b = Box4::new(win_w - 76.0, 14.0, 28.0, 28.0);
        let close_b = Box4::new(win_w - 42.0, 14.0, 28.0, 28.0);
        (zone, [pin_b, min_b, fs_b, close_b])
    }
}


pub(in crate::player) fn picker_offline_folder_btn_rect(rect: GlassRect) -> Box4 {
    let w = 74.0_f32;
    let h = 26.0_f32;
    let x = rect.left as f32 + rect.width as f32 - 14.0 - w;
    let y = rect.top as f32 + VERSION_HEADER + 6.0 + (34.0 - h) * 0.5;
    Box4::new(x, y, w, h)
}
