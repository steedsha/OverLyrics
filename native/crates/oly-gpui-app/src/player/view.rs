//! The player view: one window, one frame, one GPU device.
//!
//! Everything the user sees is painted here, in this order: immersion
//! background, cover (still or motion), lyric rows, progress, toolbar, then the
//! glass panels on top. The panels are real glass: a paint hook copies the
//! region behind them out of the live render target, filters it, and composites
//! the panel back into its own texture in the same frame — no CPU readback and
//! no second renderer.
//!
//! Interaction is one state machine. The wheel, the touch pan and the scrollbar
//! thumb all move the same [`ScrollSurface`] offset, so the picture cannot drift
//! between two competing scroll positions, and the automatic follow only
//! resumes after the engine's debounce window has passed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::*;
use oly_contracts::lyrics::LyricDoc;
use oly_render::glass::{place_popover, Material, Rect as GlassRect, Side};
use oly_render::mask::position_at;

use crate::ui::controls::{Mode, ToolBarState};
use crate::ui::player::ScrollSurface;
use crate::ui::volume::{VolumePanel, VolumeSource, VolumeWrite, SLIDER_MAX, SLIDER_MIN};

#[path = "translation_view.rs"]
mod translation_view;

use super::cadence::PresentCadence;
use super::chrome;
use super::{input,sync::SyncState};
use super::settings::{self,Settings,Control};
use super::icons::{self, Icon};
use super::panel_motion::PanelMotion;
use super::elastic::{Elastic,PressGesture};
use super::fonts;
use super::glass::GlassPanel;
use super::interaction::{Feedback, panel_pixels};
use super::model::{self as model, row_light, Box4, Display, Metrics, Row};
use super::paint::{self, RowLight};
use super::rows::{RowLines, Shaper};
use super::scene::{decode_cover_file, Cover, PlayerScene};

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
enum Act {
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
struct Scenario {
    steps: Vec<(f64, Act)>,
    next: usize,
    last_step: Option<(f64,f64)>,
}

impl Scenario {
    // Preserve each scheduled gap even after slow startup/GPU warm-up. Never
    // batch a menu open and its hit-test into one unpainted frame.
    fn take_due(&mut self, now:f64)->Option<Act>{
        let (at,act)=*self.steps.get(self.next)?;
        let (elapsed,gap)=self.last_step.map(|(prev,wall)|(now-wall,at-prev)).unwrap_or((now,at));
        if elapsed<gap{return None;}
        self.next+=1;self.last_step=Some((at,now));Some(act)
    }
    fn elapsed(&self,now:f64)->f64{self.last_step.map(|(planned,real)|now-real+planned).unwrap_or(now)}
    fn named(name: &str) -> Option<Self> {
        if name=="settings-split"{return Some(Self{steps:vec![(100.,Act::OpenMore),(300.,Act::MoreRow(3)),(850.,Act::SettingsShot(false)),(1000.,Act::OpenMore),(1200.,Act::MoreRow(4)),(1750.,Act::SettingsShot(true)),(1900.,Act::ClosePanels),(2100.,Act::OpenMore),(2300.,Act::MoreRow(3)),(2900.,Act::VersionProbe)],next:0,last_step:None});}
        if name=="lens-drag"{return Some(Self{steps:vec![(100.,Act::OpenMore),(450.,Act::LensDragStart(0)),(520.,Act::LensDragMove(0)),(720.,Act::LensShot("more-drag.png")),(780.,Act::LensDragEnd),(900.,Act::MoreRow(4)),(1400.,Act::LensDragStart(1)),(1460.,Act::LensDragMove(1)),(1680.,Act::LensShot("app-drag-translate.png")),(1740.,Act::LensDragEnd),(1900.,Act::LensShot("app-translate-rest.png"))],next:0,last_step:None});}
        if name=="background-focus"{return Some(Self{steps:vec![(600.,Act::BackgroundShot(1)),(1400.,Act::BackgroundShot(2)),(1600.,Act::FocusAway),(1900.,Act::FrameSample),(3100.,Act::FrameRate)],next:0,last_step:None});}
        if name=="lyric-hover" {return Some(Self{steps:vec![(0.,Act::HoverLyric)],next:0,last_step:None});}
        if name=="settings-corners"{return Some(Self{steps:vec![(100.,Act::DetailsMode),(250.,Act::OpenMore),(500.,Act::MoreRow(3)),(1200.,Act::DetailsProbe)],next:0,last_step:None});}
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
        if name=="compact-details"{return Some(Self{steps:vec![(100.,Act::DetailsMode),(250.,Act::OpenMore),(500.,Act::MoreRow(3)),(1200.,Act::DetailsProbe),(1600.,Act::DetailsDrag),(1750.,Act::DetailsColor),(2050.,Act::DetailsColorProbe),(2300.,Act::DetailsClose),(3100.,Act::DetailsClosed),(3200.,Act::DetailsShortcut),(3800.,Act::DetailsProbe),(4000.,Act::DetailsClose),(4600.,Act::DetailsClosed)],next:0,last_step:None});}
        if name=="scrollbar" {return Some(Self{steps:vec![(100.,Act::Pause),(200.,Act::ThumbDown),(700.,Act::ThumbMove(45)),(12000.,Act::ThumbUp)],next:0,last_step:None});}
        if name == "liquid" {
            return Some(Self {steps:vec![
                (100.0,Act::Pause),(500.0,Act::OpenMore),(570.0,Act::OpenMore),(620.0,Act::OpenMore),
                (1100.0,Act::ToggleTranslation),(1160.0,Act::ToggleTranslation),(1600.0,Act::MoreRow(3)),
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
enum PickerKind {
    Lyrics,
    Media,
    Cover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hot {
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
    WindowMinimize,
    WindowFullscreen,
    WindowClose,
    LyricRow(usize),
}

fn pointer_hit(hot: &[(Hot,Box4)], panels: &[Box4], x: f32, y: f32) -> Option<Hot> {
    let covered=panels.iter().any(|r| r.contains(x,y));
    hot.iter().rev().find(|(h,r)| r.contains(x,y) && (!covered || matches!(h,Hot::MoreRow(_) | Hot::Candidate(_) | Hot::SmartTranslation(_) | Hot::CoverSwitch(_) | Hot::PickerRefresh | Hot::PickerImport | Hot::VolumeTrack | Hot::SettingsClose | Hot::SettingsPage(_) | Hot::SettingSlider(_) | Hot::SettingChoice(_) | Hot::DropdownOption(_) | Hot::ModalCancel | Hot::ModalSave | Hot::SyncMinus | Hot::SyncPlus | Hot::MenuMode(_)))).map(|(h,_)| *h)
}

/// The fixed chrome geometry for one frame.
#[derive(Clone, Debug, Default)]
struct Chrome {
    prev: Box4,
    play: Box4,
    next: Box4,
    more: Box4,
    volume: Box4,
    lyrics_toggle: Box4,
    progress: Box4,
    /// The lyric scrollbar: a thin track down the right edge of the lyric
    /// column and the thumb that reflects the shared scroll offset. It is not
    /// the song progress thumb above — the two measure different axes.
    scroll_track: Box4,
    scroll_thumb: Box4,
}

impl Chrome {
    /// `scroll` is `(offset, min, max)` from the shared [`ScrollSurface`]. The
    /// lyric scrollbar is the only chrome that depends on it; passing it here
    /// keeps the painted thumb and the draggable rect the same rectangle.
    fn build(m: &Metrics, scroll: (f64, f64, f64)) -> Self {
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
struct PlanRow {
    hover_region: Option<(usize, Box4)>,
    lines: Arc<RowLines>,
    bounds: Box4,
    align: paint::LineAlign,
    light: RowLight,
    translation_alpha: f32,
    roman_alpha: f32,
    scale: f32,
    wrapper: paint::WrapperTransform,
    blur: f32,
    top: f32,
}

/// Interlude three-dot indicator ready to paint.
struct PlanInterlude {
    x: f32,
    y: f32,
    dot_diameter: f32,
    dot_gap: f32,
    scale: f32,
    opacity: f32,
    dot_opacities: [f32; 3],
}

/// Bottom credits line ready to paint.
struct PlanBottom {
    lines: Arc<super::rows::BottomLines>,
    x_base: f32,
    wrap_width: f32,
    align: paint::LineAlign,
    top: f32,
    alpha: f32,
    blur: f32,
}

/// One glass panel ready to paint.
struct PlanPanel {
    content_alpha: f32,
    surface_alpha: f32,
    kind: PanelKind,
    rect: GlassRect,
    surface: CustomSurfaceHandle,
    snapshot: CustomSurfaceHandle,
    hook: PaintHook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelKind {
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
struct PickerEntry {
    title: String,
    subtitle: String,
    match_badge: String,
    timing_badge: Option<&'static str>,
    has_translation: bool,
    smart_translation: bool,
    smart_translation_key: String,
    path: PathBuf,
    source: String,
    id: String,
    candidate_index: usize,
    selected: bool,
}
const VERSION_HEADER: f32 = 42.0;
const VERSION_ROW: f32 = 64.0;

fn picker_smart_btn_rect(row: Box4) -> Box4 {
    Box4::new(row.right() - 96.0, row.y + 30.0, 90.0, 22.0)
}

fn picker_refresh_rect(panel: GlassRect) -> Box4 {
    Box4::new(panel.left as f32 + panel.width as f32 - 36.0, panel.top as f32 + 7.0, 26.0, 26.0)
}

fn picker_import_rect(panel: GlassRect) -> Box4 {
    Box4::new(panel.left as f32 + panel.width as f32 - 66.0, panel.top as f32 + 7.0, 26.0, 26.0)
}

fn album_return_rect() -> Box4 {
    Box4::new(10.0, 10.0, 22.0, 22.0)
}

struct Plan {
    settings_only:bool,
    immersion: bool,
    metrics: Metrics,
    chrome: Chrome,
    bg: Option<CustomSurfaceHandle>,
    cover: Option<CustomSurfaceHandle>,
    motion: Option<CustomSurfaceHandle>,
    motion_alpha: f32,
    rows: Vec<PlanRow>,
    lyric_hover: Option<(Box4, f32)>,
    interlude: Option<PlanInterlude>,
    bottom: Option<PlanBottom>,
    panels: Vec<PlanPanel>,
    position_ms: f64,
    duration_ms: f64,
    play_morph: f32,
    translation: bool,
    badge: Option<String>,
    status: String,
    track_title: String,
    track_artist: String,
    cursor: Option<(f32, f32)>,
    drag_thumb: Option<(f32, f32)>,
    scroll_glass: Option<PlanPanel>,
    scroll_line: Option<(Box4, f32)>,
    slider_lens: Option<PlanPanel>,
    button_lens: Option<(Hot, PlanPanel, Box4, Feedback)>,
    window_controls_alpha: f32,
    mode_well_rect: Box4,
    mode_lens_rect: Box4,
    mode_swell: f32,
    mode_lens_glass: Option<PlanPanel>,
    active_mode: u8,
    feedback: Vec<(Hot, Feedback)>,
    volume_enabled: bool,
    lyric_position_ms:f64,
    sync_enabled:bool,
    sync_label:String,
    volume_label: String,
    volume_available: bool,
    mode:Mode,
    fullscreen:bool,
    chrome_visible:bool,
    content_alpha: f32,
    chrome_alpha: f32,
    compact_chrome_alpha: f32,
    album_meta_alpha: f32,
    cover_scale: f32,
    cover_vis: f32,
    picker_start:usize,
    picker_kind: PickerKind,
    show_media_row: bool,
    picker_refresh_angle: f32,
    control_pending:bool,
    lyrics_visible: bool,
    lyrics_alpha: f32,
    smart_switch_fractions: Vec<(String, f32)>,
    cover_switch_fractions: [f32; 2],
    empty_notice: Option<String>,
    island: Option<(Box4, f32)>,
    island_glass_panel: Option<PlanPanel>,
    display: Display,
    settings_paint: Option<settings::Presentation>,
}

#[cfg(test)]
mod live_clock_tests {
    use super::*;

    #[::std::prelude::v1::test]
    fn live_now_ms_uses_unix_epoch_scale_aligned_with_smtc_timestamps() {
        let live_now = PlayerView::live_now_ms();
        assert!(
            live_now > 1_700_000_000_000.0,
            "live_now_ms must be Unix epoch ms so ProgressClock preserves (now - last_updated_unix_ms), got {live_now}"
        );
        #[cfg(windows)]
        {
            let smtc_now = oly_platform_win::smtc::now_ms() as f64;
            assert!(
                (live_now - smtc_now).abs() < 5_000.0,
                "live_now_ms ({live_now}) must stay in the same clock domain as smtc::now_ms ({smtc_now})"
            );
        }
    }
}

struct SettingsHost {parent:WeakEntity<PlayerView>,side:super::details::PanelSide,last_config:serde_json::Value}

#[derive(Clone)]
struct InlineSwitchDrag {
    key: String,
    track: Box4,
    origin: f32,
    fraction: f32,
    moved: bool,
}

const COVER_SWITCH_ROW: f32 = 44.0;

fn cover_switch_row_rect(panel_left: f32, panel_top: f32, panel_w: f32, i: u8) -> Box4 {
    Box4::new(
        panel_left + 8.0,
        panel_top + VERSION_HEADER + i as f32 * COVER_SWITCH_ROW,
        panel_w - 16.0,
        COVER_SWITCH_ROW - 4.0,
    )
}

fn cover_switch_track_rect(row: Box4) -> Box4 {
    Box4::new(row.right() - 42.0, row.y + (row.h - 19.0) * 0.5, 34.0, 19.0)
}

fn picker_smart_switch_rect(row: Box4) -> Box4 {
    let btn = picker_smart_btn_rect(row);
    Box4::new(btn.right() - 34.0, btn.y + 2.5, 34.0, 19.0)
}

fn window_caption_rects(win_w: f32, compact: bool) -> (Box4, [Box4; 3]) {
    if compact {
        let zone = Box4::new((win_w - 112.0).max(0.0), 0.0, 112.0, 48.0);
        let min_b = Box4::new(win_w - 84.0, 10.0, 22.0, 22.0);
        let fs_b = Box4::new(win_w - 58.0, 10.0, 22.0, 22.0);
        let close_b = Box4::new(win_w - 32.0, 10.0, 22.0, 22.0);
        (zone, [min_b, fs_b, close_b])
    } else {
        let zone = Box4::new((win_w - 160.0).max(0.0), 0.0, 160.0, 74.0);
        let min_b = Box4::new(win_w - 110.0, 14.0, 28.0, 28.0);
        let fs_b = Box4::new(win_w - 76.0, 14.0, 28.0, 28.0);
        let close_b = Box4::new(win_w - 42.0, 14.0, 28.0, 28.0);
        (zone, [min_b, fs_b, close_b])
    }
}

/// The player.
pub struct PlayerView {
    cfg: ViewConfig,
    settings_host:Option<SettingsHost>,
    settings_child:Option<AnyWindowHandle>,
    settings_child_view:Option<WeakEntity<PlayerView>>,
    pending_settings_window:Option<usize>,
    sync_settings_child_config: bool,
    pending_import_rx: Option<std::sync::mpsc::Receiver<Option<PathBuf>>>,
    native_background: Option<bool>,
    doc_auto_picked: bool,
    auto_pick_persisted: bool,
    qa_frame_sample: Option<(u64,f64)>,
    smart_translation_worker: Option<super::translation::TranslationWorker>,
    translation_signature: Option<(u64,String,serde_json::Value)>,
    translated_rows: Vec<super::translation::TranslationPatch>,
    search_config: Option<serde_json::Value>,
    pressed_candidate: Option<(String,String,usize)>,
    focus: FocusHandle,
    scene: Option<PlayerScene>,
    bg_surface: Option<CustomSurfaceHandle>,
    bg_size: (u32, u32),
    cover: Option<Cover>,
    cover_surface: Option<CustomSurfaceHandle>,
    cover_size: (u32, u32),
    motion_surface: Option<CustomSurfaceHandle>,
    motion_size: (u32, u32),

    shaper: Shaper,
    display: Display,
    settings: Settings,
    sync:SyncState,
    pending_mode:Option<u8>,
    switch_drag:Option<super::details::SwitchDrag>,
    segmented_drag: Option<(usize, &'static str, Box4, usize)>,
    pre_fullscreen_mode: u8,
    last_fullscreen: bool,
    window_controls_alpha: f32,
    pending_minimize: bool,
    pending_quit: bool,
    picker_start:usize,
    picker_capacity:usize,
    picker_kind: PickerKind,
    chrome_wake_ms:f64,
    cursor_idle_ms: f64,
    cursor_hide_pending: bool,
    browse_idle_ms: f64,
    lyrics_user_hidden: bool,
    lyrics_auto_hidden: bool,
    lyrics_user_force_shown: bool,
    lyrics_center: oly_core::scroll::Spring,
    lyrics_reveal_alpha: f32,
    staged_mode:Option<u8>,
    wide_size:Option<(f32,f32)>,
    last_window_bounds:Option<WindowBounds>,
    mode_transition:Option<super::transition::ModeTransition>,
    geometry_memory:super::transition::WindowGeometryMemory,
    volume_req_id:i64,
    volume_read_id:Option<i64>,
    volume_read_deadline_ms:Option<f64>,
    volume_cached:Option<(String,f64)>,
    pending_motion_path:Option<(u64,PathBuf)>,
    motion_debounce_deadline_ms:Option<(u64,f64)>,
    motion_dispatched:Option<(u64,String,String)>,
    motion_needs_clear:bool,
    motion_last_enabled:bool,
    motion_fade: f32,
    inline_switch_drag: Option<InlineSwitchDrag>,
    bg_cover_hash: Option<(usize, u64)>,
    prev_cover_rgba: Option<Vec<u8>>,
    cover_fade: f32,
    cover_blend_buf: Vec<u8>,
    cover_accent_hex: Option<u32>,
    island_wake_ms: f64,
    island_drop: f32,
    cover_gone_at_ms: Option<f64>,
    compact_chrome_alpha: f32,
    album_meta_alpha: f32,
    cover_scale_spring: oly_core::scroll::Spring,
    cover_vis_spring: oly_core::scroll::Spring,
    fonts: fonts::Registration,
    doc: Option<LyricDoc>,
    doc_provider:String,
    doc_identity:Option<(String,String)>,
    rows: Vec<Row>,
    lyric_timeline: super::playback::LyricTimeline,
    lines: Vec<oly_contracts::lyrics::AmllLine>,

    position_ms: f64,
    duration_ms: f64,
    playing: bool,
    source: Source,
    live: Option<crate::live::LiveSession>,
    cover_dirty: bool,
    live_poll_pending: bool,

    scroll: ScrollSurface,
    follow: model::FollowMotion,
    group_motion: Vec<model::RowMotion>,
    group_bg_slide: Vec<model::RowMotion>,
    group_bg_alpha: Vec<f32>,
    group_passed_alpha: Vec<f32>,
    main_scale: Vec<model::RowMotion>,
    bg_scale: Vec<model::RowMotion>,
    focus_controller: oly_core::timeline::FocusController,
    interlude_dots: oly_core::interlude::InterludeDots,
    bottom_text: String,
    bottom_motion: Option<model::RowMotion>,
    bottom_alpha: f32,
    bottom_target_alpha: f32,
    pos_y_params: oly_core::scroll::SpringParams,
    pending_seek: bool,
    seek_preview_ms: Option<f64>,
    prev_player_offset_ms: f64,
    prev_playing: bool,
    prev_viewport: (f32, f32),
    prev_display: Option<Display>,
    browse_state: Option<(usize, bool, bool)>,
    thumb_elastic: super::elastic::Elastic,
    thumb_glass: Option<GlassPanel>,
    tool: ToolBarState,
    volume: VolumePanel,

    chrome: Chrome,
    hot: Vec<(Hot, Box4)>,
    panel_bounds: Vec<Box4>,
    pressed: Option<Hot>,
    feedback: Vec<(Hot, Feedback)>,
    elastic: Vec<(Hot,Elastic)>,
    press_gesture: PressGesture,
    drag_thumb: Option<(f32, f64)>,
    picked: usize,
    /// Whether the version list is showing inside the more panel. It is its own
    /// flag: a selected candidate index and "the list is open" are different
    /// facts, and overloading one integer made the first row click close the
    /// list it had just opened.
    picker_open: bool,

    panel_motion: [PanelMotion;4],
    panel_elastics: [Elastic; 5],
    panel_drag_targets: Vec<(usize, Box4)>,
    drag_panel: Option<usize>,
    panel_press_candidate: Option<usize>,
    picker_refresh_angle: f32,
    picker_refresh_spinning: bool,
    picker_refresh_active: bool,
    picker_locked_rows: Option<usize>,
    translation_motion: Feedback,
    smart_switch_springs: std::collections::HashMap<String, oly_core::scroll::Spring>,
    lyric_hover: super::interaction::LyricHover,
    mode_lens: chrome::SegmentedLens,
    more_glass: Option<GlassPanel>,
    volume_glass: Option<GlassPanel>,
    picker_glass: Option<GlassPanel>,
    settings_glass: Option<GlassPanel>,
    slider_lens_glass: Option<GlassPanel>,
    button_lens_glass: Option<GlassPanel>,
    segmented_lens_glass: Option<GlassPanel>,
    tab_lens_glass: Option<GlassPanel>,
    dropdown_glass: Option<GlassPanel>,
    island_glass: Option<GlassPanel>,
    play_morph: oly_core::scroll::Spring,

    /// The scripted interaction pass. `None` on every normal launch; the QA
    /// harness opts in so a recording can exercise the real entry points.
    scenario: Option<Scenario>,

    frames: u64,
    started: Instant,
    last_ms: f64,
    cadence: PresentCadence,
    cursor: Option<(f32, f32)>,
    reported: bool,
    /// Frames that had at least one glass panel in the paint plan. A zero here
    /// with a non-zero `acts` says the panels never reached the window even
    /// though the script opened them.
    panels_painted: u64,
    /// Script steps that actually ran, so a report can separate "the view
    /// never painted" from "the script never fired".
    acts: u64,
    /// Whether the offscreen capture has been written. Setting it is what ends
    /// a capture run, so a failed write still stops instead of looping.
    captured: bool,
    /// Set on the pass that pins the clock to the capture time. The next pass
    /// then captures the scene that pass painted, so the PNG is the frame at
    /// the requested media time rather than one frame later.
    capture_ready: bool,
}

impl PlayerView {
    pub fn new(cfg: ViewConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        if cx.has_global::<gpui_kit::component::Theme>() {
            let theme = gpui_kit::component::Theme::global_mut(cx);
            theme.colors.background = gpui_kit::transparent_black();
            theme.tokens.background = gpui_kit::transparent_black().into();
        }
        let registration = if cfg.settings_only {
            fonts::Registration {
                all: cx.text_system().all_font_names(),
                bundled: Vec::new(),
            }
        } else {
            match fonts::register(&cx.text_system()) {
                Ok(registration) => registration,
                Err(error) => {
                    log::warn!("font registration failed: {error:#}");
                    fonts::Registration {
                        all: cx.text_system().all_font_names(),
                        bundled: Vec::new(),
                    }
                }
            }
        };
        let (latin, cjk) = fonts::requested_families();
        if !fonts::has_family(&registration.all, latin) {
            log::warn!("{latin:?} is not registered; text will fall back");
        }
        log::info!(
            "fonts registered ({}): CJK {}",
            registration.all.len(),
            fonts::cjk_status(&registration)
        );
        let cjk_present = fonts::has_family(&registration.all, cjk);
        if !cjk_present {
            log::warn!(
                "{cjk:?} is not registered; CJK glyphs fall back to a platform face. \
                 The experiment records this instead of shipping a substitute."
            );
        }

        let shaper = Shaper::new(cx.text_system().clone(), 128);
        let scenario = cfg.scenario.as_deref().and_then(Scenario::named);
        let sync=SyncState::new(cfg.source==Source::Live);
        let start_idle = cfg.source == Source::Live && !cfg.settings_only;
        let mut this = Self {settings_host:None,settings_child:None,settings_child_view:None,pending_settings_window:None,sync_settings_child_config:false,pending_import_rx:None,native_background:None,doc_auto_picked:false,auto_pick_persisted:false,qa_frame_sample:None,smart_translation_worker:None,translation_signature:None,translated_rows:Vec::new(),search_config:None,pressed_candidate:None,
            cfg,
            focus: cx.focus_handle(),
            scene: None,
            bg_surface: None,
            bg_size: (0, 0),
            cover: None,
            cover_surface: None,
            cover_size: (0, 0),
            motion_surface: None,
            motion_size: (0, 0),
            shaper,
            display: Display::default(),
            settings:Settings::default(),
            sync,pending_mode:None,switch_drag:None,segmented_drag:None,pre_fullscreen_mode:2,last_fullscreen:false,window_controls_alpha:0.0,pending_minimize:false,pending_quit:false,picker_start:0,picker_capacity:4,picker_kind:PickerKind::Lyrics,chrome_wake_ms:0.,cursor_idle_ms:0.,cursor_hide_pending:false,browse_idle_ms:0.,lyrics_user_hidden:false,lyrics_auto_hidden:start_idle,lyrics_user_force_shown:false,lyrics_center:{let mut s=oly_core::scroll::Spring::new(if start_idle {1.0} else {0.0});s.update_params(oly_core::scroll::SpringParams::new(140.0,22.0));s},lyrics_reveal_alpha:if start_idle {0.0} else {1.0},staged_mode:None,wide_size:None,last_window_bounds:None,
            mode_transition:None,geometry_memory:Default::default(),
            volume_req_id:0,volume_read_id:None,volume_read_deadline_ms:None,volume_cached:None,
            pending_motion_path:None,motion_debounce_deadline_ms:None,motion_dispatched:None,motion_needs_clear:false,motion_last_enabled:true,motion_fade:0.0,inline_switch_drag:None,bg_cover_hash:None,prev_cover_rgba:None,cover_fade:1.0,cover_blend_buf:Vec::new(),cover_accent_hex:None,island_wake_ms:-10_000.0,island_drop:0.0,cover_gone_at_ms:None,compact_chrome_alpha:1.0,album_meta_alpha:1.0,
            cover_scale_spring: {
                let mut s = oly_core::scroll::Spring::new(1.0);
                s.update_params(oly_core::scroll::SpringParams::new(260.0, 24.0));
                s
            },
            cover_vis_spring: {
                let mut s = oly_core::scroll::Spring::new(1.0);
                s.update_params(oly_core::scroll::SpringParams::new(62.0, 15.8));
                s
            },
            fonts: registration,
            doc: None,
            doc_provider:String::new(),doc_identity:None,
            rows: Vec::new(),
            lines: Vec::new(),
            position_ms: 0.0,
            duration_ms: 0.0,
            playing: true,
            source: Source::Replay,
            live: None,
            cover_dirty: false,
            live_poll_pending: false,
            scroll: ScrollSurface::new(),
            follow: model::FollowMotion::default(),
            lyric_timeline: Default::default(),
            group_motion: Vec::new(),
            group_bg_slide: Vec::new(),
            group_bg_alpha: Vec::new(),
            group_passed_alpha: Vec::new(),
            main_scale: Vec::new(),
            bg_scale: Vec::new(),
            focus_controller: oly_core::timeline::FocusController::new(),
            interlude_dots: oly_core::interlude::InterludeDots::new(),
            bottom_text: String::new(),
            bottom_motion: None,
            bottom_alpha: 0.2,
            bottom_target_alpha: 0.2,
            pos_y_params: Display::default().spring_params(),
            pending_seek: false,
            seek_preview_ms: None,
            prev_player_offset_ms: 0.0,
            prev_playing: true,
            prev_viewport: (0.0, 0.0),
            prev_display: None,
            browse_state: None,
            thumb_elastic: Default::default(),
            thumb_glass: None,
            tool: ToolBarState::new(Mode::Window),
            volume: VolumePanel::open("", None, VolumeSource::Internal),
            chrome: Chrome::default(),
            hot: Vec::new(),
            panel_bounds: Vec::new(),
            pressed: None,
            feedback: Vec::new(),
            elastic: Vec::new(),
            press_gesture: PressGesture::default(),
            drag_thumb: None,
            picked: 0,
            picker_open: false,
            panel_motion: std::array::from_fn(|_| PanelMotion::default()),
            panel_elastics: std::array::from_fn(|_| Elastic::default()),
            panel_drag_targets: Vec::new(),
            drag_panel: None,
            panel_press_candidate: None,
            picker_refresh_angle: 0.0,
            picker_refresh_spinning: false,
            picker_refresh_active: false,
            picker_locked_rows: None,
            lyric_hover: Default::default(),
            translation_motion: Feedback { hover: 1.0, press: 0.0, ..Default::default() },
            smart_switch_springs: std::collections::HashMap::new(),
            mode_lens: chrome::SegmentedLens::default(),
            more_glass: None,
            volume_glass: None,
            picker_glass: None,
            settings_glass: None,
            slider_lens_glass: None,
            button_lens_glass: None,
            segmented_lens_glass: None,
            tab_lens_glass: None,
            dropdown_glass: None,
            island_glass: None,
            play_morph: {
                super::interaction::state_icon_spring(1.0)
            },
            scenario,
            frames: 0,
            started: Instant::now(),
            last_ms: 0.0,
            cadence: PresentCadence::new(),
            cursor: None,
            reported: false,
            panels_painted: 0,
            acts: 0,
            captured: false,
            capture_ready: false,
        };

        if this.cfg.start_mini {
            this.tool = ToolBarState::new(Mode::Mini);
            this.wide_size = Some((1272.0, 843.0));
        }
        this.source = this.cfg.source.clone();
        if this.source == Source::Live {
            match crate::live::LiveSession::start() {
                Ok(live) => {this.settings=Settings::with_store(live.config.clone());this.live = Some(live);},
                Err(error) => log::error!("live startup failed: {error:#}"),
            }
            this.playing = false;
        }
        this.sync_profile_display();
        this.pos_y_params = this.display.spring_params();
        this.prev_playing = this.playing;
        this.play_morph = {
            super::interaction::state_icon_spring(if this.playing { 1.0 } else { 0.0 })
        };
        this.cover_scale_spring.set_position(if this.playing { 1.0 } else { 0.74 });
        this.cover_vis_spring.set_position(if this.display.show_cover { 1.0 } else { 0.0 });
        this.interlude_dots.set_playing(this.playing);
        if !this.cfg.settings_only {
            if let Some(device) = window.gpu_device_handle().ok() {
                this.scene = Some(PlayerScene::new(device.0, device.1));
            } else {
                log::error!("no GPU device handle: the background and motion cover stay dark");
            }
        }

        if let Some(path) = this.cfg.cover.clone() {
            match decode_cover_file(&path) {
                Ok(cover) => this.install_cover(&cover),
                Err(error) => log::warn!("cover {}: {error:#}", path.display()),
            }
        }
        if let Some(path) = this.cfg.motion.clone() {
            if let Some(scene) = this.scene.as_mut() {
                match scene.open_motion(&path) {
                    Ok(()) => log::info!("motion cover opened: {}", path.display()),
                    Err(error) => log::warn!("motion cover {}: {error:#}", path.display()),
                }
            }
        }
        if let Some(path) = this.cfg.fixture.clone() {
            this.load_document(&path);
        }
        if this.rows.is_empty() {
            log::warn!(
                "no lyric document: run with --fixture <file> to replay one. \
                 The player still draws its background, chrome and panels."
            );
        }
        let owner=cx.weak_entity();
        window.on_window_should_close(cx,move|_,cx|{
            let Some(view)=owner.upgrade() else{return true;};
            if view.read(cx).settings_host.is_some(){view.update(cx,|v,cx|{v.settings.close_choice();v.settings.open=false;cx.notify();});return false;}
            let child=view.read(cx).settings_child;
            if let Some(child)=child{let _=child.update(cx,|_,w,_|w.remove_window());}
            true
        });
        cx.observe_window_activation(window,|view,w,cx|{
            if view.settings_host.is_some() && !w.is_window_active(){view.settings.close_choice();view.settings.open=false;cx.notify();}
        }).detach();
        this
    }

    fn active_profile_for(&self) -> &'static str {
        if matches!(self.tool.mode(), Some(Mode::Mini | Mode::Lyrics)) {
            "mini"
        } else if self.settings.config["display"]["showCover"]
            .as_bool()
            .unwrap_or(true)
        {
            "cover"
        } else {
            "nocover"
        }
    }

    fn sync_profile_display(&mut self) {
        if self.settings_host.is_none() {
            self.settings.profile = self.active_profile_for();
            let show_cover = self.settings.config["display"]["showCover"].as_bool().unwrap_or(true);
            self.settings.display_mode_label = Some(match self.tool.mode() {
                Some(Mode::Lyrics) => "歌词模式",
                Some(Mode::Mini) => "迷你模式",
                Some(Mode::Album) => "封面模式",
                _ if show_cover => "窗口模式",
                _ => "无封面模式",
            });
        }
        let profile = self.settings.active_profile();
        self.display = Display::from_config_for(&self.settings.config, profile);
        let player_offset = self.player_base_offset_ms();
        if (player_offset - self.prev_player_offset_ms).abs() > 1e-3 {
            self.prev_player_offset_ms = player_offset;
            self.pending_seek = true;
        }
    }

    /// Loads one fixture and installs it as the replayed document.
    pub fn load_document(&mut self, path: &std::path::Path) {
        match load_fixture(path) {
            Ok((doc,provider)) => {self.doc_provider=provider;self.install_document(doc, true);},
            Err(error) => log::error!("fixture {}: {error:#}", path.display()),
        }
    }

    fn install_document(&mut self, doc: LyricDoc, replay: bool) {
        if let Some(w)=&self.smart_translation_worker{w.cancel();}
        self.translation_signature=None;self.translated_rows.clear();
        self.lines = super::playback::prepare_lines(&doc);
        self.rows = model::rows_from(&self.lines);
        for row in &mut self.rows {
            row.word_timed = doc.lines.get(row.group).is_some_and(|line| {
                let words = if row.is_bg && !line.bg_words.is_empty() { &line.bg_words } else { &line.words };
                words.iter().any(|w| w.start.is_some() && w.end.is_some())
            });
        }
        self.bottom_text = oly_contracts::credit_names(&doc);
        self.lyric_timeline = super::playback::LyricTimeline::new(&self.rows);
        self.follow = model::FollowMotion::default();
        self.focus_controller.reset();
        self.interlude_dots = oly_core::interlude::InterludeDots::new();
        self.interlude_dots.set_playing(self.playing);
        self.bottom_motion = None;
        self.bottom_alpha = 0.2;
        self.bottom_target_alpha = 0.2;
        self.browse_state = None;
        self.group_motion.clear();
        self.group_bg_slide.clear();
        self.group_bg_alpha.clear();
        self.group_passed_alpha.clear();
        self.main_scale.clear();
        self.bg_scale.clear();
        self.pos_y_params = self.display.spring_params();
        self.pending_seek = self.position_ms > 0.0;
        self.scroll = ScrollSurface::new();
        if replay {
            self.duration_ms = if doc.duration_sec > 0.0 { doc.duration_sec * 1000.0 }
                else { doc.lines.last().and_then(|l| l.end).unwrap_or(180_000) as f64 };
            self.position_ms = self.rows.first().map(|r| r.start_ms.max(0.0)).unwrap_or(0.0);
        }
        self.lyrics_auto_hidden = doc.instrumental;
        self.doc = Some(doc);
    }

    fn lyrics_hidden(&self) -> bool {
        self.lyrics_user_hidden || (self.lyrics_auto_hidden && !self.lyrics_user_force_shown)
    }

    fn record_cursor_activity(&mut self, now: f64) {
        self.cursor_idle_ms = now;
    }

    fn next_volume_req_id(&mut self) -> i64 {
        self.volume_req_id = self.volume_req_id.wrapping_add(1).max(1);
        self.volume_req_id
    }

    fn send_volume_write(&mut self, write: VolumeWrite) {
        if write.app.trim().is_empty() {
            return;
        }
        let app_key = write.app.clone();
        self.volume_cached = Some((app_key, write.value));
        if self.source != Source::Live {
            return;
        }
        let request_id = self.next_volume_req_id();
        let mode = match write.source {
            VolumeSource::Internal => "player".to_string(),
            VolumeSource::AppMixer => "mixer".to_string(),
        };
        let value = (write.value / 100.0).clamp(0.0, 1.0) as f32;
        if let Some(live) = self.live.as_ref() {
            let _ = live.host.dispatch(crate::commands::HostCommand::VolumeSet {
                app: write.app,
                value,
                mode,
                request_id,
            });
        }
    }

    fn close_volume_panel(&mut self) {
        self.volume_read_id = None;
        self.volume_read_deadline_ms = None;
        if self.volume.status().is_editable() && !self.volume.app().trim().is_empty() {
            self.volume_cached = Some((self.volume.app().to_string(), self.volume.value()));
        }
        if let Some(write) = self.volume.flush_on_close() {
            self.send_volume_write(write);
        }
        self.tool.set_volume_open(false);
    }

    fn open_live_volume(&mut self, mono_now: f64) {
        let app = self
            .live
            .as_ref()
            .and_then(|l| l.model.packet.as_ref())
            .filter(|p| !p.idle && !p.app.trim().is_empty())
            .map(|p| p.app.clone())
            .unwrap_or_default();
        if app.is_empty() {
            self.volume_read_id = None;
            self.volume_read_deadline_ms = None;
            self.volume = VolumePanel::open("", None, VolumeSource::Internal);
            self.volume.on_error();
            return;
        }
        let cached = self
            .volume_cached
            .as_ref()
            .filter(|(a, _)| a == &app)
            .map(|(_, v)| *v);
        self.volume = VolumePanel::open(&app, cached, VolumeSource::Internal);
        let req_id = self.next_volume_req_id();
        self.volume_read_id = Some(req_id);
        self.volume_read_deadline_ms = Some(mono_now + 4000.0);
        if let Some(live) = self.live.as_ref() {
            let _ = live.host.dispatch(crate::commands::HostCommand::VolumeGet {
                app,
                mode: "auto".into(),
                request_id: req_id,
            });
        }
    }

    fn clear_motion_surface(&mut self, window: &mut Window) {
        if let Some(scene) = self.scene.as_mut() {
            scene.clear_motion();
        }
        if let Some(old) = self.motion_surface.take() {
            window.destroy_custom_surface(old);
        }
        self.motion_size = (0, 0);
        self.pending_motion_path = None;
    }

    fn poll_live(&mut self, mono_now: f64, live_now: f64) {
        use crate::commands::UiEvent;
        let mut vol_events = Vec::new();
        let mut reset = false;
        let mut idle_reset = false;
        let (prev_gen, after_gen, active_app, track_meta, answer, artwork, motion_answer) = {
            let Some(live) = self.live.as_mut() else { return; };
            let prev_gen = live.model.generation();
            let events = live.inbox.lock().unwrap_or_else(|e| e.into_inner()).drain();
            for event in events {
                match event {
                    UiEvent::State(packet) => {
                        if live.model.packet.as_ref().map(|p|p.is_playing()) != Some(packet.is_playing()) { log::info!("[ui-state] playing={}",packet.is_playing()); }
                        live.host.follow(&packet.app);
                        let idle = packet.is_idle_state();
                        if let Some(track) = live.model.feed(packet, live_now) {
                            live.search.request(live.model.generation(), track.clone(), None);
                            live.track = Some(track);
                            live.candidates.clear();
                            live.status = "正在搜索歌词…".into();
                            reset = true;
                            idle_reset = false;
                        } else if idle {
                            live.search.cancel(); live.track = None; live.candidates.clear();
                            live.status = "等待播放器…".into(); reset = true; idle_reset = true;
                        }
                    }
                    UiEvent::Idle => {
                        live.model.idle(); live.search.cancel(); live.track = None;
                        live.candidates.clear(); live.status = "等待播放器…".into(); reset = true; idle_reset = true;
                    }
                    UiEvent::Control(answer) => {
                        if let Some(accepted) = live.model.control_result(answer.request_id, answer.accepted) {
                            live.status = if accepted { String::new() } else { "播放器未响应此操作".into() };
                        }
                    }
                    UiEvent::Volume(ev) => vol_events.push(ev),
                    UiEvent::Sessions(sessions) => {
                        live.media_sessions = sessions;
                    }
                }
            }
            if self.picker_open
                && self.picker_kind == PickerKind::Media
                && live.media_sessions.len() <= 1
            {
                self.picker_open = false;
                self.picker_kind = PickerKind::Lyrics;
            }
            live.model.tick(live_now);
            self.position_ms = live.model.position(live_now);
            self.playing = live.model.effective_playing();
            self.duration_ms = live.model.packet.as_ref().map(|p| p.duration_ms).unwrap_or(0.0);
            let answer = live.search.take().filter(|a| live.model.accepts(a.generation));
            live.request_artwork();
            let after_gen = live.model.generation();
            let artwork = live.artwork.take().filter(|(g, _)| *g == after_gen);
            let motion_answer = live.motion.take().filter(|a| a.generation == after_gen);
            let active_app = live
                .model
                .packet
                .as_ref()
                .filter(|p| !p.idle)
                .map(|p| p.app.clone())
                .unwrap_or_default();
            let track_meta = live
                .model
                .packet
                .as_ref()
                .filter(|p| !p.idle && !p.title.trim().is_empty())
                .map(|p| {
                    (
                        after_gen,
                        p.track_key(),
                        p.title.clone(),
                        p.artist.clone(),
                        p.album.trim().to_string(),
                    )
                });
            (
                prev_gen,
                after_gen,
                active_app,
                track_meta,
                answer,
                artwork,
                motion_answer,
            )
        };
        if self.settings.active_player != active_app {
            self.settings.active_player = active_app.clone();
            let player_offset = self.player_base_offset_ms();
            if (player_offset - self.prev_player_offset_ms).abs() > 1e-3 {
                self.prev_player_offset_ms = player_offset;
                self.pending_seek = true;
            }
        }
        if reset {
            self.picker_refresh_active = false;
            self.picker_locked_rows = None;
            self.doc = None; self.doc_provider.clear();self.doc_identity=None; self.doc_auto_picked=false; self.auto_pick_persisted=false; self.lines.clear(); self.rows.clear();
            self.lyrics_auto_hidden = idle_reset;
            self.lyrics_user_force_shown = false;
            self.bottom_text.clear(); self.focus_controller.reset();
            self.interlude_dots.clear_interlude(true);
            self.bottom_motion = None; self.bottom_alpha = 0.2; self.bottom_target_alpha = 0.2;
            self.browse_state = None;
            self.scroll = ScrollSurface::new(); self.follow = model::FollowMotion::default();
            self.group_motion.clear(); self.group_bg_slide.clear(); self.group_bg_alpha.clear(); self.group_passed_alpha.clear();
            self.main_scale.clear(); self.bg_scale.clear(); self.picked = usize::MAX;
        }
        if let Some(answer) = answer {
            let defer_refresh_update = self.picker_refresh_active && answer.searching;
            if !answer.searching {
                self.picker_refresh_active = false;
                self.picker_locked_rows = None;
            }
            if !defer_refresh_update {
                let no_candidates = answer.candidates.is_empty()
                    && self.live.as_ref().is_none_or(|l| l.candidates.is_empty());
                if let Some(live) = self.live.as_mut() {
                    if !answer.manual {
                        if !answer.candidates.is_empty() || !answer.searching {
                            live.candidates = answer.candidates;
                        }
                    } else if let Some((source, id)) = &answer.selected {
                        if !live.candidates.iter().any(|c| &c.source == source && &c.id == id) {
                            if let Some(kept) = answer
                                .candidates
                                .into_iter()
                                .find(|c| &c.source == source && &c.id == id)
                            {
                                live.candidates.push(kept);
                            }
                        }
                    }
                    live.status = answer.error.clone();
                }
                if let Some(doc) = answer.doc {
                    log::info!("live lyrics installed: provider={} selected={:?} rows={}",answer.provider,answer.selected,doc.lines.len());
                    self.picked = self
                        .live
                        .as_ref()
                        .and_then(|live| {
                            answer.selected.as_ref().and_then(|(source, id)| {
                                live.candidates.iter().position(|c| &c.source == source && &c.id == id)
                            })
                        })
                        .unwrap_or(usize::MAX);
                    let unchanged=self.doc_identity.is_some() && self.doc_identity==answer.selected;
                    self.doc_auto_picked = answer.auto_picked;
                    if !unchanged {
                        self.auto_pick_persisted = false;
                    }
                    self.doc_identity=answer.selected;
                    self.doc_provider=answer.provider;
                    self.lyrics_auto_hidden = doc.instrumental;
                    if !unchanged{self.install_document(doc, false);}
                } else if no_candidates
                    || answer.error == "未找到匹配歌词"
                    || !any_lyric_source_enabled(&self.settings.config)
                {
                    self.lyrics_auto_hidden = true;
                }
            }
        }
        if self.playing
            && self.doc.is_some()
            && !self.auto_pick_persisted
            && self.duration_ms > 5_000.0
        {
            if let (Some(live), Some((source, id))) = (self.live.as_ref(), self.doc_identity.as_ref()) {
                let media_pos = live.model.media_position(live_now);
                if media_pos >= self.duration_ms * 0.5 {
                    if let Some(track) = live.track.clone() {
                        live.search.promote_pick(track, source.clone(), id.clone());
                        self.auto_pick_persisted = true;
                        self.doc_auto_picked = false;
                    }
                }
            }
        }
        if let Some((_, Some(cover))) = artwork {
            self.install_cover(&cover);
        }
        if track_meta.is_none() && (self.cover.is_some() || self.bg_cover_hash.is_some()) {
            self.cover = None;
            self.cover_accent_hex = None;
            self.bg_cover_hash = None;
            self.prev_cover_rgba = None;
            self.cover_blend_buf = Vec::new();
            self.cover_fade = 1.0;
            self.cover_dirty = true;
            if let Some(scene) = self.scene.as_mut() {
                scene.clear_cover();
            }
        }
        if (after_gen != prev_gen || (reset && !idle_reset)) && track_meta.is_some() {
            self.island_wake_ms = self.now_ms();
        }
        if after_gen != prev_gen || track_meta.is_none() {
            if let Some(live) = self.live.as_ref() {
                live.motion.cancel();
            }
            self.pending_motion_path = None;
            self.motion_debounce_deadline_ms = None;
            self.motion_dispatched = None;
            self.motion_fade = 0.0;
            self.motion_needs_clear = true;
        } else if !self.display.motion_cover && self.motion_last_enabled {
            if let Some(live) = self.live.as_ref() {
                live.motion.cancel();
            }
            self.pending_motion_path = None;
            self.motion_debounce_deadline_ms = None;
            self.motion_dispatched = None;
        }
        self.motion_last_enabled = self.display.motion_cover;
        if self.display.motion_cover {
            if let Some((generation, tk, title, artist, album)) = track_meta {
                let target_key = (generation, tk.clone(), album.clone());
                if self.motion_dispatched.as_ref() != Some(&target_key) {
                    match self.motion_debounce_deadline_ms {
                        Some((g, deadline)) if g == generation => {
                            if mono_now >= deadline {
                                self.motion_debounce_deadline_ms = None;
                                self.motion_dispatched = Some(target_key);
                                if let Some(live) = self.live.as_ref() {
                                    live.motion.request(
                                        generation,
                                        tk,
                                        title,
                                        artist,
                                        album,
                                        true,
                                        self.display.motion_cache_mb,
                                    );
                                }
                            }
                        }
                        _ => {
                            self.motion_debounce_deadline_ms = Some((generation, mono_now + 700.0));
                        }
                    }
                }
                if let Some(ans) = motion_answer {
                    if let Some(path) = ans.file {
                        self.pending_motion_path = Some((ans.generation, path));
                    }
                }
            }
        }
        if let Some(rx) = self.pending_import_rx.take() {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.import_lyric_file(&path, false);
                }
                Ok(None) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.pending_import_rx = Some(rx);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if self.tool.panels().volume_open {
            if !self.volume.app().is_empty() && active_app != self.volume.app() {
                self.close_volume_panel();
            } else {
                for ev in vol_events {
                    if self.volume_read_id == Some(ev.request_id) {
                        self.volume_read_id = None;
                        self.volume_read_deadline_ms = None;
                        let src = if ev.mode == "player" {
                            VolumeSource::Internal
                        } else {
                            VolumeSource::AppMixer
                        };
                        if ev.available {
                            if let Some(frac) = ev.value {
                                let pct = (f64::from(frac) * 100.0).clamp(0.0, 100.0);
                                self.volume.on_read(Some(pct), src);
                                if !ev.app.trim().is_empty() {
                                    self.volume_cached = Some((ev.app, pct));
                                }
                            } else {
                                self.volume.on_error();
                            }
                        } else {
                            self.volume.on_error();
                        }
                    }
                }
                if let Some(deadline) = self.volume_read_deadline_ms {
                    if mono_now >= deadline {
                        self.volume_read_id = None;
                        self.volume_read_deadline_ms = None;
                        self.volume.on_timeout();
                    }
                }
                if let Some(write) = self.volume.tick(mono_now) {
                    self.send_volume_write(write);
                }
            }
        }
    }

    fn cover_signature(cover: &Cover) -> (usize, u64) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        cover.width.hash(&mut h);
        cover.height.hash(&mut h);
        cover.rgba.pixels.hash(&mut h);
        (cover.rgba.pixels.len(), h.finish())
    }

    fn install_cover(&mut self, cover: &Cover) {
        let sig = Self::cover_signature(cover);
        if self.bg_cover_hash == Some(sig) {
            return;
        }
        self.prev_cover_rgba = self.cover.as_ref().and_then(|old| {
            if old.width == cover.width && old.height == cover.height {
                Some(old.rgba.pixels.clone())
            } else {
                image::RgbaImage::from_raw(old.width, old.height, old.rgba.pixels.clone()).map(
                    |img| {
                        image::imageops::resize(
                            &img,
                            cover.width,
                            cover.height,
                            image::imageops::FilterType::Triangle,
                        )
                        .into_raw()
                    },
                )
            }
        });
        self.cover_fade = if self.prev_cover_rgba.is_some() { 0.0 } else { 1.0 };
        self.bg_cover_hash = Some(sig);
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        if let Err(error) = scene.set_cover(cover) {
            log::warn!("background artwork: {error:#}");
        }
        self.cover_dirty = true;
        self.cover_accent_hex = model::extract_cover_accent(&cover.rgba.pixels, cover.width, cover.height);
        self.cover = Some(Cover {
            width: cover.width,
            height: cover.height,
            rgba: cover.rgba.clone(),
        });
    }

    /// Re-registers the background surface when the renderer rebuilt its
    /// target. Handles stay valid until the texture behind them is replaced,
    /// which `resize` does.
    fn sync_background(&mut self, window: &mut Window, scale: f32, immersion: bool) {
        if !immersion {
            if let Some(old) = self.bg_surface.take() {
                window.destroy_custom_surface(old);
                self.bg_size = (0, 0);
            }
            return;
        }
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let Some(texture) = scene.background_texture() else {
            return;
        };
        let size = scene.background_size();
        if self.bg_surface.is_none() || self.bg_size != size {
            if let Some(old) = self.bg_surface.take() {
                window.destroy_custom_surface(old);
            }
            match window.register_gpu_surface(texture) {
                Ok(handle) => {
                    self.bg_surface = Some(handle);
                    self.bg_size = size;
                    log::info!("background surface registered at {size:?}");
                }
                Err(error) => log::warn!("background surface: {error:#}"),
            }
        }
        let _ = scale;
    }

    fn sync_motion(&mut self, window: &mut Window) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        if !scene.has_motion() {
            if let Some(old) = self.motion_surface.take() {
                window.destroy_custom_surface(old);
            }
            self.motion_size = (0, 0);
            return;
        }
        let Some(texture) = scene.motion_texture() else {
            return;
        };
        let size = scene.motion_size();
        if self.motion_surface.is_none() || self.motion_size != size {
            if let Some(old) = self.motion_surface.take() {
                window.destroy_custom_surface(old);
            }
            match window.register_gpu_surface(&texture) {
                Ok(handle) => {
                    self.motion_surface = Some(handle);
                    self.motion_size = size;
                }
                Err(error) => log::warn!("motion surface: {error:#}"),
            }
        }
    }

    fn sync_cover_surface(&mut self, window: &mut Window, dt_ms: f64) {
        let Some(cover) = self.cover.as_ref() else {
            if self.cover_dirty {
                if let Some(old) = self.cover_surface.take() {
                    window.destroy_custom_surface(old);
                }
                self.cover_size = (0, 0);
                self.cover_dirty = false;
            }
            return;
        };
        let mut need_upload = self.cover_dirty;
        self.cover_dirty = false;
        if self.cover_fade < 1.0 {
            self.cover_fade = (self.cover_fade + (dt_ms.clamp(0.0, 100.0) as f32 / 550.0)).min(1.0);
            need_upload = true;
        }
        if self.cover_surface.is_some() && self.cover_size != (cover.width, cover.height) {
            if let Some(old) = self.cover_surface.take() {
                window.destroy_custom_surface(old);
            }
            self.cover_size = (0, 0);
        }
        if self.cover_surface.is_some() && !need_upload {
            if self.cover_fade < 1.0 {
                self.cover_fade = 1.0;
            }
            if self.prev_cover_rgba.is_some() {
                self.prev_cover_rgba = None;
                self.cover_blend_buf = Vec::new();
            }
            return;
        }
        let target_bytes = cover.rgba.pixels.as_slice();
        let bytes: &[u8] = if self.cover_fade < 1.0 {
            if let Some(prev) = self.prev_cover_rgba.as_ref().filter(|p| p.len() == target_bytes.len()) {
                let s = self.cover_fade;
                let smooth = s * s * (3.0 - 2.0 * s);
                let alpha = (smooth * 256.0).round().clamp(0.0, 256.0) as u32;
                let inv = 256 - alpha;
                self.cover_blend_buf.resize(target_bytes.len(), 0);
                for ((dst, &p), &n) in self
                    .cover_blend_buf
                    .iter_mut()
                    .zip(prev.iter())
                    .zip(target_bytes.iter())
                {
                    *dst = ((u32::from(p) * inv + u32::from(n) * alpha) >> 8) as u8;
                }
                self.cover_blend_buf.as_slice()
            } else {
                self.cover_fade = 1.0;
                self.prev_cover_rgba = None;
                self.cover_blend_buf = Vec::new();
                target_bytes
            }
        } else {
            self.prev_cover_rgba = None;
            self.cover_blend_buf = Vec::new();
            target_bytes
        };
        if let Some(handle) = self.cover_surface {
            if let Err(error) = window.update_custom_surface(handle, bytes) {
                log::warn!("cover surface update: {error:#}");
            }
        } else {
            match window.create_custom_surface(cover.width, cover.height, bytes) {
                Ok(handle) => {
                    self.cover_surface = Some(handle);
                    self.cover_size = (cover.width, cover.height);
                }
                Err(error) => log::warn!("cover surface: {error:#}"),
            }
        }
    }

    fn now_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    /// Wall-clock Unix milliseconds aligned with WinRT SMTC `last_updated_unix_ms`.
    fn live_now_ms() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0)
    }

    /// Renders the last painted frame to an offscreen texture and writes it as
    /// a PNG. Nothing is presented, so a viewport larger than the free space on
    /// the screen can still be captured.
    fn take_offscreen_capture(&mut self, window: &mut Window) {
        #[cfg(feature = "qa-capture")]
        {
            let Some(path) = self.cfg.capture_rgba.clone() else {
                return;
            };
            match window.render_to_image() {
                Ok(image) => {
                    if let Some(parent) = path.parent() {
                        if let Err(error) = std::fs::create_dir_all(parent) {
                            log::warn!("{}: {error:#}", parent.display());
                        }
                    }
                    match image.save(&path) {
                        Ok(()) => log::info!(
                            "offscreen capture: {} ({}x{}) at {:.0}ms",
                            path.display(),
                            image.width(),
                            image.height(),
                            self.position_ms
                        ),
                        Err(error) => log::warn!("{}: {error:#}", path.display()),
                    }
                }
                Err(error) => log::warn!("offscreen capture failed: {error:#}"),
            }
        }
        #[cfg(not(feature = "qa-capture"))]
        {
            let _ = window;
            log::warn!("--capture-rgba needs the qa-capture feature");
        }
    }

    /// Advances the replay clock. A live source would feed the same fields from
    /// the shell; the view never branches on where they came from.
    fn advance(&mut self, now_ms: f64, live_now_ms: f64) {
        if self.source == Source::Live {
            self.poll_live(now_ms, live_now_ms);
            self.sync_smart_translation();
            self.last_ms = now_ms;
            return;
        }
        let dt = if self.last_ms <= 0.0 {
            0.0
        } else {
            (now_ms - self.last_ms).clamp(0.0, 250.0)
        };
        self.last_ms = now_ms;
        if self.playing {
            self.position_ms += dt;
            if self.duration_ms > 0.0 && self.position_ms > self.duration_ms {
                self.position_ms = 0.0;
            }
        }
    }

    fn metrics(&self, window: &Window, cx: &App) -> Metrics {
        self.metrics_for_mode(self.tool.mode().unwrap_or(Mode::Window), window, cx)
    }

    fn metrics_for_mode(&self, mode: Mode, window: &Window, cx: &App) -> Metrics {
        let size = window.viewport_size();
        let (w, h) = (f32::from(size.width), f32::from(size.height));
        // The display query lives on `App`, not on the window, and it is what
        // tells the layout whether the window is at its screen size (the
        // reference's `wideArtworkSize` scales with that ratio).
        let screen = cx
            .primary_display()
            .map(|d| {
                let b = d.bounds();
                (f32::from(b.size.width), f32::from(b.size.height))
            })
            .unwrap_or((w, h));
        let mut m = model::metrics(w, h, screen, window.is_fullscreen(), &self.display);
        if matches!(mode, Mode::Mini | Mode::Album | Mode::Lyrics) {
            m = model::compact_metrics(w, h, mode == Mode::Lyrics, &self.display);
            if mode == Mode::Album {
                m.cover = Box4::new(0.0, 0.0, w, h);
                m.art_height = h;
                m.cover_meta_top = h - 144.0;
                m.progress = Box4::new(16.0, h - 94.0, (w - 32.0).max(1.0), 16.0);
            }
        }
        let layout_w = if matches!(mode, Mode::Window) {
            w / m.ui_scale.max(0.25)
        } else {
            w
        };
        m.lyric_viewport = m.lyrics;
        m.lyrics = model::lyric_content(m.lyric_viewport, m.font, layout_w, &self.display);
        m
    }

    /// Writes the run's evidence: the swap chain's own cadence series and a
    /// summary naming what was actually on screen (fonts, rows, frames).
    fn write_reports(&mut self) {
        let Some(dir) = self.cfg.evidence_dir.as_ref() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(dir) {
            log::warn!("evidence dir {}: {error:#}", dir.display());
            return;
        }
        let cadence = dir.join("present-report.json");
        if let Err(error) = std::fs::write(&cadence, self.cadence.report()) {
            log::warn!("{}: {error:#}", cadence.display());
        }
        let (latin,cjk)=(self.display.font_family,self.display.trans_font_family);
        let _=std::fs::write(dir.join("display-report.json"),serde_json::to_vec_pretty(&self.settings.effective()).unwrap());
        let summary = format!(
            concat!(
                "{{
",
                "  \"frames\": {},
",
                "  \"rows\": {},
",
                "  \"durationMs\": {:.0},
",
                "  \"positionMs\": {:.0},
",
                "  \"playing\": {},
",
                "  \"latinFamily\": \"{}\",
",
                "  \"latinRegistered\": {},
",
                "  \"cjkFamily\": \"{}\",
",
                "  \"cjkRegistered\": {},
",
                "  \"cjkStatus\": \"{}\",
",
                "  \"shapedRows\": {},
",
                "  \"shapeHits\": {},
",
                "  \"shapeMisses\": {},
",
                "  \"glassError\": {}
",
                "}}
"
            ),
            self.frames,
            self.rows.len(),
            self.duration_ms,
            self.position_ms,
            self.playing,
            latin,
            fonts::has_family(&self.fonts.all, latin),
            cjk,
            fonts::has_family(&self.fonts.all, cjk),
            fonts::cjk_status(&self.fonts),
            self.shaper.shaped_rows,
            self.shaper.hits,
            self.shaper.misses,
            match self.more_glass.as_mut().and_then(|g| g.take_error()) {
                Some(e) => format!("{e:?}"),
                None => "null".to_string(),
            }
        );
        let live_now = Self::live_now_ms();
        let input_state=serde_json::json!({"offsetMs":self.sync.offset_ms(),"lyricTimeMs":self.lyric_time(),"mediaTimeMs":self.live.as_ref().map(|l|l.model.media_position(live_now)).unwrap_or(self.position_ms),"offsetEnabled":self.sync.enabled(),"scope":self.sync.scope_label(),"bounds":format!("{:?}",self.last_window_bounds),"mode":format!("{:?}",self.tool.mode())});
        let _=std::fs::write(dir.join("input-report.json"),serde_json::to_vec_pretty(&input_state).unwrap());
        let path = dir.join("player-report.json");
        if let Some(live) = self.live.as_ref() {
            let state = serde_json::json!({
                "mode": "live-experiment", "generation": live.model.generation(),
                "packet": live.model.packet.as_ref().map(|p| serde_json::json!({
                    "app": p.app, "title": p.title, "artist": p.artist,
                    "durationMs": p.duration_ms, "positionMs": p.position_ms,
                    "playing": p.is_playing(), "artworkPresent": p.cover.is_some(),
                })), "candidates": live.candidates.len(),
                "status": live.status, "rows": self.rows.len(),
            });
            let _ = std::fs::write(dir.join("live-report.json"), serde_json::to_vec_pretty(&state).unwrap());
        }
        if let Err(error) = std::fs::write(&path, summary) {
            log::warn!("{}: {error:#}", path.display());
        }

        // The panel path is what decides whether the glass work reaches the
        // screen, so the run states it directly instead of leaving it to be
        // inferred from pixels: which panels were open, how many surfaces were
        // stacked, and which scripted steps actually fired.
        let more_probe = self.more_glass.as_mut().map(|g| g.probe());
        let volume_probe = self.volume_glass.as_mut().map(|g| g.probe());
        let picker_probe = self.picker_glass.as_mut().map(|g| g.probe());
        let panels = serde_json::json!({
            "panelsPainted": self.panels_painted,
            "moreOpen": self.tool.panels().more_open,
            "volumeOpen": self.tool.panels().volume_open,
            "pickerOpen": self.picker_open,
            "hotRects": self.hot.len(),
            "moreGlass": self.more_glass.is_some(),
            "volumeGlass": self.volume_glass.is_some(),
            "pickerGlass": self.picker_glass.is_some(),
            "moreProbe": more_probe,
            "volumeProbe": volume_probe,
            "pickerProbe": picker_probe,
            "scenarioNext": self.scenario.as_ref().map(|s| s.next).unwrap_or(0),
            "scenarioTotal": self.scenario.as_ref().map(|s| s.steps.len()).unwrap_or(0),
            "acts": self.acts,
        });
        let path = dir.join("panel-report.json");
        if let Err(error) = std::fs::write(&path, panels.to_string()) {
            log::warn!("{}: {error:#}", path.display());
        }
        log::info!(
            "run complete: {} frames, {} rows, cadence p50 {:.3} ms",
            self.frames,
            self.rows.len(),
            self.cadence.percentiles().0
        );
    }

    /// Builds the group stack, interlude dots, and bottom credits for one frame.
    ///
    /// The offset is whatever the scroll engine holds; automatic alignment only
    /// resumes when all AMLL joint conditions hold (quiet debounce, focus
    /// change out of browsing, active playback or end credits, and target in
    /// render range).
    fn plan_rows(&mut self, m: &Metrics, dt_ms: f64) -> (Vec<PlanRow>, Option<PlanInterlude>, Option<PlanBottom>) {
        if self.rows.is_empty() {
            self.scroll.set_bounds(0.0, 0.0);
            return (Vec::new(), None, None);
        }
        let time = self.lyric_time();
        let now = self.now_ms();
        let force_seek = std::mem::take(&mut self.pending_seek);
        let step = self.lyric_timeline.sync_at(time, self.playing, now, force_seek);
        let groups = self.lyric_timeline.groups().to_vec();
        let group_count = groups.len();
        if group_count == 0 {
            self.scroll.set_bounds(0.0, 0.0);
            return (Vec::new(), None, None);
        }

        let mut non_playback_layout_change = false;
        if self.playing != self.prev_playing {
            self.prev_playing = self.playing;
            self.interlude_dots.set_playing(self.playing);
            if self.playing {
                self.browse_idle_ms = now;
            }
            non_playback_layout_change = true;
        }
        if self.prev_viewport != (m.lyrics.w, m.lyrics.h) {
            self.prev_viewport = (m.lyrics.w, m.lyrics.h);
            non_playback_layout_change = true;
        }
        if self.prev_display.as_ref() != Some(&self.display) {
            let feel_changed = self
                .prev_display
                .as_ref()
                .is_some_and(|d| d.spring_feel != self.display.spring_feel);
            self.prev_display = Some(self.display);
            if feel_changed {
                self.pos_y_params = self.display.spring_params();
                self.interlude_dots.update_spring_params(self.pos_y_params);
            }
            non_playback_layout_change = true;
        }

        let is_initial = self.group_motion.len() != group_count;
        if is_initial
            || step.diff.is_interlude_changed
            || step.diff.is_scroll_to_changed
            || step.diff.is_end_of_song_changed
            || step.diff.is_time_jumped
        {
            let interval = self
                .lyric_timeline
                .group_start_interval(step.snapshot.scroll_to_index);
            let policy = oly_core::scroll::pos_y_spring_policy(
                step.diff.is_time_jumped,
                step.snapshot.active_interlude.is_some(),
                interval,
                step.snapshot.is_end_of_song,
            );
            let (s_scale, d_scale) = match self.display.spring_feel {
                model::SpringFeel::Soft => (0.62, 1.45),
                model::SpringFeel::Standard => (1.0, 1.0),
                model::SpringFeel::Lively => (1.5, 0.72),
            };
            self.pos_y_params = oly_core::scroll::SpringParams {
                stiffness: policy.stiffness * s_scale,
                damping: policy.damping * d_scale,
                mass: self.display.spring_params().mass,
                soft: false,
            };
            self.interlude_dots.update_spring_params(self.pos_y_params);
        }

        self.interlude_dots.sync_clock(time);
        let can_display_interlude = if let Some(inter) = step.snapshot.active_interlude {
            self.interlude_dots.set_interlude(
                inter.start_ms,
                inter.end_ms,
                step.snapshot.current_ms,
                step.diff.is_time_jumped,
                inter.anchor_line_index,
            )
        } else {
            self.interlude_dots
                .clear_interlude(step.diff.is_time_jumped);
            false
        };

        let has_duet = self.rows.iter().any(|r| r.is_duet);
        let wrap_width = m.lyrics.w * if has_duet { 0.85 } else { 1.0 };
        let bottom_shaped = self
            .shaper
            .bottom_line(&self.bottom_text, m, &self.display, wrap_width)
            .ok()
            .flatten();
        let has_bottom_content = bottom_shaped.is_some();

        let mut main_shaped: Vec<Option<Arc<RowLines>>> = Vec::with_capacity(group_count);
        let mut bg_shaped: Vec<Option<Arc<RowLines>>> = Vec::with_capacity(group_count);
        for g in &groups {
            main_shaped.push(
                self.shaper
                    .row(&self.rows[g.main_row], m, &self.display, wrap_width, 0)
                    .ok(),
            );
            bg_shaped.push(
                g.bg_row.and_then(|ri| {
                    self.shaper
                        .row(&self.rows[ri], m, &self.display, wrap_width, 0)
                        .ok()
                }),
            );
        }

        if is_initial {
            self.group_bg_slide = (0..group_count)
                .map(|g| {
                    let hidden = if groups[g].is_bg_first { 80.0 } else { -80.0 };
                    let initial = if step.active_groups[g] || !self.playing {
                        0.0
                    } else {
                        hidden
                    };
                    model::RowMotion::new(initial)
                })
                .collect();
            self.group_bg_alpha = (0..group_count)
                .map(|g| if step.active_groups[g] || !self.playing { 1.0 } else { 0.0 })
                .collect();
            let init_passed_boundary = step
                .snapshot
                .active_interlude
                .map(|i| (i.anchor_line_index + 1).max(0) as usize)
                .unwrap_or(step.snapshot.scroll_to_index);
            self.group_passed_alpha = (0..group_count)
                .map(|g| {
                    if self.display.hide_passed_lines
                        && self.playing
                        && g < init_passed_boundary
                    {
                        0.0
                    } else {
                        1.0
                    }
                })
                .collect();
            self.main_scale = (0..group_count)
                .map(|_| model::RowMotion::new(100.0))
                .collect();
            self.bg_scale = (0..group_count)
                .map(|_| model::RowMotion::new(100.0))
                .collect();
        }

        let pad_bg = model::row_padding(m, &self.display, true);
        let pad_main = model::row_padding(m, &self.display, false);
        let bg_gap = m.font * 0.3;
        let mut group_heights = Vec::with_capacity(group_count);
        for g in 0..group_count {
            let main_h = main_shaped[g]
                .as_ref()
                .map(|l| l.height)
                .unwrap_or(m.line_height);
            let h = if let Some(bg_lines) = bg_shaped[g].as_ref() {
                let bg_inner_h = (bg_lines.height - 2.0 * pad_bg).max(0.0);
                let bg_block_h = bg_gap + bg_inner_h;
                if groups[g].is_bg_first {
                    let active_prog =
                        model::bg_wrapper_progress(self.group_bg_slide[g].position());
                    main_h + bg_block_h * active_prog
                } else if step.active_groups[g] || !self.playing {
                    main_h + bg_block_h
                } else {
                    main_h
                }
            } else {
                main_h
            };
            group_heights.push(h);
        }

        if step.diff.is_time_jumped && !self.scroll.is_direct() {
            self.browse_state = None;
            self.follow.resume_now();
            let cur = self.scroll.offset();
            self.scroll.engine_mut().reset(cur);
        }
        self.scroll.tick(now);
        if self.scroll.is_browsing() {
            self.browse_state = Some((
                step.snapshot.scroll_to_index,
                step.snapshot.focus_on_interlude,
                step.snapshot.is_end_of_song,
            ));
        } else if let Some((prev_group, prev_interlude, prev_end)) = self.browse_state {
            let in_render_range = if step.snapshot.is_end_of_song && has_bottom_content {
                let h = bottom_shaped.as_ref().map(|b| b.height).unwrap_or(0.0);
                self.bottom_motion.as_ref().is_some_and(|bm| {
                    let top = m.lyrics.y + bm.position() as f32 + pad_main;
                    h > 0.0
                        && top < m.lyric_viewport.bottom()
                        && top + h > m.lyric_viewport.y
                })
            } else {
                let target_g = step.snapshot.scroll_to_index.min(group_count - 1);
                let h = group_heights[target_g];
                self.group_motion.get(target_g).is_some_and(|gm| {
                    let top = m.lyrics.y + gm.position() as f32;
                    h > 0.0
                        && top < m.lyric_viewport.bottom()
                        && top + h > m.lyric_viewport.y
                })
            };
            let idle_elapsed = !self.scroll.is_direct()
                && !self.scroll.is_browsing()
                && (now - self.browse_idle_ms >= 3000.0);
            if self.playing
                && (idle_elapsed
                    || super::playback::can_auto_resume_browsing(super::playback::AutoResumeInput {
                        is_direct: self.scroll.is_direct(),
                        can_resume_after_idle: self.scroll.can_resume_auto_align(now),
                        focus_on_interlude: step.snapshot.focus_on_interlude,
                        prev_scroll_to: prev_group,
                        prev_focus_interlude: prev_interlude,
                        prev_end_of_song: prev_end,
                        scroll_to_index: step.snapshot.scroll_to_index,
                        is_end_of_song: step.snapshot.is_end_of_song,
                        has_bottom_content,
                        playing_contains_target: step
                            .snapshot
                            .playing
                            .contains(&step.snapshot.scroll_to_index),
                        target_in_render_range: in_render_range,
                    }))
            {
                self.browse_state = None;
                self.follow.resume_now();
                let cur = self.scroll.offset();
                self.scroll.engine_mut().reset(cur);
            }
        }

        let manual = self.browse_state.is_some();
        let focal_target = self.focus_controller.resolve(
            &step.snapshot,
            group_count,
            manual,
            has_bottom_content,
            can_display_interlude,
        );

        let dot_diameter = m.font * 0.27 * self.display.dot_scale;
        let dot_gap = m.font * 0.18;
        let dots_inner_w = dot_diameter * 3.0 + dot_gap * 2.0;
        let dots_box_h = (m.win_h * 0.01)
            .clamp(m.font * 0.5, m.font * 3.0)
            .max(dot_diameter);
        let dots_elem_h = dots_box_h + m.font * 0.8;
        let dot_margin = m.font * 0.4;
        let total_interlude_h = dots_elem_h + dot_margin * 2.0;

        let layout_interlude_anchor = if can_display_interlude {
            step.snapshot.active_interlude.map(|i| i.anchor_line_index)
        } else {
            None
        }
        .or_else(|| match focal_target {
            oly_core::timeline::FocalTarget::Interlude(a) => Some(a),
            _ => None,
        });
        let interlude_slot = layout_interlude_anchor.map(|a| (a, total_interlude_h));

        let geo = model::group_geometry(
            &group_heights,
            focal_target,
            m.anchor_px,
            interlude_slot,
            has_bottom_content,
        );
        self.scroll.set_bounds(geo.min as f64, geo.max as f64);

        let offset = self.follow.step_configured(
            self.scroll.offset(),
            geo.follow_target() as f64,
            manual,
            dt_ms,
            None,
            self.display.enable_spring,
            self.pos_y_params,
        );
        self.scroll.move_to(offset);

        let offset = self.scroll.offset() as f32;
        let browsing = manual || self.follow.is_direct();
        let direct = self.scroll.is_direct();
        let snap_pos = direct || !self.display.enable_spring;

        if self.group_motion.len() != group_count {
            let base = if browsing { offset } else { geo.follow_target() };
            self.group_motion = geo
                .tops
                .iter()
                .map(|top| model::RowMotion::new((*top + base) as f64))
                .collect();
        }

        if can_display_interlude
            && let (Some(inter), Some(inter_top)) =
                (step.snapshot.active_interlude, geo.interlude_top)
        {
            let interlude_y = inter_top + if browsing { offset } else { geo.follow_target() };
            let target_y = m.lyrics.y + interlude_y + dot_margin + m.font * 0.4;
            let next_idx = ((inter.anchor_line_index + 1).max(0) as usize).min(group_count - 1);
            let next_is_duet = groups[next_idx].is_duet;
            let center_no_duet = !self.display.show_cover && !has_duet && self.tool.mode() == Some(Mode::Window);
            let dots_right = self.display.align_right ^ next_is_duet;
            let dots_x = if center_no_duet {
                m.lyrics.x + (m.lyrics.w - dots_inner_w).max(0.0) * 0.5
            } else if dots_right {
                m.lyrics.right() - dots_inner_w
            } else {
                m.lyrics.x
            };
            self.interlude_dots
                .set_transform(dots_x as f64, target_y as f64, snap_pos, self.pos_y_params);
        }
        self.interlude_dots.update(dt_ms);
        let plan_interlude = self.interlude_dots.visible_snapshot().map(|(snap, ix, iy)| PlanInterlude {
            x: ix as f32,
            y: iy as f32 + (dots_box_h - dot_diameter) * 0.5,
            dot_diameter,
            dot_gap,
            scale: snap.scale as f32,
            opacity: snap.opacity as f32,
            dot_opacities: [
                snap.dot_opacities[0] as f32,
                snap.dot_opacities[1] as f32,
                snap.dot_opacities[2] as f32,
            ],
        });

        let top_bound = m.lyrics.y - m.line_height * 2.0;
        let bottom_bound = m.lyrics.bottom() + m.line_height * 2.0;
        let disable_stagger =
            browsing || step.diff.is_time_jumped || non_playback_layout_change || is_initial;
        let mut delay = 0.0;
        let mut base_delay = if disable_stagger { 0.0 } else { 0.05 };
        let passed_boundary = step
            .snapshot
            .active_interlude
            .map(|i| (i.anchor_line_index + 1).max(0) as usize)
            .unwrap_or(step.snapshot.scroll_to_index);

        let main_scale_params = oly_core::scroll::SpringParams {
            stiffness: 100.0,
            damping: 25.0,
            mass: 2.0,
            soft: false,
        };
        let bg_scale_params = oly_core::scroll::SpringParams {
            stiffness: 50.0,
            damping: 20.0,
            mass: 1.0,
            soft: false,
        };

        let mut out = Vec::with_capacity(self.rows.len());
        for g in 0..group_count {
            let group = &groups[g];
            let target_y = geo.tops[g] + if browsing { offset } else { geo.follow_target() };
            let group_y = self.group_motion[g].step(
                target_y as f64,
                snap_pos,
                delay,
                dt_ms,
                self.pos_y_params,
            ) as f32;
            let group_screen_top = m.lyrics.y + group_y;
            let is_active = step.active_groups[g];

            let scale_target_main = if !is_active && self.playing && self.display.enable_scale {
                97.0
            } else {
                100.0
            };
            let line_scale_main = self.main_scale[g].step(
                scale_target_main,
                !self.display.enable_spring,
                0.0,
                dt_ms,
                main_scale_params,
            ) as f32
                / 100.0;

            let scale_target_bg = if !is_active && self.playing { 75.0 } else { 100.0 };
            let line_scale_bg = self.bg_scale[g].step(
                scale_target_bg,
                !self.display.enable_spring,
                0.0,
                dt_ms,
                bg_scale_params,
            ) as f32
                / 100.0;

            let hidden_slide_y = if group.is_bg_first { 80.0 } else { -80.0 };
            let target_slide_y = if is_active || !self.playing {
                0.0
            } else {
                hidden_slide_y
            };
            let slide_y = self.group_bg_slide[g].step(
                target_slide_y,
                snap_pos,
                delay,
                dt_ms,
                self.pos_y_params,
            );
            let target_bg_alpha = if is_active || !self.playing { 1.0 } else { 0.0 };
            self.group_bg_alpha[g] = model::step_opacity(
                self.group_bg_alpha[g],
                target_bg_alpha,
                dt_ms,
                300.0,
                snap_pos || step.diff.is_time_jumped,
            );

            if target_y + group_heights[g] >= 0.0 {
                delay += base_delay;
                if g >= step.snapshot.scroll_to_index {
                    base_delay /= 1.05;
                }
            }

            let hide_passed = self.display.hide_passed_lines
                && self.playing
                && !browsing
                && g < passed_boundary;
            let target_passed_alpha = if hide_passed { 0.0 } else { 1.0 };
            let passed_alpha = model::step_opacity(
                self.group_passed_alpha[g],
                target_passed_alpha,
                dt_ms,
                300.0,
                snap_pos || step.diff.is_time_jumped,
            );
            self.group_passed_alpha[g] = passed_alpha;
            if passed_alpha <= 0.005 {
                continue;
            }
            let Some(main_lines) = main_shaped[g].as_ref() else {
                continue;
            };

            let blur = if self.display.blur_max && !is_active && !browsing {
                let focal_g = step.snapshot.scroll_to_index;
                let d = if g < focal_g {
                    focal_g - g + 1
                } else {
                    g.saturating_sub(step.latest_group)
                };
                let scale = self.display.blur_strength / 100.0;
                (1.0 + d as f32).min(5.0) * scale * if m.win_w <= 1024.0 { 0.8 } else { 1.0 }
            } else {
                0.0
            };
            let trans_alpha = if step.highlighted_groups[g] {
                self.display.trans_current
            } else {
                self.display.trans_opacity
            };

            // One wrapper covers main, translation, romanization and visible BG.
            let (hover_outer, _) = super::playback::row_bounds(
                m.lyric_viewport, self.display.align_right,
                self.rows[group.main_row].is_duet, has_duet,
            );
            let hover_region = Some((group.main_row,
                Box4::new(hover_outer.x, group_screen_top, hover_outer.w, group_heights[g])));
            let mut main_draw_top = group_screen_top + pad_main;
            let mut bg_plan: Option<PlanRow> = None;

            if let (Some(bg_row_idx), Some(bg_lines)) = (group.bg_row, bg_shaped[g].as_ref()) {
                let bg_inner_h = (bg_lines.height - 2.0 * pad_bg).max(0.0);
                let bg_block_h = bg_gap + bg_inner_h;
                let active_prog = model::bg_wrapper_progress(slide_y);
                let w_scale = model::bg_wrapper_scale(active_prog);
                let translate_y = (slide_y / 100.0) as f32 * bg_inner_h;

                let (bg_base_top, pivot_y) = if group.is_bg_first {
                    let slot_h = bg_block_h * active_prog;
                    main_draw_top = group_screen_top + slot_h + pad_main;
                    let base = group_screen_top + pad_main - bg_block_h * (1.0 - active_prog);
                    let draw = base + translate_y;
                    (draw, draw + bg_inner_h)
                } else {
                    let base = group_screen_top + main_lines.height - pad_main + bg_gap;
                    let draw = base + translate_y;
                    (draw, draw)
                };

                let bg_alpha = self.group_bg_alpha[g];
                let bg_hidden = !is_active
                    && self.playing
                    && ((slide_y - hidden_slide_y).abs() < 0.1 || bg_alpha <= 0.001);
                if !bg_hidden
                    && bg_alpha > 0.001
                    && bg_base_top <= bottom_bound
                    && bg_base_top + bg_inner_h >= top_bound
                {
                    let bg_row = &self.rows[bg_row_idx];
                    let light = row_light(bg_row, time, is_active, true, &self.display);
                    let mut row_light = RowLight {
                        base: light.dark * bg_alpha * passed_alpha,
                        dark: light.dark * bg_alpha * passed_alpha,
                        bright: light.bright * bg_alpha * passed_alpha,
                        mask: None,
                        glow: light.glow.map(|mut g| {
                            for v in &mut g {
                                *v *= bg_alpha * passed_alpha;
                            }
                            g
                        }),
                    };
                    if light.sweep.is_some() {
                        row_light.mask = Some(self.mask_positions(bg_row, bg_lines));
                    }
                    let (bounds, align_right) = super::playback::row_bounds(
                        m.lyrics,
                        self.display.align_right,
                        bg_row.is_duet,
                        has_duet,
                    );
                    let align = if !self.display.show_cover && !has_duet && self.tool.mode() == Some(Mode::Window) {
                        paint::LineAlign::Center
                    } else if align_right {
                        paint::LineAlign::Right
                    } else {
                        paint::LineAlign::Left
                    };
                    let pivot_x = align.pivot_x(bounds.x, bounds.w);
                    bg_plan = Some(PlanRow {
                        hover_region,
                        lines: bg_lines.clone(),
                        bounds,
                        align,
                        light: row_light,
                        translation_alpha: trans_alpha * bg_alpha * passed_alpha,
                        roman_alpha: self.display.phonetic_opacity * bg_alpha * passed_alpha,
                        scale: line_scale_bg,
                        wrapper: paint::WrapperTransform {
                            scale: w_scale,
                            pivot_x,
                            pivot_y,
                        },
                        blur,
                        top: bg_base_top,
                    });
                }
            }

            if let Some(bg_row_plan) = bg_plan {
                out.push(bg_row_plan);
            }

            if main_draw_top <= bottom_bound && main_draw_top + main_lines.height >= top_bound {
                let main_row = &self.rows[group.main_row];
                let light = row_light(main_row, time, is_active, false, &self.display);
                let mut row_light = RowLight {
                    base: light.dark * passed_alpha,
                    dark: light.dark * passed_alpha,
                    bright: light.bright * passed_alpha,
                    mask: None,
                    glow: light.glow.map(|mut g| {
                        for v in &mut g {
                            *v *= passed_alpha;
                        }
                        g
                    }),
                };
                if light.sweep.is_some() {
                    row_light.mask = Some(self.mask_positions(main_row, main_lines));
                }
                let (bounds, align_right) = super::playback::row_bounds(
                    m.lyrics,
                    self.display.align_right,
                    main_row.is_duet,
                    has_duet,
                );
                let align = if !self.display.show_cover && !has_duet && self.tool.mode() == Some(Mode::Window) {
                    paint::LineAlign::Center
                } else if align_right {
                    paint::LineAlign::Right
                } else {
                    paint::LineAlign::Left
                };
                out.push(PlanRow {
                    hover_region,
                    lines: main_lines.clone(),
                    bounds,
                    align,
                    light: row_light,
                    translation_alpha: trans_alpha * passed_alpha,
                    roman_alpha: self.display.phonetic_opacity * passed_alpha,
                    scale: line_scale_main,
                    wrapper: paint::WrapperTransform::default(),
                    blur,
                    top: main_draw_top,
                });
            }
        }

        let plan_bottom = if let Some(bottom_lines) = bottom_shaped {
            let is_bottom_focused = focal_target == oly_core::timeline::FocalTarget::Bottom;
            let target_y = geo.bottom_top + if browsing { offset } else { geo.follow_target() };
            let bottom_y = self
                .bottom_motion
                .get_or_insert_with(|| model::RowMotion::new(target_y as f64))
                .step(
                    target_y as f64,
                    snap_pos,
                    delay,
                    dt_ms,
                    self.pos_y_params,
                ) as f32;
            self.bottom_target_alpha = if is_bottom_focused { 0.85 } else { 0.2 };
            self.bottom_alpha = model::step_opacity(
                self.bottom_alpha,
                self.bottom_target_alpha,
                dt_ms,
                400.0,
                snap_pos || step.diff.is_time_jumped,
            );
            let bottom_blur = if self.display.blur_max && !is_bottom_focused && !browsing {
                let d = group_count.saturating_sub(step.latest_group);
                (1.0 + d as f32).min(5.0) * if m.win_w <= 1024.0 { 0.8 } else { 1.0 }
            } else {
                0.0
            };
            let bottom_screen_top = m.lyrics.y + bottom_y + pad_main;
            if bottom_screen_top <= bottom_bound
                && bottom_screen_top + bottom_lines.height >= top_bound
            {
                let (bounds, align_right) = super::playback::row_bounds(
                    m.lyrics,
                    self.display.align_right,
                    false,
                    has_duet,
                );
                let align = if !self.display.show_cover && !has_duet && self.tool.mode() == Some(Mode::Window) {
                    paint::LineAlign::Center
                } else if align_right {
                    paint::LineAlign::Right
                } else {
                    paint::LineAlign::Left
                };
                Some(PlanBottom {
                    lines: bottom_lines,
                    x_base: bounds.x,
                    wrap_width: bounds.w,
                    align,
                    top: bottom_screen_top,
                    alpha: self.bottom_alpha,
                    blur: bottom_blur,
                })
            } else {
                None
            }
        } else {
            self.bottom_motion = None;
            None
        };

        (out, plan_interlude, plan_bottom)
    }

    /// Mask positions for the active row, in the reference's own units.
    ///
    /// The legacy build animates a `mask-position` across the word box; the
    /// ported keyframe generator produces exactly those stops, so the wipe here
    /// is the same curve rather than a per-word linear ramp.
    fn mask_positions(&self, row: &Row, lines: &RowLines) -> Vec<f32> {
        let progress = paint::line_progress(self.lyric_time(), row.start_ms, row.duration_ms());
        lines.masks.iter().map(|frames| position_at(frames, progress)).collect()
    }

    /// Runs at most one due step after input geometry exists. Each arm reaches the same
    /// function a real input event would (`on_scroll`, `on_down`, `on_drag`,
    /// `on_key`) so nothing here can pass while the listener path is broken.
    fn apply_scenario(&mut self, now: f64, window: &mut Window, cx:&mut Context<Self>) {
        // GPU/asset startup may exceed the first action time. Never consume a
        // pointer action before the first painted hit map has been installed.
        if self.hot.is_empty() { return; }
        let Some(scenario) = self.scenario.as_mut() else {
            return;
        };
        let due:Vec<Act>=scenario.take_due(now).into_iter().collect();
        self.acts += due.len() as u64;
        for act in due {
            log::info!("[scenario] act={act:?} now={now:.1} frames={} mode={:?} settings={} more={} hot={}",self.frames,self.tool.mode(),self.settings.open,self.tool.panels().more_open,self.hot.len());
            match act {
                Act::DetailsMode=>{self.pending_mode=Some(1);}
                Act::SetMode(m)=>{self.pending_mode=Some(m);}
                Act::ProbeBounds(expect_fs, expect_w, expect_h)=>{
                    let b=window.bounds();
                    let w=f32::from(b.size.width);
                    let h=f32::from(b.size.height);
                    log::info!("[fullscreen-roundtrip] fs={} bounds={:?} resizable={}",window.is_fullscreen(),b,window.is_resizable());
                    assert_eq!(window.is_fullscreen(), expect_fs, "fullscreen state mismatch: bounds={b:?}");
                    if !expect_fs {
                        assert!((w-expect_w).abs()<=2. && (h-expect_h).abs()<=2., "expected size ({expect_w}x{expect_h}), got ({w}x{h}) bounds={b:?}");
                    }
                }
                Act::VersionProbe=>{
                    let rows=self.candidates();assert_eq!(rows.len(),2);assert_eq!(rows[0].timing_badge,Some("逐字"));assert!(rows[0].has_translation);assert_eq!(rows[1].timing_badge,Some("逐行"));assert!(!rows[1].has_translation);
                    assert!(rows.iter().all(|r|!r.id.contains("below") && !r.id.contains("unknown")));
                    let before=rows[1].smart_translation;let (_,r)=self.hot.iter().find(|(h,_)|*h==Hot::SmartTranslation(1)).copied().expect("row translation button");let(x,y)=r.centre();self.click_at(x,y);
                    assert_ne!(before,self.candidates()[1].smart_translation);
                    #[cfg(feature="qa-capture")]
                    if let Some(dir)=&self.cfg.evidence_dir{window.render_to_image().unwrap().save(dir.join("versions.png")).unwrap();}
                }
                Act::SettingsShot(app)=>{
                    assert_eq!(self.settings.kind,if app{settings::SettingsKind::Application}else{settings::SettingsKind::Lyrics});
                    assert!(self.settings.open,"settings kind={:?} mode={:?} pending={:?} hot={:?}",self.settings.kind,self.tool.mode(),self.pending_settings_window,self.hot);
                    #[cfg(feature="qa-capture")]
                    if let Some(dir)=&self.cfg.evidence_dir{let _=std::fs::create_dir_all(dir);window.render_to_image().unwrap().save(dir.join(if app{"application.png"}else{"lyrics.png"})).unwrap();}
                }
                Act::LensDragStart(which)=>{
                    let target = if which==0 {
                        self.hot.iter().find(|(h,_)|*h==Hot::MenuMode(2)).map(|(_,r)|r.centre())
                    } else {
                        self.hot.iter().find(|(h,_)|*h==Hot::SettingsPage(0)).map(|(_,r)|r.centre())
                    }.expect("lens drag start target");
                    self.cursor=Some(target);self.on_down(target.0,target.1,true);
                }
                Act::LensDragMove(which)=>{
                    let target = if which==0 {
                        let a=self.hot.iter().find(|(h,_)|*h==Hot::MenuMode(1)).unwrap().1.centre();
                        let b=self.hot.iter().find(|(h,_)|*h==Hot::MenuMode(2)).unwrap().1.centre();
                        ((a.0+b.0)*0.5,a.1)
                    } else {
                        self.hot.iter().find(|(h,_)|*h==Hot::SettingsPage(1)).unwrap().1.centre()
                    };
                    self.cursor=Some(target);self.on_drag(target.0,target.1);
                }
                Act::LensDragEnd=>{
                    let target=self.cursor.unwrap_or((0.,0.));self.on_up(target.0,target.1);
                }
                Act::LensShot(_name)=>{
                    #[cfg(feature="qa-capture")]
                    if let Some(dir)=&self.cfg.evidence_dir{let _=std::fs::create_dir_all(dir);window.render_to_image().unwrap().save(dir.join(_name)).unwrap();}
                }
                Act::BackgroundShot(_i)=>{
                    #[cfg(feature="qa-capture")]
                    if let Some(dir)=&self.cfg.evidence_dir{let _=std::fs::create_dir_all(dir);window.render_to_image().unwrap().save(dir.join(format!("frame-{_i}.png"))).unwrap();}
                }
                Act::FocusAway=>{
                    log::info!("[focus-qa] creating auxiliary window at {now:.1}ms");
                    #[cfg(feature="qa-capture")]
                    {let _=gpui_kit::open_window(WindowOptions{window_bounds:Some(WindowBounds::Windowed(gpui_kit::bounds(point(px(0.),px(0.)),size(px(100.),px(100.))))),titlebar:Some(TitlebarOptions{title:Some("Focus QA".into()),..Default::default()}),..Default::default()},cx,|_,cx|cx.new(|_|CornerBackdrop));}
                    log::info!("[focus-qa] auxiliary window opened at {now:.1}ms");
                }
                Act::FrameSample=>{assert!(!window.is_window_active());self.qa_frame_sample=Some((self.frames,now));}
                Act::FrameRate=>{
                    assert!(!window.is_window_active());let (frames,start)=self.qa_frame_sample.take().unwrap();let fps=(self.frames-frames) as f64*1000./(now-start);
                    log::info!("[focus-qa] inactive_fps={fps:.2}");assert!(fps>45.,"inactive cadence unexpectedly low: {fps}");
                    if let Some(dir)=&self.cfg.evidence_dir{let _=std::fs::write(dir.join("inactive-fps.txt"),format!("fps={fps:.2}\nactive=false\n"));}
                }
                Act::HoverLyric=>{
                    let m=self.metrics(window, cx);
                    self.cursor=Some((m.lyrics.x+30.,m.lyrics.y+m.lyrics.h*0.42));
                }
                Act::DetailsShortcut=>{self.activate(Some(Hot::MoreRow(4)),0.,0.);self.on_key("s");assert!(!self.settings.open,"external lyrics settings replace the inset sheet");}
                Act::DetailsProbe=>{
                    let child=self.settings_child.expect("settings sidecar must be created");
                    let host=window.bounds();let area=window.display(cx).unwrap().visible_bounds();
                    let b=|b:gpui_kit::Bounds<Pixels>|Box4::new(b.origin.x.into(),b.origin.y.into(),b.size.width.into(),b.size.height.into());
                    let (expected,side)=super::details::side_panel(b(host),b(area),380.);
                    let actual=child.update(cx,|_,w,_|b(w.bounds())).expect("settings sidecar must remain open");
                    #[cfg(feature="qa-capture")]
                    child.update(cx,|_,w,_|{
                        let image=w.render_to_image().expect("sidecar alpha capture");
                        // Empty footer, away from text, controls and edge shadows.
                        let pixel=image.get_pixel(image.width()/2,image.height()-30).0;
                        assert!(pixel[3]>20 && pixel[3]<110,"desktop backdrop must remain visible through sheet: {pixel:?}");
                        if let Some(dir)=&self.cfg.evidence_dir{
                            let _=std::fs::create_dir_all(dir);
                            image.save(dir.join("sidecar-alpha.png")).expect("save sidecar alpha");
                            std::fs::write(dir.join("backdrop.txt"),format!("native=Blurred footer_rgba={pixel:?}\n")).expect("save alpha evidence");
                        }
                    }).expect("sidecar capture window");
                    assert!((actual.x-expected.x).abs()<=1. && (actual.y-expected.y).abs()<=1. && (actual.w-expected.w).abs()<=1. && (actual.h-expected.h).abs()<=1.);
                    if let Some(dir)=&self.cfg.evidence_dir{let _=std::fs::create_dir_all(dir);let _=std::fs::write(dir.join("sidecar.txt"),format!("side={side:?} host={host:?} actual={actual:?} expected={expected:?}\n"));}
                }
                Act::DetailsColor=>{let _=self.settings_child_view.clone().unwrap().update(cx,|child,cx|{child.settings.set_kind(settings::SettingsKind::Application);child.settings.page=4;child.settings.scroll=0.;child.settings.open_choice=Some(4);cx.notify();});}
                Act::DetailsColorProbe=>{
                    #[cfg(feature="qa-capture")]
                    {
                        let d=self.settings_child_view.clone().unwrap().update(cx,|child,_|child.settings.dropdown_layout(Settings::panel_for(settings::SettingsKind::Application,380.,650.)).expect("accent list")).unwrap();
                        assert!(d.is_color);
                        self.settings_child.unwrap().update(cx,|_,w,_|{
                            let image=w.render_to_image().expect("accent list capture");let scale=w.scale_factor();
                            let pixel=image.get_pixel(((d.rect.right()-12.)*scale) as u32,((d.rect.bottom()-12.)*scale) as u32).0;
                            assert!(pixel[3]>90 && pixel[3]<160,"accent list must not accumulate dark shadows: {pixel:?}");
                            if let Some(dir)=&self.cfg.evidence_dir{image.save(dir.join("accent-alpha.png")).unwrap();std::fs::write(dir.join("accent.txt"),format!("rgba={pixel:?}\n")).unwrap();}
                        }).unwrap();
                    }
                }
                Act::DetailsDrag=>{
                    let (key,before,after)=self.settings_child_view.clone().unwrap().update(cx,|child,cx|{
                        let panel=child.panel_bounds.last().copied().expect("settings panel");
                        if !child.settings.rows(panel).iter().any(|(_,item,_)|matches!(item.control,Control::Switch)) {
                            child.settings.scroll = child.settings.max_scroll(panel);
                            child.hot.retain(|(h,_)|!matches!(h,Hot::SettingSlider(_)|Hot::SettingChoice(_)));
                            for (i,item,row) in child.settings.rows(panel) {
                                child.hot.push((match item.control.slider_bounds() {Some(_)=>Hot::SettingSlider(i as u8),None=>Hot::SettingChoice(i as u8)},match item.control.slider_bounds(){Some(_)=>Box4::new(row.x+8.0,row.y+26.0,row.w-16.0,27.0),None=>row}));
                            }
                        }
                        let (index,item,row)=child.settings.rows(panel).into_iter().find(|(_,item,_)|matches!(item.control,Control::Switch)).expect("visible switch");
                        let before=child.settings.value(item.key).as_bool().unwrap();let track=super::details::switch_track(row);
                        let from=super::details::switch_knob(track,if before{1.}else{0.},0.).centre();
                        let to=super::details::switch_knob(track,if before{0.}else{1.},0.).centre();
                        child.on_down(from.0,from.1,true);child.on_drag(to.0,to.1);
                        assert_eq!(child.settings.value(item.key).as_bool(),Some(before));
                        assert!(child.settings.switch_preview.is_some());
                        child.on_up(to.0,to.1);let after=child.settings.value(item.key).as_bool().unwrap();assert_ne!(before,after);assert!(child.switch_drag.is_none());
                        let _=index;cx.notify();(item.key,before,after)
                    }).expect("sidecar drag");
                    if let Some(dir)=&self.cfg.evidence_dir{let _=std::fs::write(dir.join("switch.txt"),format!("key={key} before={before} after={after} preview_did_not_commit=true\n"));}
                }
                Act::DetailsClose=>{assert_eq!(self.settings.value("showTranslation").as_bool(),Some(false),"sidecar drafts must reach player");let _=self.settings_child_view.clone().unwrap().update(cx,|child,cx|{child.activate(Some(Hot::SettingsClose),0.,0.);cx.notify();});}
                Act::DetailsClosed=>{
                    assert!(self.settings_child.unwrap().update(cx,|_,_,_|()).is_err(),"settings close must remove only the sidecar");
                    assert_eq!(self.tool.mode(),Some(Mode::Lyrics));
                    if let Some(dir)=&self.cfg.evidence_dir{let _=std::fs::write(dir.join("close.txt"),"sidecar_closed=true player_still_running=true\n");}
                }
                Act::Pause => self.playing = false,
                Act::Resume => self.playing = true,
                Act::Wheel(dy) => {
                    let ev = ScrollEvent {
                        position: point(px(0.0), px(0.0)),
                        delta: ScrollDelta::Pixels(point(px(0.0), px(dy as f32))),
                        modifiers: Modifiers::default(),
                        touch_phase: TouchPhase::Moved,
                        source: ScrollSource::Pointer,
                    };
                    self.scroll.on_scroll(&ev, now);
                }
                Act::ThumbDown=>{let(x,y)=self.chrome.scroll_thumb.centre();self.cursor=Some((x,y));self.on_down(x,y,true);},
                Act::ThumbMove(dy)=>{if let Some((y,_))=self.drag_thumb{let x=self.chrome.scroll_thumb.centre().0+8.;self.cursor=Some((x,y+dy as f32));self.on_drag(x,y+dy as f32);}},
                Act::ThumbUp=>{let(x,y)=self.cursor.unwrap_or(self.chrome.scroll_thumb.centre());self.on_up(x,y);},
                Act::ThumbDrag(dy) => {
                    let (x, y) = self.chrome.scroll_thumb.centre();
                    self.on_down(x,y,true);
                    self.on_drag(x, y + dy as f32);
                    self.cancel_press();
                }
                Act::OpenMore => {
                    let (x, y) = self.chrome.more.centre();
                    self.click_at(x, y);
                }
                Act::MoreRow(i) => {
                    if let Some(rect) = self.hot.iter().find(|(h, _)| *h == Hot::MoreRow(i)).map(|(_, r)| *r) {
                        let (x, y) = rect.centre();
                        log::info!("[scenario] menu row={i} hit={:?} rect={rect:?}",pointer_hit(&self.hot,&self.panel_bounds,x,y));
                        self.click_at(x, y);
                    }
                }
                Act::Pick(i) => {
                    // Pick the row the picker paints, through the same arm the
                    // pointer uses.
                    if let Some((_, rect)) = self
                        .hot
                        .iter()
                        .find(|(h, _)| *h == Hot::Candidate(i))
                    {
                        let (cx, cy) = rect.centre();
                        self.click_at(cx, cy);
                    }
                }
                Act::OpenVolume => {
                    let (x, y) = self.chrome.volume.centre();
                    self.click_at(x, y);
                }
                Act::Volume(v) => {
                    if let Some((_, rect)) = self
                        .hot
                        .iter()
                        .find(|(h, _)| *h == Hot::VolumeTrack)
                    {
                        let f = ((v - SLIDER_MIN) / (SLIDER_MAX - SLIDER_MIN)).clamp(0.0, 1.0) as f32;
                        self.click_at(rect.x + rect.w * f, rect.centre().1);
                    }
                }
                Act::ToggleTranslation => {
                    self.on_key("t");
                }
                Act::ToggleBreak => {
                    self.on_key("b");
                }
                Act::ClosePanels => {
                    self.on_key("escape");
                }
                Act::Resize(w, h) => {
                    window.resize(size(px(w as f32), px(h as f32)));
                }
            }
        }
    }

    fn open_settings_window(&mut self,_page:usize,window:&mut Window,cx:&mut Context<Self>){
        self.cursor = None;
        let kind = settings::SettingsKind::Application;
        let sheet_title = "设置";
        let win_title = "设置 · OverLyrics";
        let (sw, sh) = (540., 440.);
        let host=window.bounds();
        let area=window.display(cx).map(|d|d.visible_bounds()).unwrap_or(host);
        let b=|b:gpui_kit::Bounds<Pixels>|Box4::new(b.origin.x.into(),b.origin.y.into(),b.size.width.into(),b.size.height.into());
        let (rect,side)=super::details::side_panel_sized(b(host),b(area),sw,sh);
        let page=0;
        let profile=self.settings.active_profile();
        let active_player=self.settings.active_player.clone();
        if let Some(child)=self.settings_child {
            if child.update(cx,|_,w,_|{w.set_window_title(win_title);w.set_bounds(gpui_kit::bounds(point(px(rect.x),px(rect.y)),size(px(rect.w),px(rect.h))));w.activate_window();}).is_ok(){
                if let Some(view)=&self.settings_child_view{let _=view.update(cx,|v,cx|{v.settings.profile=profile;v.settings.active_player=active_player;v.settings.set_kind(kind);v.settings.open=true;v.settings.page=page;v.settings.title=sheet_title;v.settings.scroll=0.0;v.settings.close_choice();v.sync_profile_display();cx.notify();});}
                return;
            }
            self.settings_child=None;
        }
        let parent=cx.weak_entity();let config=self.settings.config.clone();let store=self.settings.shared_store();let active_player=self.settings.active_player.clone();let display_mode_label=self.settings.display_mode_label;
        #[cfg(feature="qa-capture")]
        if self.cfg.scenario.as_deref()==Some("settings-corners") {
            let _=gpui_kit::open_window(WindowOptions{
                window_bounds:Some(WindowBounds::Windowed(gpui_kit::bounds(point(px(rect.x-8.),px(rect.y-8.)),size(px(rect.w+16.),px(rect.h+16.))))),
                titlebar:Some(TitlebarOptions{title:Some("Corner QA background".into()),appears_transparent:true,..Default::default()}),
                focus:false,is_resizable:false,..Default::default()
            },cx,|_,cx|cx.new(|_|CornerBackdrop));
        }

        let options=WindowOptions{
            window_bounds:Some(WindowBounds::Windowed(gpui_kit::bounds(point(px(rect.x),px(rect.y)),size(px(rect.w),px(rect.h))))),
            titlebar:Some(TitlebarOptions{title:Some(win_title.into()),appears_transparent:true,..Default::default()}),
            window_background:WindowBackgroundAppearance::Blurred,is_resizable:false,is_minimizable:false,
            display_id:window.display(cx).map(|d|d.id()),..Default::default()
        };
        match gpui_kit::open_window(options,cx,move|w,cx|cx.new(|cx|{
            let mut view=Self::new(ViewConfig{freeze_ms:Some(0.),settings_only:true,..Default::default()},w,cx);
            view.settings=store.map(Settings::with_store).unwrap_or_default();view.settings.config=config.clone();
            view.settings.profile=profile;view.settings.display_mode_label=display_mode_label;view.settings.active_player=active_player;view.settings.set_kind(kind);view.settings.open=true;view.settings.page=page;view.settings.title=sheet_title;
            view.settings_host=Some(SettingsHost{parent,side,last_config:config});view.playing=false;
            view.sync_profile_display();
            view.play_morph.set_target_position(0.0);view.play_morph.update(10.0);
            view
        })){
            Ok((child,view))=>{
                self.settings_child_view=Some(view.downgrade());
                if let Some(root)=child.downcast::<gpui_kit::base::Root>(){let _=root.update(cx,|r,_,cx|{r.style().background=Some(gpui_kit::transparent_black().into());cx.notify();});}
                self.settings_child=Some(child);log::info!("[settings-sidecar] side={side:?} page={page} host={host:?} bounds={rect:?}");
            }
            Err(error)=>{log::warn!("settings side window: {error}");self.settings.open=true;}
        }
    }

    fn open_panels(&self) -> (bool, bool) {
        let panels = self.tool.panels();
        (panels.more_open, panels.volume_open)
    }

    fn ensure_glass(&mut self, window: &mut Window, kind: PanelKind, size: (f64, f64), scale: f32) -> bool {
        let slot = match kind {
            PanelKind::More => &mut self.more_glass,
            PanelKind::Volume => &mut self.volume_glass,
            PanelKind::Picker => &mut self.picker_glass,
            PanelKind::Settings => &mut self.settings_glass,
            PanelKind::Thumb => &mut self.thumb_glass,
            PanelKind::SliderLens => &mut self.slider_lens_glass,
            PanelKind::ButtonLens => &mut self.button_lens_glass,
            PanelKind::SegmentedLens => &mut self.segmented_lens_glass,
            PanelKind::TabLens => &mut self.tab_lens_glass,
            PanelKind::Dropdown => &mut self.dropdown_glass,
            PanelKind::Island => &mut self.island_glass,
        };
        let (w, h) = panel_pixels(size, scale);
        let reusable_lens = matches!(kind, PanelKind::SegmentedLens | PanelKind::TabLens);
        if slot.as_ref().is_some_and(|panel| {
            let (pw, ph) = panel.pixels();
            if reusable_lens {
                pw >= w && ph >= h
            } else {
                (pw, ph) == (w, h)
            }
        }) {
            return true;
        }
        if let Some(mut old) = slot.take() {
            old.destroy(window);
        }
        let mut material = Material::popover_clear();
        material.blur_px = if kind == PanelKind::More {8.0} else {12.0};
        material.tint_opacity = if kind == PanelKind::More {0.14} else {0.17};
        material.saturation = 1.24;
        material.radius = 16.0;
        if kind==PanelKind::Thumb {material.blur_px=5.0;material.tint=[1.0;3];material.tint_opacity=0.22;material.saturation=1.25;material.rim_hi=[1.0,1.0,1.0,0.72];material.rim_lo=[1.0,1.0,1.0,0.12];}
        if kind == PanelKind::SliderLens {
            material.blur_px = 3.5;
            material.tint = [1.0; 3];
            material.tint_opacity = 0.14;
            material.saturation = 1.32;
            material.rim_hi = [1.0, 1.0, 1.0, 0.85];
            material.rim_lo = [1.0, 1.0, 1.0, 0.22];
        }
        if matches!(kind, PanelKind::SegmentedLens | PanelKind::TabLens | PanelKind::ButtonLens) {
            material.blur_px = 3.2;
            material.tint = [0.97, 0.985, 1.0];
            material.tint_opacity = 0.03;
            material.saturation = 1.28;
            material.contrast = 1.03;
            material.rim_hi = [1.0, 1.0, 1.0, 0.34];
            material.rim_lo = [0.60, 0.68, 0.82, 0.14];
        }
        if kind == PanelKind::Dropdown {material.radius=12.0;}
        material.blur_px *= scale as f64;
        match GlassPanel::create(window, w, h, material, scale as f64) {
            Ok(mut panel) => {
                if let Some(error) = panel.take_error() {
                    log::warn!("glass {kind:?}: {error}");
                }
                log::info!("glass {kind:?} created at {w}x{h}");
                *slot = Some(panel);
                true
            }
            Err(error) => {
                log::warn!("glass {kind:?}: {error:#}");
                false
            }
        }
    }

    fn style_popover_glass(panel: &mut GlassPanel, kind: PanelKind, immersion: bool, theme_mode: model::ThemeMode) {
        match theme_mode {
            model::ThemeMode::Light => {
                panel.set_tint(
                    [0.98, 0.99, 1.0],
                    if kind == PanelKind::More { 0.80 } else { 0.86 },
                    [1.0, 1.0, 1.0, 0.85],
                    [0.0, 0.0, 0.0, 0.12],
                );
            }
            model::ThemeMode::Dark => {
                panel.set_tint(
                    [0.06, 0.07, 0.09],
                    if kind == PanelKind::More { 0.86 } else { 0.92 },
                    [1.0, 1.0, 1.0, 0.42],
                    [0.0, 0.0, 0.0, 0.32],
                );
            }
            model::ThemeMode::LiquidGlass => {
                if immersion {
                    panel.set_tint(
                        [0.20, 0.22, 0.26],
                        if kind == PanelKind::More { 0.14 } else { 0.17 },
                        [1.0, 1.0, 1.0, 0.62],
                        [1.0, 1.0, 1.0, 0.14],
                    );
                } else {
                    panel.set_tint(
                        [0.18, 0.20, 0.24],
                        if kind == PanelKind::More { 0.16 } else { 0.20 },
                        [1.0, 1.0, 1.0, 0.64],
                        [1.0, 1.0, 1.0, 0.14],
                    );
                }
            }
        }
    }

    fn panel_rect(&self, anchor: Box4, size: (f64, f64), viewport: (f64, f64)) -> (GlassRect, Side) {
        let preferred = if (anchor.y as f64) >= size.1 + 16.0 || (anchor.y as f64) >= viewport.1 * 0.4 {
            Some(Side::Above)
        } else {
            None
        };
        place_popover(
            GlassRect {
                left: anchor.x as f64,
                top: anchor.y as f64,
                width: anchor.w as f64,
                height: anchor.h as f64,
            },
            size,
            viewport,
            preferred,
        )
    }
}

#[cfg(feature="qa-capture")]
struct CornerBackdrop;
#[cfg(feature="qa-capture")]
impl Render for CornerBackdrop {
    fn render(&mut self,_:&mut Window,_:&mut Context<Self>)->impl IntoElement{
        div().size_full().bg(rgb(0xffffff)).child(canvas(|_,_,_|{},|bounds,_,window,_|{
            let w=f32::from(bounds.size.width);let h=f32::from(bounds.size.height);
            for (bx,by) in [(0.,0.),(w-64.,0.),(0.,h-64.),(w-64.,h-64.)]{
                for y in 0..16 {for x in 0..16 {
                    chrome::fill_round(window,bx+x as f32*4.,by+y as f32*4.,4.,4.,0.,Hsla::from(rgb(if (x+y)%2==0{0x181820}else{0xeed3ed})));
                }}
            }
        }).size_full())
    }
}

impl Render for PlayerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_minimize {
            self.pending_minimize = false;
            window.minimize_window();
        }
        if self.pending_quit {
            self.pending_quit = false;
            self.settings.finish_edit(self.now_ms(), true);
            if let Some(child) = self.settings_child.take() {
                let _ = child.update(cx, |_, w, _| w.remove_window());
            }
            cx.quit();
        }
        if let Some(page) = self.pending_settings_window.take() {
            self.open_settings_window(page, window, cx);
        }
        if self.settings_host.is_some() && !self.settings.open && !self.panel_motion[3].visible(){window.remove_window();return div().into_any_element();}
        if self.settings_host.is_none() {
            if let Some(mode) = self.pending_mode.take() {
                if let Some(child) = self.settings_child.take() {
                    let _ = child.update(cx, |_, w, _| w.remove_window());
                }
                let target_shape = super::transition::WindowShape::from_u8(mode);
                let cur_logical = super::transition::WindowShape::from_mode(
                    self.tool.mode().unwrap_or(Mode::Window),
                    window.is_fullscreen(),
                );
                let same_target = cur_logical == target_shape && self.mode_transition.is_none();
                let already_going = self
                    .mode_transition
                    .as_ref()
                    .is_some_and(|t| t.to_shape == target_shape);
                if !same_target && !already_going {
                    let wb = window.bounds();
                    let cur_rect = super::transition::RectF::new(
                        f32::from(wb.origin.x),
                        f32::from(wb.origin.y),
                        f32::from(wb.size.width),
                        f32::from(wb.size.height),
                    );
                    if self.mode_transition.is_none() {
                        self.geometry_memory.record_leaving(cur_logical, cur_rect);
                        if cur_logical == super::transition::WindowShape::Window
                            && !window.is_fullscreen()
                        {
                            self.wide_size = Some((cur_rect.w, cur_rect.h));
                        }
                    }
                    if target_shape == super::transition::WindowShape::Fullscreen
                        && !window.is_fullscreen()
                    {
                        self.pre_fullscreen_mode = match cur_logical {
                            super::transition::WindowShape::Mini => 0,
                            super::transition::WindowShape::Lyrics => 1,
                            _ => 2,
                        };
                    }
                    let screen_area = window.display(cx).map(|d| {
                        let b = d.bounds();
                        super::transition::RectF::new(
                            f32::from(b.origin.x),
                            f32::from(b.origin.y),
                            f32::from(b.size.width),
                            f32::from(b.size.height),
                        )
                    });
                    let work_area = window.display(cx).map(|d| {
                        let b = d.visible_bounds();
                        super::transition::RectF::new(
                            f32::from(b.origin.x),
                            f32::from(b.origin.y),
                            f32::from(b.size.width),
                            f32::from(b.size.height),
                        )
                    });
                    self.close_volume_panel();
                    self.tool.set_more_open(false);
                    self.picker_open = false;
                    self.settings.open = false;
                    self.settings.close_choice();
                    self.chrome_wake_ms = if target_shape == super::transition::WindowShape::Lyrics {
                        -10_000.0
                    } else {
                        self.now_ms()
                    };
                    self.cursor_idle_ms = self.now_ms();
                    self.tool.on_mode_changed(target_shape.to_mode());
                    self.sync_profile_display();
                    let from_shape = self
                        .mode_transition
                        .as_ref()
                        .filter(|t| {
                            t.to_shape != super::transition::WindowShape::Fullscreen
                                || window.is_fullscreen()
                        })
                        .map(|t| t.to_shape)
                        .unwrap_or(cur_logical);
                    let target_area = if target_shape == super::transition::WindowShape::Fullscreen {
                        screen_area.or(work_area)
                    } else {
                        work_area
                    };
                    let target_rect = super::transition::transition_target(
                        cur_rect,
                        from_shape,
                        target_shape,
                        self.wide_size.unwrap_or((1280.0, 840.0)),
                        target_area,
                        self.geometry_memory.wide_bounds,
                        self.geometry_memory.compact_origin,
                    );
                    self.mode_transition = Some(super::transition::ModeTransition::new(
                        from_shape,
                        target_shape,
                        cur_rect,
                        target_rect,
                    ));
                }
            }
        }
        let mut render_mode = self.tool.mode().unwrap_or(Mode::Window);
        let mut content_alpha = 1.0_f32;
        let mut chrome_alpha = 1.0_f32;
        let step_now = self.now_ms();
        let mut render_fullscreen = window.is_fullscreen();
        if let Some(tr) = self.mode_transition.as_mut() {
            let step = tr.step(step_now, window.is_fullscreen());
            render_mode = step.render_mode;
            content_alpha = step.content_alpha;
            chrome_alpha = step.chrome_alpha;
            render_fullscreen = if step.finished {
                tr.to_shape == super::transition::WindowShape::Fullscreen
            } else {
                tr.render_fullscreen(window.is_fullscreen(), step_now)
            };
            if let Some(b) = step.bounds {
                window.set_bounds(gpui_kit::Bounds {
                    origin: point(px(b.x), px(b.y)),
                    size: gpui_kit::size(px(b.w), px(b.h)),
                });
            }
            if step.trigger_fullscreen_toggle {
                window.toggle_fullscreen();
            }
            if step.finished {
                let landed_lyrics = tr.to_shape == super::transition::WindowShape::Lyrics;
                self.mode_transition = None;
                if landed_lyrics {
                    self.chrome_wake_ms = -10_000.0;
                }
            }
        }
        if self.tool.mode() == Some(Mode::Mini)
            && self.pending_mode.is_none()
            && self.mode_transition.is_none()
        {
            let h = if self.settings.open || self.panel_motion[3].visible() {
                650.0
            } else {
                176.0
            };
            if (f32::from(window.viewport_size().height) - h).abs() > 0.5 {
                window.resize(gpui_kit::size(px(380.0), px(h)));
            }
        }
        if self.settings_host.is_none()
            && !window.is_fullscreen()
            && self
                .mode_transition
                .as_ref()
                .is_none_or(|t| !t.involves_fullscreen())
        {
            let allow_resize = !matches!(self.tool.mode(), Some(Mode::Mini | Mode::Lyrics));
            if window.is_resizable() != allow_resize {
                window.set_resizable(allow_resize);
            }
            if self.tool.mode() == Some(Mode::Window)
                && self.cfg.capture_rgba.is_none()
                && self.cfg.scenario.is_none()
            {
                let vp = window.viewport_size();
                let vw = f32::from(vp.width);
                let vh = f32::from(vp.height);
                if vw < 760.0 || vh < 500.0 {
                    window.resize(gpui_kit::size(px(vw.max(760.0)), px(vh.max(500.0))));
                }
            }
        }
        let bounds=window.window_bounds();
        if Some(bounds)!=self.last_window_bounds {log::info!("[window-input] bounds={bounds:?}");self.last_window_bounds=Some(bounds);}
        if let Some(at) = self.cfg.freeze_ms {
            self.playing = false;
            self.position_ms = at;
        }
        let now = self.now_ms();
        let dt = if self.last_ms <= 0.0 {
            0.0
        } else {
            (now - self.last_ms).clamp(0.0, 250.0)
        };
        self.settings.tick(now);
        if let Some(host)=self.settings_host.as_mut(){
            if host.last_config!=self.settings.config{
                let config=self.settings.config.clone();host.last_config=config.clone();
                let _=host.parent.update(cx,|parent,cx|{parent.settings.config=config;parent.sync_profile_display();cx.notify();});
            }
            if host.parent.upgrade().is_none(){self.settings.open=false;}
        }
        let live_now = if self.source == Source::Live { Self::live_now_ms() } else { now };
        self.advance(now, live_now);
        if self.settings_host.is_none() {
            if let Some(child_view) = self.settings_child_view.clone() {
                let push_config = self.sync_settings_child_config;
                self.sync_settings_child_config = false;
                let config = self.settings.config.clone();
                let active_player = self.settings.active_player.clone();
                let profile = self.settings.profile;
                let display_mode_label = self.settings.display_mode_label;
                let track_artist = self
                    .live
                    .as_ref()
                    .and_then(|l| l.track.as_ref())
                    .map(oly_engine::strings::primary_artist)
                    .unwrap_or_default();
                let differs = push_config
                    || child_view.upgrade().is_some_and(|v| {
                        let r = v.read(cx);
                        r.settings.active_player != active_player
                            || r.settings.current_track_artist != track_artist
                            || r.settings.profile != profile
                            || r.settings.display_mode_label != display_mode_label
                    });
                if differs {
                    let _ = child_view.update(cx, |v, cx| {
                        if push_config {
                            v.settings.config = config.clone();
                            if let Some(host) = v.settings_host.as_mut() {
                                host.last_config = config.clone();
                            }
                        }
                        v.settings.profile = profile;
                        v.settings.display_mode_label = display_mode_label;
                        v.sync_profile_display();
                        v.settings.active_player = active_player;
                        v.settings.current_track_artist = track_artist;
                        cx.notify();
                    });
                }
            } else {
                self.sync_settings_child_config = false;
            }
        }
        self.update_sync();
        self.frames += 1;

        // The scripted pass drives the very same entry points the pointer and
        // keyboard listeners call, so the recording shows the wiring rather
        // than a set of fields being assigned.
        self.apply_scenario(now, window, cx);

        self.cadence.observe(window, now);

        // An offscreen capture pins the clock for one painted frame, then
        // reads that frame's scene back on the next pass. The two passes are
        // what make the PNG the frame at `capture_at_ms` rather than whatever
        // the wall clock had reached by the time the copy ran.
        if self.cfg.capture_rgba.is_some() && !self.captured {
            if self.capture_ready {
                self.captured = true;
                self.take_offscreen_capture(window);
                cx.quit();
            } else {
                // Pin the clock before the warm-up so every frame the capture
                // could read shows the same media time.
                self.playing = false;
                if let Some(at) = self.cfg.capture_at_ms {
                    self.position_ms = at;
                }
                // The background pass needs a few frames before its first
                // present texture has content, and the glyph atlas fills on
                // first use. Capturing frame 1 would copy a cleared target.
                if self.frames >= 30 {
                    self.capture_ready = true;
                }
                window.request_animation_frame();
                cx.notify();
            }
        }
        let target_morph = if self.playing { 1.0 } else { 0.0 };
        let target_cover_scale = if self.playing { 1.0 } else { 0.74 };
        let target_cover_vis = if self.display.show_cover { 1.0 } else { 0.0 };
        if self.frames <= 1 || (self.cfg.capture_rgba.is_some() && !self.playing) {
            self.play_morph = super::interaction::state_icon_spring(target_morph);
            self.cover_scale_spring.set_position(target_cover_scale);
            self.cover_vis_spring.set_position(target_cover_vis);
        } else {
            self.play_morph.set_target_position(target_morph);
            self.play_morph.update(dt / 1000.0);
            self.cover_scale_spring.set_target_position(target_cover_scale);
            self.cover_scale_spring.update(dt / 1000.0);
            self.cover_vis_spring.set_target_position(target_cover_vis);
            self.cover_vis_spring.update(dt / 1000.0);
        }

        // A bounded run writes its evidence and quits, so the harness reads
        // the numbers from the run instead of recomputing them.
        if !self.reported {
            if let Some(limit) = self.cfg.seconds {
                let elapsed=self.scenario.as_ref().map(|s|s.elapsed(now)).unwrap_or(now);
                if elapsed >= limit as f64 * 1000.0 {
                    self.reported = true;
                    self.write_reports();
                    cx.quit();
                }
            }
        }

        if self.motion_needs_clear {
            self.motion_needs_clear = false;
            self.clear_motion_surface(window);
        }
        if self.settings.open
            && (self.settings.modal.is_some() || self.settings.editing_key.is_some())
            && !self.focus.is_focused(window)
        {
            window.focus(&self.focus, cx);
        }
        if self.settings_host.is_none() {
            self.settings.current_track_artist = self
                .live
                .as_ref()
                .and_then(|l| l.track.as_ref())
                .map(oly_engine::strings::primary_artist)
                .unwrap_or_default();
        }
        if let Some(action) = self.settings.pending_action.take() {
            if let Some(host) = self.settings_host.as_mut() {
                let parent = host.parent.clone();
                if let Ok((status, reset_cfg)) = parent.update(cx, |p, cx| {
                    let res = p.execute_settings_action(action);
                    cx.notify();
                    res
                }) {
                    if let Some(fresh) = reset_cfg {
                        self.settings.config = fresh.clone();
                        host.last_config = fresh;
                        self.sync_profile_display();
                    }
                    if !status.is_empty() {
                        self.settings.status = status;
                    }
                }
            } else {
                let (status, _) = self.execute_settings_action(action);
                if !status.is_empty() {
                    self.settings.status = status;
                }
            }
        }
        if let Some(changed_artist) = self.settings.alias_changed_artist.take() {
            if let Some(host) = self.settings_host.as_ref() {
                let _ = host.parent.update(cx, |p, cx| {
                    p.notify_artist_alias_changed(&changed_artist);
                    cx.notify();
                });
            } else {
                self.notify_artist_alias_changed(&changed_artist);
            }
        }

        let mut m = self.metrics_for_mode(render_mode, window, cx);
        let cover_vis = if render_mode == Mode::Window {
            self.cover_vis_spring.position().clamp(0.0, 1.0) as f32
        } else if self.display.show_cover {
            1.0
        } else {
            0.0
        };
        let cover_scale = if render_mode == Mode::Window {
            self.cover_scale_spring.position().clamp(0.5, 1.15) as f32
        } else {
            1.0
        };
        if render_mode == Mode::Window && cover_vis > 0.001 && cover_vis < 0.999 {
            let vp_size = window.viewport_size();
            let (vw, vh) = (f32::from(vp_size.width), f32::from(vp_size.height));
            let screen = cx
                .primary_display()
                .map(|d| {
                    let b = d.bounds();
                    (f32::from(b.size.width), f32::from(b.size.height))
                })
                .unwrap_or((vw, vh));
            let mut d_on = self.display;
            d_on.show_cover = true;
            let mut d_off = self.display;
            d_off.show_cover = false;
            let m_on = model::metrics(vw, vh, screen, window.is_fullscreen(), &d_on);
            let m_off = model::metrics(vw, vh, screen, window.is_fullscreen(), &d_off);
            let lx = m_off.lyrics.x + (m_on.lyrics.x - m_off.lyrics.x) * cover_vis;
            let lw = m_off.lyrics.w + (m_on.lyrics.w - m_off.lyrics.w) * cover_vis;
            m.lyric_viewport = Box4::new(lx, m_on.lyrics.y, lw, m_on.lyrics.h);
            m.lyrics = model::lyric_content(m.lyric_viewport, m.font, vw / m.ui_scale.max(0.25), &self.display);
            m.art_height = m_on.art_height;
            let slide_left = -(1.0 - cover_vis) * (m_on.cover.w * 0.48 + m_on.wide_left);
            let col_slide = slide_left;
            m.cover = Box4::new(m_on.cover.x + slide_left, m_on.cover.y, m_on.cover.w, m_on.cover.h);
            m.cover_meta_top = m_on.cover_meta_top;
            m.progress = Box4::new(m_on.progress.x + col_slide, m_on.progress.y, m_on.progress.w, m_on.progress.h);
            m.toolbar = Box4::new(m_on.toolbar.x + col_slide, m_on.toolbar.y, m_on.toolbar.w, m_on.toolbar.h);
        }
        let center_target = if render_mode == Mode::Window && self.lyrics_hidden() {
            1.0
        } else {
            0.0
        };
        if self.frames <= 1 {
            self.lyrics_center.set_position(center_target);
            self.lyrics_reveal_alpha = if render_mode == Mode::Window && self.lyrics_hidden() {
                0.0
            } else {
                1.0
            };
        } else {
            self.lyrics_center.set_target_position(center_target);
            self.lyrics_center.update(dt / 1000.0);
        }
        let center_t = if render_mode == Mode::Window {
            self.lyrics_center.position().clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        if render_mode == Mode::Window && self.frames > 1 {
            let reveal_target = if !self.lyrics_hidden() && center_t <= 0.03 {
                1.0
            } else {
                0.0
            };
            let duration_ms = if reveal_target > 0.5 { 220.0 } else { 140.0 };
            self.lyrics_reveal_alpha = model::step_opacity(
                self.lyrics_reveal_alpha,
                reveal_target,
                dt,
                duration_ms,
                false,
            );
        } else if render_mode != Mode::Window {
            self.lyrics_reveal_alpha = 1.0;
        }
        let lyrics_alpha = if render_mode == Mode::Window {
            super::transition::quintic(self.lyrics_reveal_alpha)
        } else {
            1.0
        };
        let scale = window.scale_factor();
        let phys = (
            (m.win_w * scale).round().max(1.0) as u32,
            (m.win_h * scale).round().max(1.0) as u32,
        );

        let motion_path = self.pending_motion_path.take().and_then(|(generation, path)| {
            (self.display.motion_cover
                && self
                    .live
                    .as_ref()
                    .is_some_and(|l| l.model.generation() == generation))
            .then_some(path)
        });
        let tick_playing = if self.source == Source::Live {
            self.live
                .as_ref()
                .is_some_and(|l| l.model.effective_playing())
        } else {
            self.playing
        };
        let immersion=self.display.immersion_background && (matches!(render_mode,Mode::Window) || self.display.immersion_compact);
        let render_bg = immersion && self.settings_host.is_none();
        if let Some(scene) = self.scene.as_mut() {
            if let Some(path) = motion_path {
                if let Err(error) = scene.open_motion_looped(&path) {
                    log::warn!("live motion cover {}: {error:#}", path.display());
                }
            }
            scene.set_beat(super::scene::BeatConfig {
                active: render_bg,
                enabled: self.display.beat_follow,
                flash_threshold: self.display.beat_flash_threshold,
                flash_strength: self.display.beat_flash_strength,
                wobble_threshold: self.display.beat_wobble_threshold,
                wobble_strength: self.display.beat_wobble_strength,
            });
            if render_bg {
                // Cap blurred immersion background target to 1280px longest edge
                // to save up to 9x VRAM & fragment work on 2K/4K displays.
                let max_edge = phys.0.max(phys.1);
                let (bg_w, bg_h) = if max_edge > 1280 {
                    let s = 1280.0 / max_edge as f32;
                    (
                        ((phys.0 as f32 * s).round() as u32).max(1),
                        ((phys.1 as f32 * s).round() as u32).max(1),
                    )
                } else {
                    phys
                };
                scene.resize(bg_w, bg_h);
            }
            if window.is_visible(){
                if let Err(error) = scene.tick(now, self.position_ms, tick_playing, render_bg) {log::warn!("scene tick: {error:#}");}
            }
        }
        self.sync_background(window, scale, render_bg);
        if self.settings_host.is_none() && self.native_background!=Some(immersion){
            window.set_background_appearance(if immersion{WindowBackgroundAppearance::Transparent}else{WindowBackgroundAppearance::Blurred});
            self.native_background=Some(immersion);
        }
        self.sync_motion(window);
        self.sync_cover_surface(window, dt);

        // Rows first: laying them out is what gives the scroll engine its
        // bounds, and the lyric scrollbar's thumb is a function of those bounds.
        let (mut plan_rows, mut plan_interlude, mut plan_bottom) =
            if matches!(render_mode, Mode::Mini | Mode::Album) || content_alpha <= 0.005 {
                (Vec::new(), None, None)
            } else {
                self.plan_rows(&m, dt)
            };
        let row_fade = chrome_alpha * lyrics_alpha;
        if row_fade < 0.999 {
            for row in &mut plan_rows {
                row.light.base *= row_fade;
                row.light.dark *= row_fade;
                row.light.bright *= row_fade;
                row.translation_alpha *= row_fade;
                row.roman_alpha *= row_fade;
            }
            if let Some(inter) = plan_interlude.as_mut() {
                inter.opacity *= row_fade;
            }
            if let Some(bottom) = plan_bottom.as_mut() {
                bottom.alpha *= row_fade;
            }
        }

        let engine = self.scroll.engine();
        let mut chrome = Chrome::build(
            &m,
            (engine.offset(), engine.min_offset(), engine.max_offset()),
        );
        self.settings.viewport_h = m.win_h;
        match self.tool.mode() {
            Some(Mode::Mini)=>{
                chrome.prev=Box4::new(m.win_w/2.-96.,88.,40.,40.);
                chrome.play=Box4::new(m.win_w/2.-24.,86.,48.,44.);
                chrome.next=Box4::new(m.win_w/2.+56.,88.,40.,40.);
                chrome.more=Box4::new(0.,0.,0.,0.);
                chrome.volume=Box4::new(0.,0.,0.,0.);
                chrome.lyrics_toggle=Box4::new(0.,0.,0.,0.);
            }
            Some(Mode::Album)=>{
                let x=(m.win_w-296.)/2.;let y=m.win_h-56.;
                chrome.volume=Box4::new(x+4.,y+4.,34.,34.);
                chrome.prev=Box4::new(x+60.,y+2.,38.,38.);
                chrome.play=Box4::new(x+124.,y,48.,42.);
                chrome.next=Box4::new(x+196.,y+2.,38.,38.);
                chrome.more=Box4::new(x+258.,y+4.,34.,34.);
                chrome.lyrics_toggle=Box4::new(0.,0.,0.,0.);
            }
            Some(Mode::Lyrics)=>{
                let x=(m.win_w-296.)/2.;let y=m.win_h-106.;
                chrome.volume=Box4::new(x+4.,y+8.,36.,36.);
                chrome.prev=Box4::new(x+60.,y+4.,44.,44.);
                chrome.play=Box4::new(x+120.,y,56.,52.);
                chrome.next=Box4::new(x+192.,y+4.,44.,44.);
                chrome.more=Box4::new(x+256.,y+8.,36.,36.);
                chrome.lyrics_toggle=Box4::new(0.,0.,0.,0.);
            }
            _=>{}
        }
        if render_mode == Mode::Window && center_t > 0.0001 {
            let center_left = ((m.win_w - m.column) * 0.5).max(16.0);
            let shift_x = (center_left - m.wide_left) * center_t;
            m.wide_left += shift_x;
            m.cover.x += shift_x;
            m.progress.x += shift_x;
            m.toolbar.x += shift_x;
            chrome.progress.x += shift_x;
            chrome.volume.x += shift_x;
            chrome.more.x += shift_x;
            chrome.prev.x += shift_x;
            chrome.play.x += shift_x;
            chrome.next.x += shift_x;
            chrome.lyrics_toggle.x += shift_x;
        }
        let compact_auto_hide = matches!(render_mode, Mode::Lyrics | Mode::Album);
        let compact_wake = (now - self.chrome_wake_ms < 3000.0)
            || self.tool.panels().any_open()
            || self.settings.open
            || self.pressed.is_some();
        let album_meta_wake = (now - self.chrome_wake_ms < 5200.0)
            || self.tool.panels().any_open()
            || self.settings.open
            || self.pressed.is_some();
        if compact_auto_hide {
            self.album_meta_alpha = model::step_opacity(
                self.album_meta_alpha,
                if album_meta_wake { 1.0 } else { 0.0 },
                dt,
                if album_meta_wake { 150.0 } else { 360.0 },
                false,
            );
            self.compact_chrome_alpha = model::step_opacity(
                self.compact_chrome_alpha,
                if compact_wake { 1.0 } else { 0.0 },
                dt,
                if compact_wake { 240.0 } else { 300.0 },
                false,
            );
        } else {
            self.compact_chrome_alpha = 1.0;
            self.album_meta_alpha = 1.0;
        }
        let compact_eased = super::transition::quintic(self.compact_chrome_alpha);
        if render_mode == Mode::Album {
            m.cover_meta_top = m.win_h - 72.0 - 72.0 * compact_eased;
        }
        if compact_auto_hide {
            let slide_y = (1.0 - compact_eased) * 14.0;
            chrome.prev.y += slide_y;
            chrome.play.y += slide_y;
            chrome.next.y += slide_y;
            chrome.volume.y += slide_y;
            chrome.more.y += slide_y;
            chrome.progress.y += slide_y;
        }
        let nocover_island = render_mode == Mode::Window && !self.display.show_cover && self.settings_host.is_none();
        if nocover_island && cover_vis <= 0.01 {
            if self.cover_gone_at_ms.is_none() {
                self.cover_gone_at_ms = Some(now);
            }
        } else {
            self.cover_gone_at_ms = None;
        }
        let island_ready = nocover_island
            && cover_vis <= 0.01
            && self.cover_gone_at_ms.is_some_and(|t| now - t >= 200.0);
        let island_w = 520.0_f32.min((m.win_w - 220.0).max(280.0));
        let island_h = 54.0_f32;
        let island_x = (m.win_w - island_w) * 0.5;
        let island_wake_zone = Box4::new(island_x - 24.0, 0.0, island_w + 48.0, 76.0);
        let island_awake = island_ready
            && ((now - self.island_wake_ms < 3600.0)
                || self.cursor.is_some_and(|(cx, cy)| island_wake_zone.contains(cx, cy))
                || self.tool.panels().any_open()
                || self.settings.open
                || self.pressed.is_some());
        self.island_drop = model::step_opacity(
            self.island_drop,
            if island_awake { 1.0 } else { 0.0 },
            dt,
            if island_awake { 280.0 } else { 220.0 },
            false,
        );
        let island_plan = if nocover_island && self.island_drop > 0.005 {
            let eased = super::transition::quintic(self.island_drop);
            let iy = -island_h + (island_h + 14.0) * eased;
            let ir = Box4::new(island_x, iy, island_w, island_h);
            chrome.more = Box4::new(ir.right() - 36.0, iy + 10.0, 28.0, 28.0);
            chrome.volume = Box4::new(ir.right() - 68.0, iy + 10.0, 28.0, 28.0);
            chrome.next = Box4::new(ir.right() - 102.0, iy + 9.0, 30.0, 30.0);
            chrome.play = Box4::new(ir.right() - 140.0, iy + 7.0, 34.0, 34.0);
            chrome.prev = Box4::new(ir.right() - 174.0, iy + 9.0, 30.0, 30.0);
            chrome.lyrics_toggle = Box4::default();
            chrome.progress = Box4::new(ir.x + 16.0, iy + island_h - 8.5, (ir.w - 32.0).max(1.0), 2.5);
            Some((ir, eased))
        } else {
            if nocover_island && cover_vis <= 0.01 {
                chrome.prev = Box4::default();
                chrome.play = Box4::default();
                chrome.next = Box4::default();
                chrome.volume = Box4::default();
                chrome.more = Box4::default();
                chrome.lyrics_toggle = Box4::default();
                chrome.progress = Box4::default();
            }
            None
        };
        self.chrome = chrome.clone();

        let span = (engine.max_offset() - engine.min_offset()).max(0.0);
        let scrollable = !self.rows.is_empty()
            && !matches!(render_mode, Mode::Mini | Mode::Album)
            && !(render_mode == Mode::Window && self.lyrics_hidden())
            && lyrics_alpha > 0.01
            && span > 2.0
            && m.lyrics.h > 20.0;
        let thumb_held = scrollable && self.drag_thumb.is_some();
        let pull = if thumb_held {
            self.press_gesture.delta()
        } else {
            [0.0; 2]
        };
        self.thumb_elastic.step(pull, thumb_held, dt);
        let pressure = self.thumb_elastic.pressure().clamp(0.0, 1.0);
        let reveal = scrollable
            && (thumb_held
                || self.thumb_elastic.pending()
                || self
                    .cursor
                    .is_some_and(|(x, y)| m.lyric_viewport.contains(x, y) || x >= m.win_w - 36.0));
        let mut scroll_glass = None;
        if reveal && (thumb_held || pressure > 0.02) {
            let size = (32.0, (chrome.scroll_thumb.h + 20.0).ceil().max(24.0) as f64);
            if self.ensure_glass(window, PanelKind::Thumb, size, scale) {
                let shape = super::scrollbar::held_shape(
                    chrome.scroll_thumb.h,
                    pressure,
                    self.thumb_elastic.pull(),
                );
                if let Some(panel) = self.thumb_glass.as_mut() {
                    panel.set_shape(
                        GlassRect {
                            left: shape.x as f64 * scale as f64,
                            top: shape.y as f64 * scale as f64,
                            width: shape.w as f64 * scale as f64,
                            height: shape.h as f64 * scale as f64,
                        },
                        shape.w as f64 * 0.5 * scale as f64,
                    );
                    panel.set_liquid(scale, (0.85 * pressure).clamp(0.15, 0.92), pressure);
                    panel.set_light([0.15, 0.1, 0.8]);
                    scroll_glass = Some(PlanPanel {
                        kind: PanelKind::Thumb,
                        content_alpha: 1.0,
                        surface_alpha: pressure,
                        rect: GlassRect {
                            left: chrome.scroll_track.x as f64 - 14.0,
                            top: chrome.scroll_thumb.y as f64 - 10.0,
                            width: size.0,
                            height: size.1,
                        },
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
            }
        }
        let prog_h = chrome.progress.h.clamp(14.0, 18.0);
        let prog_y = chrome.progress.y + (chrome.progress.h - prog_h) * 0.5;
        let mut hot: Vec<(Hot, Box4)> = vec![
            (Hot::Prev, chrome.prev),
            (Hot::Play, chrome.play),
            (Hot::Next, chrome.next),
            // The rail is `.lg-slider` at `width: calc(100% + 26px);
            // margin-inline: -13px`, so the pointer target overhangs the
            // progress column exactly like the painted bar. The seek fraction
            // is then measured over the same box the fill is drawn across.
            (
                Hot::Progress,
                Box4::new(
                    chrome.progress.x - 13.0,
                    prog_y,
                    chrome.progress.w + 26.0,
                    prog_h,
                ),
            ),
        ];
        if chrome.more.w > 0.0 && chrome.more.h > 0.0 {
            hot.push((Hot::More, chrome.more));
        }
        if chrome.volume.w > 0.0 && chrome.volume.h > 0.0 {
            hot.push((Hot::Volume, chrome.volume));
        }
        if chrome.lyrics_toggle.w > 0.0 && chrome.lyrics_toggle.h > 0.0 {
            hot.push((Hot::LyricsToggle, chrome.lyrics_toggle));
        }
        if scrollable {
            hot.push((
                Hot::Thumb,
                super::scrollbar::hit(chrome.scroll_track, chrome.scroll_thumb),
            ));
        }

        if matches!(self.tool.mode(), Some(Mode::Lyrics | Mode::Album)) && self.compact_chrome_alpha < 0.08 {hot.retain(|(h,_)|!matches!(h,Hot::Prev|Hot::Play|Hot::Next|Hot::Volume|Hot::More|Hot::LyricsToggle|Hot::Progress));}
        if nocover_island && self.island_drop < 0.08 {
            hot.retain(|(h, _)| !matches!(h, Hot::Prev | Hot::Play | Hot::Next | Hot::Volume | Hot::More | Hot::LyricsToggle | Hot::Progress));
        }
        if render_mode == Mode::Mini && m.art_height > 0.0 {
            hot.push((Hot::AlbumEnter, m.cover));
        }
        let fold_box = super::details::fold_hit_box(m.win_w, m.win_h);
        let compact_fold_allowed = self.settings_host.is_none()
            && matches!(self.tool.mode(), Some(Mode::Mini | Mode::Lyrics));
        if compact_fold_allowed && super::details::fold_visible(self.cursor, m.win_w, m.win_h) {
            hot.push((Hot::Fold, fold_box));
        }
        let cap_allowed = self.settings_host.is_none();
        let cap_compact = matches!(render_mode, Mode::Mini | Mode::Album | Mode::Lyrics);
        let (cap_zone, [cap_min, cap_fs, cap_close]) = window_caption_rects(m.win_w, cap_compact);
        let album_ret = album_return_rect();
        let album_ret_zone = Box4::new(0.0, 0.0, 52.0, 48.0);
        let cap_hovered = cap_allowed
            && (self.cursor.is_some_and(|(cx, cy)| {
                (cap_zone.contains(cx, cy) || (render_mode == Mode::Album && album_ret_zone.contains(cx, cy)))
                    && !self.panel_bounds.iter().any(|b| b.contains(cx, cy))
            }) || matches!(
                self.pressed,
                Some(Hot::WindowMinimize | Hot::WindowFullscreen | Hot::WindowClose | Hot::AlbumReturn)
            ));
        if cap_allowed {
            self.window_controls_alpha = model::step_opacity(
                self.window_controls_alpha,
                if cap_hovered { 1.0 } else { 0.0 },
                dt,
                160.0,
                false,
            );
        } else {
            self.window_controls_alpha = 0.0;
        }
        if cap_allowed && (cap_hovered || self.window_controls_alpha > 0.05) {
            hot.push((Hot::WindowMinimize, cap_min));
            hot.push((Hot::WindowFullscreen, cap_fs));
            hot.push((Hot::WindowClose, cap_close));
            if render_mode == Mode::Album {
                hot.push((Hot::AlbumReturn, album_ret));
            }
        }
        if !self.display.show_progress || !self.can_seek_live() {hot.retain(|(h,_)|*h!=Hot::Progress);}
        let (more_open, volume_open) = self.open_panels();
        let picker_open = self.tool.panels().more_open && self.more_open_sub();
        // A child replaces its parent surface; sampling both compounds the tint.
        let more_open = more_open && !picker_open;
        // Replacement panels never leave a second blurred surface underneath.
        if picker_open {self.panel_motion[0]=PanelMotion::default();}
        if more_open {self.panel_motion[2]=PanelMotion::default();}
        if volume_open {self.panel_motion[0]=PanelMotion::default();self.panel_motion[2]=PanelMotion::default();}
        if more_open || picker_open {self.panel_motion[1]=PanelMotion::default();}
        self.panel_motion[0].step(more_open,dt);
        self.panel_motion[1].step(volume_open,dt);
        self.panel_motion[2].step(picker_open,dt);
        self.panel_motion[3].step(self.settings.open,dt);
        let mut panel_elastic_pending = false;
        let raw_panel_drag = self.press_gesture.delta();
        for (idx, elastic) in self.panel_elastics.iter_mut().enumerate() {
            let held = self.drag_panel == Some(idx);
            let delta = if held {
                [raw_panel_drag[0] * 1.15, raw_panel_drag[1] * 1.15]
            } else {
                [0.0, 0.0]
            };
            elastic.step(delta, held, dt);
            panel_elastic_pending |= elastic.pending();
        }
        let refresh_searching = self.picker_refresh_active
            || self.live.as_ref().is_some_and(|l| l.status == "正在搜索歌词…");
        if refresh_searching {
            self.picker_refresh_spinning = true;
        }
        if !picker_open && !refresh_searching {
            self.picker_refresh_angle = 0.0;
            self.picker_refresh_spinning = false;
        } else if self.picker_refresh_spinning || self.picker_refresh_angle > 0.0 {
            let step = std::f32::consts::TAU * 1.45 * (dt.clamp(0.0, 100.0) as f32 / 1000.0);
            let next = self.picker_refresh_angle + step;
            if next >= std::f32::consts::TAU {
                if refresh_searching {
                    self.picker_refresh_angle = next % std::f32::consts::TAU;
                } else {
                    self.picker_refresh_angle = 0.0;
                    self.picker_refresh_spinning = false;
                }
            } else {
                self.picker_refresh_angle = next;
            }
        }
        let translation_pending=self.translation_motion.step(self.display.show_translation,false,dt);

        log::trace!(
            "panels more={more_open} volume={volume_open} picker={picker_open} glass={}/{}/{}",
            self.more_glass.is_some(),
            self.volume_glass.is_some(),
            self.picker_glass.is_some()
        );

        // Panels are glass surfaces over the frame that is already painted.
        let mut panels: Vec<PlanPanel> = Vec::new();
        let mut panel_drag_targets: Vec<(usize, Box4)> = Vec::new();
        let panel_radius = if self.settings_host.is_some(){8.0}else{self.display.corner_radius.clamp(8.0, 28.0)};
        let viewport = (m.win_w as f64, m.win_h as f64);
        let current_window_mode: u8 = if window.is_fullscreen() {
            3
        } else {
            match self.tool.mode().unwrap_or(Mode::Window) {
                Mode::Mini => 0,
                Mode::Lyrics => 1,
                _ => 2,
            }
        };
        let mut active_mode_u8 = self.staged_mode.unwrap_or(current_window_mode);
        let mut mode_well_rect = Box4::default();
        let mut mode_lens_rect = Box4::default();
        let mut mode_swell = 0.0;
        let mut mode_content_alpha = 1.0_f32;
        let show_media = self.has_multiple_media_sessions();
        if self.panel_motion[0].visible() {
            let size = input::more_size_with_media(m.win_h, show_media);
            let (mut rect, side) = self.panel_rect(chrome.more, size, viewport);
            let aim=self.cursor.map(|(x,y)|((x as f64-rect.left)/rect.width,(y as f64-rect.top)/rect.height));
            let (lx,ly,energy)=match aim {Some((x,y)) if (-0.15..=1.15).contains(&x)&&(-0.15..=1.15).contains(&y)=>(x,y,1.0),_=>(0.25,0.05,0.35)};
            self.panel_motion[0].aim_light(lx,ly,energy);
            let light=self.panel_motion[0].light();
            let presentation=&self.panel_motion[0];
            rect.top += (if side==Side::Above {1.0} else {-1.0})*presentation.offset() as f64;
            let p0 = self.panel_elastics[0].pull();
            rect.left += p0[0] as f64;
            rect.top += p0[1] as f64;
            if more_open {
                panel_drag_targets.push((0, Box4::new(rect.left as f32, rect.top as f32, rect.width as f32, rect.height as f32)));
            }
            let content_alpha=presentation.content_alpha();
            let surface_alpha=presentation.surface_alpha();
            let optical_motion=(1.0-presentation.progress()).abs().min(1.0) as f32;
            mode_content_alpha = content_alpha;
            let cell_w = (size.0 as f32 - 20.0) / 4.0;
            let mode_held = matches!(self.pressed, Some(Hot::MenuMode(_)));
            let target_mode_u8 = if mode_held {
                current_window_mode
            } else {
                self.staged_mode.unwrap_or(current_window_mode)
            };
            self.mode_lens.step(
                target_mode_u8 as f32 * cell_w + 4.0,
                (cell_w - 8.0).max(16.0),
                mode_held,
                dt,
            );
            if !mode_held {
                if let Some(staged) = self.staged_mode {
                    if self.mode_lens.settled_near_target() {
                        self.staged_mode = None;
                        if staged != current_window_mode {
                            self.pending_mode = Some(staged);
                        }
                    }
                }
            }
            let base_well = Box4::new(
                rect.left as f32 + 8.0,
                rect.top as f32 + 6.0,
                rect.width as f32 - 16.0,
                32.0,
            );
            mode_well_rect = self.mode_lens.well_rect(base_well);
            mode_lens_rect = self.mode_lens.rect(
                rect.left as f32 + 10.0,
                rect.top as f32 + 10.0,
                24.0,
                Feedback::default(),
            );
            mode_swell = self.mode_lens.swell();
            let track_x = rect.left as f32 + 10.0;
            let visual_slot = (((mode_lens_rect.centre().0 - track_x) / cell_w).floor() as i32)
                .clamp(0, 3) as u8;
            active_mode_u8 = if mode_held || self.mode_lens.pending() || self.staged_mode.is_some() {
                visual_slot
            } else {
                current_window_mode
            };
            if self.ensure_glass(window, PanelKind::More, size, scale) {
                if let Some(panel) = self.more_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::More, immersion, self.display.theme_mode);
                    panel.set_radius(panel_radius as f64 * scale as f64);
                    panel.set_liquid(scale, surface_alpha, optical_motion);
                    panel.set_light(light);
                    panels.push(PlanPanel {
                        content_alpha,
                        surface_alpha,
                        kind: PanelKind::More,
                        rect,
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
                if more_open {
                    let r=Box4::new(rect.left as f32,rect.top as f32,rect.width as f32,rect.height as f32);
                    let row_count = if show_media { 5 } else { 4 };
                    for i in 0..4 {hot.push((Hot::MenuMode(i),input::mode_rect(r,i)));}
                    for i in 1..row_count {hot.push((Hot::MoreRow(i as u8),input::row_rect_count(r,i as u8,row_count)));}
                    if self.sync.enabled(){hot.push((Hot::SyncMinus,input::sync_rect_count(r,false,row_count)));hot.push((Hot::SyncPlus,input::sync_rect_count(r,true,row_count)));}
                }
            }
        } else if let Some(staged) = self.staged_mode.take() {
            if staged != current_window_mode {
                self.pending_mode = Some(staged);
            }
        }
        if self.panel_motion[1].visible() {
            let size = (240.0, 96.0);
            let (mut rect, side) = place_popover(GlassRect {left:chrome.volume.x as f64,top:chrome.volume.y as f64,width:chrome.volume.w as f64,height:chrome.volume.h as f64},size,viewport,Some(Side::Above));
            let aim=self.cursor.map(|(x,y)|((x as f64-rect.left)/rect.width,(y as f64-rect.top)/rect.height));
            let (lx,ly,energy)=match aim {Some((x,y)) if (-0.15..=1.15).contains(&x)&&(-0.15..=1.15).contains(&y)=>(x,y,1.0),_=>(0.25,0.05,0.35)};
            self.panel_motion[1].aim_light(lx,ly,energy);
            let light=self.panel_motion[1].light();
            let presentation=&self.panel_motion[1];
            rect.top += (if side==Side::Above {1.0} else {-1.0})*presentation.offset() as f64;
            let p1 = self.panel_elastics[1].pull();
            rect.left += p1[0] as f64;
            rect.top += p1[1] as f64;
            if volume_open {
                panel_drag_targets.push((1, Box4::new(rect.left as f32, rect.top as f32, rect.width as f32, rect.height as f32)));
            }
            let content_alpha=presentation.content_alpha();
            let surface_alpha=presentation.surface_alpha();
            let optical_motion=(1.0-presentation.progress()).abs().min(1.0) as f32;
            if self.ensure_glass(window, PanelKind::Volume, size, scale) {
                if let Some(panel) = self.volume_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::Volume, immersion, self.display.theme_mode);
                    panel.set_radius(panel_radius as f64 * scale as f64);
                    panel.set_liquid(scale, surface_alpha, optical_motion);
                    panel.set_light(light);
                    panels.push(PlanPanel {
                        content_alpha,
                        surface_alpha,
                        kind: PanelKind::Volume,
                        rect,
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
                if volume_open { hot.push((
                    Hot::VolumeTrack,
                    Box4::new(
                        rect.left as f32 + 14.0,
                        rect.top as f32 + 52.0,
                        rect.width as f32 - 28.0,
                        20.0,
                    ),
                )); }
            }
        }
        if self.panel_motion[2].visible() {
            let is_cover = self.picker_kind == PickerKind::Cover;
            let list = if is_cover { Vec::new() } else { self.candidates() };
            let visible_rows = self
                .picker_locked_rows
                .filter(|_| self.picker_refresh_active)
                .unwrap_or_else(|| list.len().clamp(1, 4));
            let size = if is_cover {
                (
                    268.0_f64.min(viewport.0 - 16.0),
                    (VERSION_HEADER as f64 + 2.0 * COVER_SWITCH_ROW as f64 + 8.0).min(viewport.1 - 16.0),
                )
            } else {
                (
                    420.0_f64.min(viewport.0 - 16.0),
                    (VERSION_HEADER as f64 + visible_rows as f64 * VERSION_ROW as f64 + 4.0).min(viewport.1 - 16.0),
                )
            };
            let (mut rect, side) = self.panel_rect(chrome.more, size, viewport);
            let aim=self.cursor.map(|(x,y)|((x as f64-rect.left)/rect.width,(y as f64-rect.top)/rect.height));
            let (lx,ly,energy)=match aim {Some((x,y)) if (-0.15..=1.15).contains(&x)&&(-0.15..=1.15).contains(&y)=>(x,y,1.0),_=>(0.25,0.05,0.35)};
            self.panel_motion[2].aim_light(lx,ly,energy);
            let light=self.panel_motion[2].light();
            let presentation=&self.panel_motion[2];
            rect.top += (if side==Side::Above {1.0} else {-1.0})*presentation.offset() as f64;
            let p2 = self.panel_elastics[2].pull();
            rect.left += p2[0] as f64;
            rect.top += p2[1] as f64;
            if picker_open {
                panel_drag_targets.push((2, Box4::new(rect.left as f32, rect.top as f32, rect.width as f32, rect.height as f32)));
            }
            let content_alpha=presentation.content_alpha();
            let surface_alpha=presentation.surface_alpha();
            let optical_motion=(1.0-presentation.progress()).abs().min(1.0) as f32;
            if self.ensure_glass(window, PanelKind::Picker, size, scale) {
                if let Some(panel) = self.picker_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::Picker, immersion, self.display.theme_mode);
                    panel.set_radius(panel_radius as f64 * scale as f64);
                    panel.set_liquid(scale, surface_alpha, optical_motion);
                    panel.set_light(light);
                    panels.push(PlanPanel {
                        content_alpha,
                        surface_alpha,
                        kind: PanelKind::Picker,
                        rect,
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
                if !is_cover {
                    self.picker_capacity=(((rect.height as f32-VERSION_HEADER)/VERSION_ROW).floor() as usize).min(4);
                    self.picker_start=self.picker_start.min(list.len().saturating_sub(self.picker_capacity));
                }
                if picker_open {
                    if is_cover {
                        for i in 0..2u8 {
                            hot.push((
                                Hot::CoverSwitch(i),
                                cover_switch_row_rect(rect.left as f32, rect.top as f32, rect.width as f32, i),
                            ));
                        }
                    } else {
                        if self.picker_kind == PickerKind::Lyrics {
                            hot.push((Hot::PickerImport, picker_import_rect(rect)));
                            hot.push((Hot::PickerRefresh, picker_refresh_rect(rect)));
                        }
                        for (i,entry) in list.iter().enumerate().skip(self.picker_start).take(self.picker_capacity) {
                            let row_r=Box4::new(rect.left as f32+8.,rect.top as f32+VERSION_HEADER+(i-self.picker_start) as f32*VERSION_ROW,rect.width as f32-16.,VERSION_ROW-4.);
                            hot.push((Hot::Candidate(i),row_r));
                            if !entry.has_translation && !entry.smart_translation_key.is_empty() {
                                hot.push((Hot::SmartTranslation(i),picker_smart_btn_rect(row_r)));
                            }
                        }
                    }
                }
            }
        }
        let mut settings_paint=None;
        if self.panel_motion[3].visible() {
            let r=self.settings.panel_rect(m.win_w,m.win_h);
            self.settings.scroll=self.settings.scroll.clamp(0.0,self.settings.max_scroll(r));
            let motion=&self.panel_motion[3];
            let content_alpha=motion.content_alpha();let surface_alpha=motion.surface_alpha();
            let offset=motion.offset();let optical_motion=(1.0-motion.progress()).abs().min(1.0) as f32;
            let r=if self.settings_host.is_none() {
                let displacement=(r.x+r.w+16.0)*(offset/14.0).clamp(0.0,1.0);
                let p3 = self.panel_elastics[3].pull();
                Box4::new(r.x - displacement + p3[0], r.y + p3[1], r.w, r.h)
            } else {
                let displacement=r.w*(offset/14.0).clamp(0.0,1.0);
                let direction=match self.settings_host.as_ref().map(|h|h.side){Some(super::details::PanelSide::Left)=>1.,_=>-1.};
                Box4::new(r.x+direction*displacement,r.y,r.w,r.h)
            };
            if self.settings_host.is_none() && self.settings.open {
                panel_drag_targets.push((3, r));
            }
            let tab_held = matches!(self.pressed, Some(Hot::SettingsPage(_)));
            self.settings.step_tabs(r, tab_held, dt);
            self.settings.step_segmented(r, dt);
            let rect=GlassRect{left:r.x as f64,top:r.y as f64,width:r.w as f64,height:r.h as f64};
            let aim = self.cursor.map(|(x, y)| (((x - r.x) / r.w) as f64, ((y - r.y) / r.h) as f64));
            let (lx, ly, energy) = match aim {
                Some((x, y)) if (-0.15..=1.15).contains(&x) && (-0.15..=1.15).contains(&y) => (x, y, 0.65),
                _ => (0.25, 0.05, 0.35),
            };
            self.panel_motion[3].aim_light(lx, ly, energy);
            let light=self.panel_motion[3].light();
            if self.ensure_glass(window,PanelKind::Settings,(r.w as f64,r.h as f64),scale) {
                if let Some(panel)=self.settings_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::Settings, immersion, self.display.theme_mode);
                    panel.set_radius(panel_radius as f64 * scale as f64);
                    panel.set_liquid(scale,surface_alpha,optical_motion);panel.set_light(light);
                    panels.push(PlanPanel{content_alpha,surface_alpha,kind:PanelKind::Settings,rect,surface:panel.surface(),snapshot:panel.snapshot(),hook:panel.hook()});
                    if self.settings.open {
                        if let Some(modal) = &self.settings.modal {
                            let mp = self.panel_elastics[4].pull();
                            let (_, _, _, cancel, save) = Settings::modal_rects_with_offset(r, modal, mp);
                            hot.push((Hot::ModalCancel, cancel));
                            if save.w > 0.0 {
                                hot.push((Hot::ModalSave, save));
                            }
                        } else {
                            hot.push((Hot::SettingsClose,Settings::close_rect_for(self.settings.kind,r)));
                            for i in 0..self.settings.pages().len() as u8 {hot.push((Hot::SettingsPage(i),self.settings.tab_rect_for(r,i as usize)));}
                            let dp = self.panel_elastics[4].pull();
                            let mut drop_layout = self.settings.dropdown_layout(r);
                            if let Some(d) = drop_layout.as_mut() {
                                d.translate(dp[0], dp[1]);
                                panel_drag_targets.push((4, d.rect));
                            }
                            let hovered_opt = drop_layout.as_ref().and_then(|d| {
                                self.cursor.and_then(|(cx, cy)| {
                                    if !d.rect.contains(cx, cy) || d.options.is_empty() {
                                        return None;
                                    }
                                    let idx = ((cy - (d.rect.y + 5.0) + d.scroll) / 31.0).floor() as isize;
                                    Some(idx.clamp(0, d.options.len() as isize - 1) as usize)
                                })
                            });
                            if drop_layout.is_some() {
                                let held = matches!(self.pressed, Some(Hot::DropdownOption(_)));
                                self.settings.step_dropdown(r, hovered_opt, held, dt);
                            }
                            for (i,item,row) in self.settings.rows(r) {
                                hot.push((match item.control.slider_bounds() {Some(_)=>Hot::SettingSlider(i as u8),None=>Hot::SettingChoice(i as u8)},match item.control.slider_bounds(){Some(_)=>Box4::new(row.x+8.0,row.y+22.0,row.w-16.0,26.0),None=>row}));
                            }
                            if let Some(d) = drop_layout {
                                let opt_count = d.options.len();
                                for (opt_i, _, _, opt_r, _, _) in &d.options {
                                    let full_h = if *opt_i + 1 < opt_count { opt_r.h + 2.0 } else { opt_r.h };
                                    let top = opt_r.y.max(d.rect.y + 4.0);
                                    let bot = (opt_r.y + full_h).min(d.rect.bottom() - 4.0);
                                    if bot > top + 2.0 {
                                        hot.push((
                                            Hot::DropdownOption(*opt_i as u8),
                                            Box4::new(opt_r.x, top, opt_r.w, bot - top),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    settings_paint=Some(self.settings.presentation(r));
                    if let Some(sp) = settings_paint.as_mut() {
                        if let Some(modal) = &sp.modal {
                            let mp = self.panel_elastics[4].pull();
                            sp.modal_offset = mp;
                            let modal_box = Settings::modal_rects_with_offset(r, modal, mp).0;
                            panel_drag_targets.push((4, modal_box));
                            if self.ensure_glass(window, PanelKind::Dropdown, (modal_box.w as f64, modal_box.h as f64), scale)
                            {
                                if let Some(dp) = self.dropdown_glass.as_mut() {
                                    Self::style_popover_glass(dp, PanelKind::Picker, immersion, self.display.theme_mode);
                                    dp.set_radius(14.0 * scale as f64);
                                    dp.set_liquid(scale, content_alpha, 0.25);
                                    dp.set_light(light);
                                    sp.modal_glass = Some((
                                        GlassRect {
                                            left: modal_box.x as f64,
                                            top: modal_box.y as f64,
                                            width: modal_box.w as f64,
                                            height: modal_box.h as f64,
                                        },
                                        dp.surface(),
                                        dp.snapshot(),
                                        dp.hook(),
                                    ));
                                }
                            }
                        }
                        if let Some(d) = sp.dropdown.as_mut() {
                            let dp = self.panel_elastics[4].pull();
                            d.translate(dp[0], dp[1]);
                            if self.ensure_glass(window, PanelKind::Dropdown, (d.rect.w as f64, d.rect.h as f64), scale) {
                                if let Some(dp) = self.dropdown_glass.as_mut() {
                                    Self::style_popover_glass(dp, PanelKind::Picker, immersion, self.display.theme_mode);
                                    dp.set_radius(12.0 * scale as f64);
                                    dp.set_liquid(scale, content_alpha, 0.25);
                                    dp.set_light(light);
                                    d.glass = Some((
                                        GlassRect {
                                            left: d.rect.x as f64,
                                            top: d.rect.y as f64,
                                            width: d.rect.w as f64,
                                            height: d.rect.h as f64,
                                        },
                                        dp.surface(),
                                        dp.snapshot(),
                                        dp.hook(),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        if self.settings_host.is_some(){hot.retain(|(h,_)|matches!(h,Hot::SettingsClose|Hot::SettingsPage(_)|Hot::SettingSlider(_)|Hot::SettingChoice(_)|Hot::DropdownOption(_)|Hot::ModalCancel|Hot::ModalSave));}
        self.panel_drag_targets = panel_drag_targets;
        self.panel_bounds=panels.iter().map(|p|Box4::new(p.rect.left as f32,p.rect.top as f32,p.rect.width as f32,p.rect.height as f32)).collect();
        let hovered = self.cursor.and_then(|(x,y)| pointer_hit(&hot,&self.panel_bounds,x,y));
        self.feedback.retain(|(h,_)| hot.iter().any(|(key,_)| key == h));
        self.elastic.retain(|(h,_)|hot.iter().any(|(key,_)|key==h));
        let mut feedback_pending = false;
        for (key, _) in &hot {
            if !self.feedback.iter().any(|(h,_)| h == key) { self.feedback.push((*key, Feedback::default())); }
            let value = &mut self.feedback.iter_mut().find(|(h,_)| h == key).unwrap().1;
            let held=self.pressed==Some(*key);
            feedback_pending |= value.step(hovered == Some(*key), held, dt);
            if !self.elastic.iter().any(|(h,_)|h==key) {self.elastic.push((*key,Elastic::default()));}
            let response=&mut self.elastic.iter_mut().find(|(h,_)|h==key).unwrap().1;
            let raw_delta = self.press_gesture.delta();
            response.step(raw_delta, held, dt);
            value.pull=response.pull();value.squeeze=response.squeeze();value.press=response.pressure();
            if let Some((x,y))=self.cursor {
                let r=hot.iter().find(|(h,_)|h==key).unwrap().1;
                value.focus=[((x-r.x)/r.w.max(1.0)*2.0-1.0).clamp(-1.5,1.5),((y-r.y)/r.h.max(1.0)*2.0-1.0).clamp(-1.5,1.5)];
            }
            feedback_pending |= response.pending();
        }
        let chrome_visible = !matches!(self.tool.mode(), Some(Mode::Lyrics | Mode::Album))
            || self.compact_chrome_alpha > 0.005;
        let hover_viewport = Box4::new(m.lyric_viewport.x, m.lyric_viewport.y,
            m.lyric_viewport.w, (m.lyric_viewport.h - if self.tool.mode()==Some(Mode::Lyrics) && chrome_visible {164.} else {0.}).max(0.));
        let hovered_lyric = if lyrics_alpha <= 0.01 {
            None
        } else {
            self.cursor.filter(|&(x,y)|
                hover_viewport.contains(x,y) && hovered.is_none()
                && !self.panel_bounds.iter().any(|b|b.contains(x,y)))
                .and_then(|(x,y)| plan_rows.iter().rev().filter_map(|r|r.hover_region)
                    .find(|(_,b)|b.contains(x,y)).map(|(i,_)|i))
        };
        feedback_pending |= self.lyric_hover.step(hovered_lyric, dt);
        let lyric_hover = self.lyric_hover.index.and_then(|i|
            plan_rows.iter().filter_map(|r|r.hover_region).find(|(j,_)|*j==i)
                .map(|(_,b)|(b,self.lyric_hover.alpha * self.display.hover_bg_alpha * chrome_alpha * lyrics_alpha)));
        if lyrics_alpha > 0.01 && (self.source != Source::Live || self.can_seek_live()) {
            let mut lyric_hots = Vec::new();
            for (idx, b) in plan_rows.iter().filter_map(|r| r.hover_region) {
                let x0 = b.x.max(hover_viewport.x);
                let y0 = b.y.max(hover_viewport.y);
                let x1 = b.right().min(hover_viewport.right());
                let y1 = b.bottom().min(hover_viewport.bottom());
                if x1 > x0 && y1 > y0 {
                    lyric_hots.push((Hot::LyricRow(idx), Box4::new(x0, y0, x1 - x0, y1 - y0)));
                }
            }
            if !lyric_hots.is_empty() {
                hot.splice(0..0, lyric_hots);
            }
        }
        self.hot = hot;
        if !panels.is_empty() {
            self.panels_painted += 1;
        }

        let fb_of = |key: Hot| {
            self.feedback
                .iter()
                .find(|(h, _)| *h == key)
                .map(|(_, f)| *f)
                .unwrap_or_default()
        };
        let scroll_line = if reveal {
            if thumb_held || pressure > 0.02 {
                let shape = super::scrollbar::held_shape(
                    chrome.scroll_thumb.h,
                    pressure,
                    self.thumb_elastic.pull(),
                );
                Some((
                    Box4::new(
                        chrome.scroll_track.x - 14.0 + shape.x,
                        chrome.scroll_thumb.y - 10.0 + shape.y,
                        shape.w,
                        shape.h,
                    ),
                    (0.32 - 0.12 * pressure).max(0.16),
                ))
            } else {
                let hovered_thumb = hovered == Some(Hot::Thumb);
                Some((
                    super::scrollbar::rest_line(
                        chrome.scroll_track,
                        chrome.scroll_thumb,
                        hovered_thumb,
                    ),
                    if hovered_thumb { 0.62 } else { 0.36 } * lyrics_alpha,
                ))
            }
        } else {
            None
        };
        let close_box = settings_paint
            .as_ref()
            .map(|sp| Settings::close_rect_for(sp.kind, sp.rect));
        let mut btn_candidates: Vec<(Hot, Box4, Feedback)> = Vec::new();
        if self.settings_host.is_none() && chrome_visible {
            btn_candidates.push((Hot::Play, chrome.play, fb_of(Hot::Play)));
            btn_candidates.push((Hot::Prev, chrome.prev, fb_of(Hot::Prev)));
            btn_candidates.push((Hot::Next, chrome.next, fb_of(Hot::Next)));
            btn_candidates.push((Hot::More, chrome.more, fb_of(Hot::More)));
            if self.tool.mode() != Some(Mode::Mini) {
                btn_candidates.push((Hot::Volume, chrome.volume, fb_of(Hot::Volume)));
            }
            if chrome.lyrics_toggle.w > 0.0 && chrome.lyrics_toggle.h > 0.0 {
                btn_candidates.push((
                    Hot::LyricsToggle,
                    chrome.lyrics_toggle,
                    fb_of(Hot::LyricsToggle),
                ));
            }
        }
        if let Some(cb) = close_box {
            btn_candidates.push((Hot::SettingsClose, cb, fb_of(Hot::SettingsClose)));
        }
        if self.settings_host.is_none() {
            if self.window_controls_alpha > 0.02 {
                btn_candidates.push((Hot::WindowMinimize, cap_min, fb_of(Hot::WindowMinimize)));
                btn_candidates.push((Hot::WindowFullscreen, cap_fs, fb_of(Hot::WindowFullscreen)));
                btn_candidates.push((Hot::WindowClose, cap_close, fb_of(Hot::WindowClose)));
                if render_mode == Mode::Album {
                    btn_candidates.push((Hot::AlbumReturn, album_ret, fb_of(Hot::AlbumReturn)));
                }
            }
        }
        let active_btn = btn_candidates
            .into_iter()
            .filter(|(hot_key, r, fb)| {
                fb.press > 0.02
                    && self.pressed == Some(*hot_key)
                    && self.press_gesture.dragged()
                    && r.w > 0.0
                    && r.h > 0.0
            })
            .max_by(|a, b| a.2.press.total_cmp(&b.2.press));
        let mut active_slider_knob: Option<(Box4, Feedback)> = None;
        if let Some(vp) = panels.iter().find(|p| p.kind == PanelKind::Volume) {
            let vf = fb_of(Hot::VolumeTrack);
            if vf.press > 0.04 {
                let frac = (self.volume.value() / 100.0).clamp(0.0, 1.0) as f32;
                let kr = chrome::slider_knob_rect(
                    vp.rect.left as f32 + 14.0,
                    vp.rect.top as f32 + 62.0,
                    vp.rect.width as f32 - 28.0,
                    frac,
                    vf,
                );
                active_slider_knob = Some((kr, vf));
            }
        }
        if active_slider_knob.is_none() {
            if let Some(sp) = &settings_paint {
                for (i, item, row) in &sp.rows {
                    if let Some((min, max, _)) = item.control.slider_bounds() {
                        let sf = fb_of(Hot::SettingSlider(*i as u8));
                        if sf.press > 0.04 {
                            let v = sp.values[item.key].as_f64().unwrap_or(min);
                            let frac = ((v - min) / (max - min)).clamp(0.0, 1.0) as f32;
                            let kr = chrome::slider_knob_rect(
                                row.x + 8.0,
                                row.y + 34.0,
                                row.w - 16.0,
                                frac,
                                sf,
                            );
                            active_slider_knob = Some((kr, sf));
                            break;
                        }
                    }
                }
            }
        }
        let active_mode_fb = fb_of(Hot::MenuMode(active_mode_u8));
        let active_page_fb = settings_paint
            .as_ref()
            .map(|sp| fb_of(Hot::SettingsPage(sp.page as u8)))
            .unwrap_or_default();
        let close_fb = fb_of(Hot::WindowClose);
        let close_hover_lens = self.settings_host.is_none()
            && active_btn.is_none()
            && self.window_controls_alpha > 0.02
            && (close_fb.hover > 0.01 || close_fb.press > 0.01);
        let mut button_lens: Option<(Hot, PlanPanel, Box4, Feedback)> = None;
        let lens_target = active_btn.or_else(|| close_hover_lens.then_some((Hot::WindowClose, cap_close, close_fb)));
        if let Some((hot_key, r, bf)) = lens_target {
            let is_close_btn = hot_key == Hot::WindowClose;
            let press = bf.press.clamp(0.0, 1.0);
            let scale_btn = if is_close_btn && press <= 0.02 {
                1.0 + 0.06 * bf.hover
            } else {
                (0.90 + 0.14 * press).max(0.85)
            };
            let lw = r.w * scale_btn * (1.0 + bf.squeeze[0] * 0.72);
            let lh = r.h * scale_btn * (1.0 + bf.squeeze[1] * 0.72);
            let cx_b = r.x + r.w * 0.5 + bf.pull[0] * 0.55;
            let cy_b = r.y + r.h * 0.5 + bf.pull[1] * 0.55;
            let lens_box = Box4::new(cx_b - lw * 0.5, cy_b - lh * 0.5, lw, lh);
            let backing_dim = ((r.w.max(r.h) * 1.4 + 20.0).ceil()).max(56.0);
            let lens_size = (backing_dim as f64, backing_dim as f64);
            if self.ensure_glass(window, PanelKind::ButtonLens, lens_size, scale) {
                let left = (cx_b - backing_dim * 0.5)
                    .round()
                    .clamp(0.0, (m.win_w - backing_dim).max(0.0));
                let top = (cy_b - backing_dim * 0.5)
                    .round()
                    .clamp(0.0, (m.win_h - backing_dim).max(0.0));
                if let Some(panel) = self.button_lens_glass.as_mut() {
                    let local_x = (lens_box.x - left).max(0.0) as f64 * scale as f64;
                    let local_y = (lens_box.y - top).max(0.0) as f64 * scale as f64;
                    let local_w = lw as f64 * scale as f64;
                    let local_h = lh as f64 * scale as f64;
                    let radius = lw.min(lh) as f64 * 0.5 * scale as f64;
                    panel.set_shape(
                        GlassRect {
                            left: local_x,
                            top: local_y,
                            width: local_w,
                            height: local_h,
                        },
                        radius,
                    );
                    if is_close_btn {
                        let ht = (bf.hover * 0.85 + press * 0.15).clamp(0.0, 1.0);
                        panel.set_tint(
                            [0.90, 0.17, 0.22],
                            (0.42 + 0.18 * ht) as f64,
                            [1.0, 0.76, 0.78, 0.62],
                            [0.55, 0.08, 0.10, 0.28],
                        );
                        let swell_c = (bf.hover * 0.45 + press * 0.48).clamp(0.0, 0.98);
                        let lens_alpha = (self.window_controls_alpha * (0.90 * bf.hover).max(press * 1.15)).clamp(0.0, 0.96);
                        panel.set_lens_liquid(scale, lens_alpha, swell_c, false);
                    } else {
                        if self.display.theme_mode == model::ThemeMode::Light {
                            panel.set_tint(
                                [0.96, 0.975, 1.0],
                                0.09,
                                [1.0, 1.0, 1.0, 0.90],
                                [0.12, 0.17, 0.26, 0.50],
                            );
                        } else {
                            panel.set_tint(
                                [0.97, 0.985, 1.0],
                                0.04,
                                [1.0, 1.0, 1.0, 0.42],
                                [0.60, 0.68, 0.82, 0.16],
                            );
                        }
                        let swell_b = (press * 0.85).clamp(0.0, 1.0);
                        let lens_alpha = (press * 1.15).clamp(0.0, 0.96);
                        panel.set_lens_liquid(scale, lens_alpha, swell_b, false);
                    }
                    panel.set_light([(0.38 + bf.focus[0] * 0.25).clamp(0.0, 1.0), 0.16, 0.92]);
                    let p = PlanPanel {
                        content_alpha: 1.0,
                        surface_alpha: if is_close_btn {
                            (self.window_controls_alpha * (0.90 * bf.hover).max(press * 1.15)).clamp(0.0, 0.96)
                        } else {
                            (press * 1.15).clamp(0.0, 0.96)
                        },
                        kind: PanelKind::ButtonLens,
                        rect: GlassRect {
                            left: left as f64,
                            top: top as f64,
                            width: lens_size.0,
                            height: lens_size.1,
                        },
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    };
                    if hot_key == Hot::SettingsClose {
                        if let Some(sp) = settings_paint.as_mut() {
                            sp.close_lens = Some((p.rect, p.surface, p.snapshot, p.hook));
                        }
                    } else {
                        button_lens = Some((hot_key, p, lens_box, bf));
                    }
                }
            }
        }
        let mut slider_lens = None;
        if let Some((kr, sf)) = active_slider_knob {
            let lens_size = (40.0, 40.0);
            if self.ensure_glass(window, PanelKind::SliderLens, lens_size, scale) {
                let (cx, cy) = kr.centre();
                let left = (cx - 20.0).round();
                let top = (cy - 20.0).round();
                if let Some(panel) = self.slider_lens_glass.as_mut() {
                    let local_x = (kr.x - left).max(0.0) as f64 * scale as f64;
                    let local_y = (kr.y - top).max(0.0) as f64 * scale as f64;
                    let local_w = kr.w as f64 * scale as f64;
                    let local_h = kr.h as f64 * scale as f64;
                    let radius = kr.w.min(kr.h) as f64 * 0.5 * scale as f64;
                    panel.set_shape(
                        GlassRect {
                            left: local_x,
                            top: local_y,
                            width: local_w,
                            height: local_h,
                        },
                        radius,
                    );
                    let lens_alpha = (sf.press * 1.15).clamp(0.0, 0.92);
                    panel.set_liquid(scale * 1.25, lens_alpha, 1.0);
                    panel.set_light([
                        (0.35 + sf.focus[0] * 0.25).clamp(0.0, 1.0),
                        0.18,
                        0.88,
                    ]);
                    slider_lens = Some(PlanPanel {
                        content_alpha: 1.0,
                        surface_alpha: lens_alpha,
                        kind: PanelKind::SliderLens,
                        rect: GlassRect {
                            left: left as f64,
                            top: top as f64,
                            width: lens_size.0,
                            height: lens_size.1,
                        },
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
            }
        }

        let mut mode_lens_glass = None;
        if self.panel_motion[0].visible() {
            mode_lens_glass = self.build_segmented_lens_glass(
                window,
                PanelKind::SegmentedLens,
                mode_lens_rect,
                mode_swell,
                mode_content_alpha,
                active_mode_fb,
                scale,
                false,
            );
        } else if let Some(sp) = settings_paint.as_mut() {
            let settings_alpha = self.panel_motion[3].content_alpha();
            if sp.tab_swell > 0.005 {
                let vertical = sp.kind == settings::SettingsKind::Application;
                if let Some(panel) = self.build_segmented_lens_glass(
                    window,
                    PanelKind::TabLens,
                    sp.tab_lens_rect,
                    sp.tab_swell,
                    settings_alpha,
                    active_page_fb,
                    scale,
                    vertical,
                ) {
                    sp.tab_lens_glass = Some((panel.rect, panel.surface, panel.snapshot, panel.hook));
                }
            }
            if let Some((&seg_key, seg)) = sp
                .segmented
                .iter()
                .filter(|(_, s)| s.swell > 0.005)
                .max_by(|a, b| a.1.swell.total_cmp(&b.1.swell))
            {
                let lens_rect = seg.lens_rect;
                let swell = seg.swell;
                let seg_fb = sp
                    .rows
                    .iter()
                    .find(|(_, item, _)| item.key == seg_key)
                    .map(|(idx, _, _)| {
                        let key = Hot::SettingChoice(*idx as u8);
                        self.feedback
                            .iter()
                            .find(|(h, _)| *h == key)
                            .map(|(_, f)| *f)
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                if let Some(panel) = self.build_segmented_lens_glass(
                    window,
                    PanelKind::SegmentedLens,
                    lens_rect,
                    swell,
                    settings_alpha,
                    seg_fb,
                    scale,
                    false,
                ) {
                    if let Some(seg_mut) = sp.segmented.get_mut(seg_key) {
                        seg_mut.glass = Some((panel.rect, panel.surface, panel.snapshot, panel.hook));
                    }
                }
            }
        }

        // The reference has no status chip in the window chrome, so the live
        // window stays visually identical at rest. A real status (searching,
        // control refused, init failure) still surfaces in the same corner.
        let badge = match self.source {
            Source::Replay => Some(format!(
                "REPLAY · {:.1}x",
                if self.playing { 1.0 } else { 0.0 }
            )),
            Source::Live => None,
        };
        let status = if self.volume.status().is_editable() {
            format!("{}%", self.volume.value().round() as i32)
        } else {
            "—".into()
        };
        let volume_label = self.volume.status().label().to_string();
        let volume_available = self.source != Source::Live
            || self
                .live
                .as_ref()
                .and_then(|l| l.model.packet.as_ref())
                .is_some_and(|p| !p.idle && !p.app.trim().is_empty());
        let dt_s = (dt.clamp(0.0, 100.0)) / 1000.0;
        let motion_frame_ready = self.motion_surface.is_some()
            && self
                .scene
                .as_ref()
                .is_some_and(PlayerScene::has_motion_frame);
        if self.display.motion_cover {
            if motion_frame_ready {
                self.motion_fade = (self.motion_fade + (dt_s as f32) / 0.45).min(1.0);
            }
        } else if self.motion_surface.is_some() && self.motion_fade > 0.0 {
            self.motion_fade = (self.motion_fade - (dt_s as f32) / 0.36).max(0.0);
            if self.motion_fade <= 0.001 {
                self.motion_fade = 0.0;
                self.clear_motion_surface(window);
            }
        } else {
            self.motion_fade = 0.0;
            if self.motion_surface.is_some() {
                self.clear_motion_surface(window);
            }
        }
        let motion_alpha = {
            let s = self.motion_fade.clamp(0.0, 1.0);
            s * s * (3.0 - 2.0 * s)
        };
        let empty_notice = if render_mode == Mode::Window
            && self.rows.is_empty()
            && !self.lyrics_hidden()
            && lyrics_alpha > 0.01
        {
            if self.source == Source::Live {
                self.live.as_ref().map(|live| {
                    if live.track.is_none() {
                        "未在播放".to_string()
                    } else if !live.status.is_empty() {
                        live.status.clone()
                    } else {
                        "未获取到歌词".to_string()
                    }
                })
            } else {
                None
            }
        } else {
            None
        };
        let candidates = if self.panel_motion[2].visible() && self.picker_kind != PickerKind::Cover {
            self.candidates()
        } else {
            Vec::new()
        };
        let mut smart_switch_pending = false;
        let mut smart_switch_fractions = Vec::new();
        let mut cover_switch_fractions = [
            if self.settings.value("showCover").as_bool().unwrap_or(true) { 1.0 } else { 0.0 },
            if self.settings.value("motionCover").as_bool().unwrap_or(true) { 1.0 } else { 0.0 },
        ];
        if self.panel_motion[2].visible() {
            if self.picker_kind == PickerKind::Cover {
                for (idx, (spring_key, setting_key)) in [
                    ("cover:showCover", "showCover"),
                    ("cover:motionCover", "motionCover"),
                ]
                .into_iter()
                .enumerate()
                {
                    let on = self.settings.value(setting_key).as_bool().unwrap_or(true);
                    let target = if on { 1.0 } else { 0.0 };
                    let spring = self
                        .smart_switch_springs
                        .entry(spring_key.to_string())
                        .or_insert_with(|| super::interaction::state_icon_spring(target));
                    let frac = if let Some(drag) = self
                        .inline_switch_drag
                        .as_ref()
                        .filter(|d| d.key == spring_key && d.moved)
                    {
                        let v = drag.fraction.clamp(0.0, 1.0);
                        spring.set_position(v as f64);
                        v
                    } else {
                        spring.set_target_position(target);
                        spring.update(dt_s);
                        smart_switch_pending |= !spring.arrived();
                        spring.position().clamp(0.0, 1.0) as f32
                    };
                    cover_switch_fractions[idx] = frac;
                }
            }
            for entry in &candidates {
                if !entry.has_translation && !entry.smart_translation_key.is_empty() {
                    let target = if entry.smart_translation { 1.0 } else { 0.0 };
                    let spring = self
                        .smart_switch_springs
                        .entry(entry.smart_translation_key.clone())
                        .or_insert_with(|| super::interaction::state_icon_spring(target));
                    let frac = if let Some(drag) = self
                        .inline_switch_drag
                        .as_ref()
                        .filter(|d| d.key == entry.smart_translation_key && d.moved)
                    {
                        let v = drag.fraction.clamp(0.0, 1.0);
                        spring.set_position(v as f64);
                        v
                    } else {
                        spring.set_target_position(target);
                        spring.update(dt_s);
                        smart_switch_pending |= !spring.arrived();
                        spring.position().clamp(0.0, 1.0) as f32
                    };
                    smart_switch_fractions.push((entry.smart_translation_key.clone(), frac));
                }
            }
        }
        let mut display_for_plan = self.display;
        if display_for_plan.accent_auto {
            display_for_plan.accent_hex = self.cover_accent_hex.unwrap_or(display_for_plan.accent_hex);
        }
        if let Some(sp) = settings_paint.as_mut() {
            sp.accent_hex = display_for_plan.accent_hex;
        }
        let mut island_glass_panel = None;
        if let Some((ir, eased)) = island_plan {
            let size = (ir.w as f64, ir.h as f64);
            if self.ensure_glass(window, PanelKind::Island, size, scale) {
                if let Some(panel) = self.island_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::Island, immersion, self.display.theme_mode);
                    panel.set_radius(18.0 * scale as f64);
                    panel.set_liquid(scale, eased, (1.0 - eased).abs().min(1.0));
                    panel.set_light([0.25, 0.08, 0.45]);
                    island_glass_panel = Some(PlanPanel {
                        content_alpha: eased,
                        surface_alpha: eased,
                        kind: PanelKind::Island,
                        rect: GlassRect {
                            left: ir.x as f64,
                            top: ir.y as f64,
                            width: ir.w as f64,
                            height: ir.h as f64,
                        },
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
            }
        }
        if !self.panel_motion[0].visible() {
            if let Some(mut old) = self.more_glass.take() { old.destroy(window); }
        }
        if !self.panel_motion[1].visible() {
            if let Some(mut old) = self.volume_glass.take() { old.destroy(window); }
        }
        if !self.panel_motion[2].visible() {
            if let Some(mut old) = self.picker_glass.take() { old.destroy(window); }
        }
        if !self.panel_motion[3].visible() {
            if let Some(mut old) = self.settings_glass.take() { old.destroy(window); }
            if let Some(mut old) = self.dropdown_glass.take() { old.destroy(window); }
        } else if self.settings.open_choice.is_none() && self.settings.modal.is_none() {
            if let Some(mut old) = self.dropdown_glass.take() { old.destroy(window); }
        }
        if island_glass_panel.is_none() {
            if let Some(mut old) = self.island_glass.take() { old.destroy(window); }
        }
        if scroll_glass.is_none() {
            if let Some(mut old) = self.thumb_glass.take() { old.destroy(window); }
        }
        if slider_lens.is_none() {
            if let Some(mut old) = self.slider_lens_glass.take() { old.destroy(window); }
        }
        let close_lens_used = settings_paint.as_ref().is_some_and(|sp| sp.close_lens.is_some());
        if button_lens.is_none() && !close_lens_used {
            if let Some(mut old) = self.button_lens_glass.take() { old.destroy(window); }
        }
        let seg_lens_used = mode_lens_glass.is_some()
            || settings_paint
                .as_ref()
                .is_some_and(|sp| sp.segmented.values().any(|s| s.glass.is_some()));
        if !seg_lens_used {
            if let Some(mut old) = self.segmented_lens_glass.take() { old.destroy(window); }
        }
        let tab_lens_used = settings_paint.as_ref().is_some_and(|sp| sp.tab_lens_glass.is_some());
        if !tab_lens_used {
            if let Some(mut old) = self.tab_lens_glass.take() { old.destroy(window); }
        }
        self.last_fullscreen = window.is_fullscreen();
        let plan = Plan {settings_only:self.settings_host.is_some(),
            immersion,
            metrics: m.clone(),
            chrome: chrome.clone(),
            bg: if !immersion || !self.scene.as_ref().is_some_and(PlayerScene::has_background) { None } else { self.bg_surface },
            cover: self.cover_surface,
            motion: if motion_alpha > 0.001 { self.motion_surface } else { None },
            motion_alpha,
            rows: plan_rows,
            lyric_hover,
            interlude: plan_interlude,
            bottom: plan_bottom,
            panels,
            position_ms: self.seek_preview_ms.unwrap_or_else(|| self.live.as_ref().map(|l|l.model.media_position(live_now)).unwrap_or(self.position_ms)),
            duration_ms: self.duration_ms,
            play_morph: self.play_morph.position() as f32,
            translation: self.display.show_translation,
            badge,
            status,
            track_title: self.live.as_ref().and_then(|l| l.model.packet.as_ref()).map(|p| p.title.clone()).or_else(|| self
                .doc
                .as_ref()
                .map(|d| d.title.clone()))
                .unwrap_or_else(|| if self.source == Source::Live { "未在播放".into() } else { "暂无歌词".into() }),
            // The cover caption uses `artistLabel` from `src/shared/track-labels.ts`:
            // providers repeat the album inside the artist field, and the
            // reference strips it for display only.
            track_artist: self
                .live
                .as_ref()
                .and_then(|l| l.model.packet.as_ref())
                .map(|p| chrome::artist_label(&p.artist, &p.album))
                .or_else(|| {
                    self.doc.as_ref().map(|d| {
                        let raw = if d.artist.is_empty() {
                            d.artists.join(" / ")
                        } else {
                            d.artist.clone()
                        };
                        chrome::artist_label(&raw, &d.album)
                    })
                })
                .unwrap_or_default(),
            cursor: self.cursor,
            feedback: self.feedback.clone(),
            volume_enabled: self.volume.status().is_editable(),
            lyric_position_ms:self.lyric_time(),sync_enabled:self.sync.enabled(),sync_label:offset_label(self.sync.offset_ms()),
            volume_label,
            volume_available,
            chrome_visible,
            content_alpha,
            chrome_alpha,
            compact_chrome_alpha: self.compact_chrome_alpha,
            album_meta_alpha: self.album_meta_alpha,
            cover_scale,
            cover_vis,
            picker_start:self.picker_start,
            picker_kind: self.picker_kind,
            show_media_row: show_media,
            picker_refresh_angle: self.picker_refresh_angle,
            mode: render_mode,
            fullscreen: render_fullscreen,
            control_pending: self.live.as_ref().is_some_and(|l| l.model.control_pending()),
            lyrics_visible: !self.lyrics_hidden(),
            lyrics_alpha,
            smart_switch_fractions,
            cover_switch_fractions,
            empty_notice,
            island: island_plan,
            island_glass_panel,
            display: display_for_plan,
            settings_paint,
            scroll_glass,
            scroll_line,
            slider_lens,
            button_lens,
            window_controls_alpha: self.window_controls_alpha,
            mode_well_rect,
            mode_lens_rect,
            mode_swell,
            mode_lens_glass,
            active_mode: active_mode_u8,
            drag_thumb: self.drag_thumb.map(|(x, y)| (x, y as f32)),
        };

        let picked = self.picked;
        let volume_value = self.volume.value();
        let more_items = input::MORE_LABELS;

        // The frame must be recomputed while anything moves.
        // A paused player still needs frames while a scripted pass or a bounded
        // run is pending: the pass advances on wall time, so stopping the frame
        // loop would freeze the scenario (and the exit timer) mid-clip.
        let animating = self.settings.animating()
            || self.settings.status_alpha > 0.001
            || !self.lyrics_center.arrived()
            || (self.lyrics_reveal_alpha > 0.001 && self.lyrics_reveal_alpha < 0.999)
            || !self.cover_scale_spring.arrived()
            || !self.cover_vis_spring.arrived()
            || self.mode_lens.pending()
            || self.staged_mode.is_some()
            || self.pending_mode.is_some()
            || self.mode_transition.is_some()
            || self.volume.has_pending_write()
            || self.cover_fade < 1.0
            || (self.motion_fade > 0.001 && self.motion_fade < 0.999)
            || (self.island_drop > 0.001 && self.island_drop < 0.999)
            || (nocover_island && (now - self.island_wake_ms) < 3800.0)
            || self.cover_gone_at_ms.is_some_and(|t| now - t < 240.0)
            || (self.compact_chrome_alpha > 0.001 && self.compact_chrome_alpha < 0.999)
            || (self.album_meta_alpha > 0.001 && self.album_meta_alpha < 0.999)
            || (!self.display.motion_cover && self.motion_fade > 0.001)
            || self.scene.as_ref().is_some_and(|s| s.has_motion() && !s.has_motion_frame())
            || smart_switch_pending
            || self.pending_settings_window.is_some()
            || !self.play_morph.arrived()
            || feedback_pending
            || translation_pending
            || self.panel_motion.iter().any(PanelMotion::pending)
            || panel_elastic_pending
            || self.picker_refresh_spinning
            || self.picker_refresh_angle > 0.0
            || (self.browse_state.is_none() && self.follow.pending())
            || self.group_motion.iter().any(model::RowMotion::pending)
            || self.group_bg_slide.iter().any(model::RowMotion::pending)
            || self.main_scale.iter().any(model::RowMotion::pending)
            || self.bg_scale.iter().any(model::RowMotion::pending)
            || self.bottom_motion.as_ref().is_some_and(model::RowMotion::pending)
            || self.group_bg_alpha.iter().any(|&a| a > 0.001 && a < 0.999)
            || self.group_passed_alpha.iter().any(|&a| a > 0.001 && a < 0.999)
            || (self.bottom_alpha - self.bottom_target_alpha).abs() > 0.001
            || (!self.rows.is_empty() && self.interlude_dots.pending())
            || self.scene.as_ref().is_some_and(PlayerScene::has_background_work)
            || (immersion && (self.cover.is_some() || self.scene.as_ref().is_some_and(|s|s.has_background())))
            || self.playing
            || self.scenario.is_some()
            || self.cfg.seconds.is_some()
            || self.scroll.engine().is_interacting()
            || self.scroll.engine().has_inertia()
            || self.thumb_elastic.pending()
            || self.drag_thumb.is_some()
            || (cap_hovered && self.window_controls_alpha < 0.999)
            || (!cap_hovered && self.window_controls_alpha > 0.001);
        let sidecar_open = self
            .settings_child_view
            .as_ref()
            .is_some_and(|w| w.upgrade().is_some());
        if !sidecar_open && self.settings_child.is_some() {
            self.settings_child = None;
            self.settings_child_view = None;
        }
        let cursor_can_hide = self.settings_host.is_none()
            && !sidecar_open
            && matches!(render_mode, Mode::Window | Mode::Lyrics)
            && !self.tool.panels().any_open()
            && !self.settings.open
            && !self.picker_open
            && self.pressed.is_none()
            && self.drag_panel.is_none()
            && self.drag_thumb.is_none()
            && self.switch_drag.is_none()
            && self.settings.editing_key.is_none()
            && (self.cursor.is_some() || (window.is_fullscreen() && window.is_window_active()));
        if cursor_can_hide && now - self.cursor_idle_ms >= 3000.0 && cx.is_cursor_visible() {
            window.hide_cursor_until_mouse_moves(cx);
        }
        if self.settings_host.is_none()
            && self.frames == 1
            && self.cfg.capture_rgba.is_none()
            && self.cfg.scenario.is_none()
        {
            window.request_animation_frame();
        }
        if animating && window.is_visible() {
            window.request_animation_frame();
        } else {
            if self.source == Source::Live && !self.live_poll_pending {
                self.live_poll_pending = true;
                let timer = cx.background_executor().timer(std::time::Duration::from_millis(120));
                cx.spawn(async move |view, cx| {
                    timer.await;
                    let _ = view.update(cx, |view, cx| { view.live_poll_pending = false; cx.notify(); });
                }).detach();
            }
            if cursor_can_hide && now - self.cursor_idle_ms < 3000.0 && !self.cursor_hide_pending {
                self.cursor_hide_pending = true;
                let wait_ms = ((3050.0 - (now - self.cursor_idle_ms)).clamp(50.0, 3050.0)) as u64;
                let timer = cx.background_executor().timer(std::time::Duration::from_millis(wait_ms));
                cx.spawn(async move |view, cx| {
                    timer.await;
                    let _ = view.update(cx, |view, cx| { view.cursor_hide_pending = false; cx.notify(); });
                }).detach();
            }
        }

        let fold_obstacle = compact_fold_allowed
            && !self.tool.panels().any_open()
            && !self.settings.open
            && !self.picker_open;
        let mut drag_metrics = m.clone();
        if render_mode == Mode::Window && self.lyrics_hidden() {
            drag_metrics.lyric_viewport = Box4::default();
        }
        let drag_regions = if self.settings_host.is_some() {
            Vec::new()
        } else {
            input::drag_regions(
                &drag_metrics,
                self.hot
                    .iter()
                    .map(|(_, r)| *r)
                    .chain(self.panel_bounds.iter().copied())
                    .chain(fold_obstacle.then_some(fold_box))
                    .chain(island_plan.map(|(ir, _)| ir))
                    .chain(cap_allowed.then_some([cap_min, cap_fs, cap_close]).into_iter().flatten()),
            )
        };
        let drag_view = cx.entity().downgrade();
        let input_entity = cx.entity();
        let input_focus = self.focus.clone();
        let input_active = self.settings.open
            && (self.settings.modal.is_some() || self.settings.editing_key.is_some());
        let view_w = plan.metrics.win_w;
        let view_h = plan.metrics.win_h;
        let canvas = canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _state, window, cx| {
                if input_active {
                    window.handle_input(
                        &input_focus,
                        gpui_kit::ElementInputHandler::new(bounds, input_entity.clone()),
                        cx,
                    );
                }
                paint_frame(
                    window,
                    cx,
                    &plan,
                    &more_items,
                    &candidates,
                    picked,
                    volume_value,
                );
                let view = drag_view.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble || !ev.dragging() {
                        return;
                    }
                    let x = f32::from(ev.position.x);
                    let y = f32::from(ev.position.y);
                    if x >= 0.0 && y >= 0.0 && x < view_w && y < view_h {
                        return;
                    }
                    let _ = view.update(cx, |this, cx| {
                        if !this.has_active_press() {
                            return;
                        }
                        this.cursor = Some((x, y));
                        let now = this.now_ms();
                        this.record_cursor_activity(now);
                        this.on_drag(x, y);
                        cx.notify();
                    });
                });
            },
        )
        .size_full();

        let mut root = div()
            .id("player-root")
            .track_focus(&self.focus)
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if !*hovered && !this.has_active_press() { this.cursor = None; cx.notify(); }
            }))
            .size_full()
            .relative()
            .on_scroll(cx.listener(|this, ev: &ScrollEvent, _window, cx| {
                let now = this.now_ms();
                this.record_cursor_activity(now);
                let p=ev.position;
                if this.picker_open && this.panel_bounds.iter().any(|r|r.contains(p.x.into(),p.y.into())) {
                    let delta=match ev.delta{ScrollDelta::Pixels(d)=>f32::from(d.y),ScrollDelta::Lines(d)=>d.y};
                    let max=this.candidates().len().saturating_sub(this.picker_capacity);
                    if delta<0.{this.picker_start=(this.picker_start+1).min(max);}else if delta>0.{this.picker_start=this.picker_start.saturating_sub(1);}
                    cx.notify();return;
                }
                if this.settings.open {
                    if this.settings.modal.is_some() {
                        return;
                    }
                    let delta=match ev.delta {ScrollDelta::Pixels(d)=>f32::from(d.y),ScrollDelta::Lines(d)=>d.y*40.0};
                    if let Some(panel_r) = this.panel_bounds.last().copied() {
                        if let Some(d) = this.settings.dropdown_layout(panel_r) {
                            if d.rect.contains(p.x.into(), p.y.into()) {
                                this.settings.choice_scroll = (this.settings.choice_scroll - delta).clamp(0.0, d.max_scroll);
                                cx.notify();
                                return;
                            }
                        }
                    }
                    if this.panel_bounds.last().is_some_and(|r|r.contains(p.x.into(),p.y.into())) {
                        this.settings.close_choice();
                        this.settings.scroll=(this.settings.scroll-delta).max(0.0);cx.notify();return;
                    }
                }
                let x=f32::from(p.x);let y=f32::from(p.y);
                let viewport=this.metrics(_window,cx).lyric_viewport;
                if this.tool.mode()==Some(Mode::Mini)
                    || (this.tool.mode()==Some(Mode::Window) && this.lyrics_hidden())
                    || !viewport.contains(x,y)
                    || this.panel_bounds.iter().any(|r|r.contains(x,y))
                {return;}
                if this.scroll.on_scroll(ev, now) {
                    this.browse_idle_ms = now;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    let now = this.now_ms();
                    this.record_cursor_activity(now);
                    window.focus(&this.focus, cx);
                    this.on_down(ev.position.x.into(), ev.position.y.into(), window.is_window_active());
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    let now = this.now_ms();
                    this.record_cursor_activity(now);
                    window.focus(&this.focus, cx);
                    this.on_right_down(ev.position.x.into(), ev.position.y.into());
                    cx.notify();
                }),
            )
            .on_drop::<gpui_kit::ExternalPaths>(cx.listener(|this, paths: &gpui_kit::ExternalPaths, _window, cx| {
                if let Some(path) = paths
                    .0
                    .iter()
                    .find(|p| oly_engine::custom_lyrics::is_supported_lyric_file(p))
                {
                    this.import_lyric_file(path, true);
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                let next_pos = (f32::from(ev.position.x), f32::from(ev.position.y));
                let moved = this.cursor.is_none_or(|(px, py)| {
                    (px - next_pos.0).abs() > 0.5 || (py - next_pos.1).abs() > 0.5
                });
                this.cursor = Some(next_pos);
                let now = this.now_ms();
                if moved {
                    this.record_cursor_activity(now);
                    if this.browse_state.is_some() {
                        this.browse_idle_ms = now;
                    }
                }
                if moved
                    && this.mode_transition.is_none()
                    && next_pos.1 > f32::from(_window.viewport_size().height) - 190.
                {
                    this.chrome_wake_ms = now;
                }
                if ev.dragging() {
                    this.on_drag(next_pos.0, next_pos.1);
                }
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _window, cx| {
                    let now = this.now_ms();
                    this.record_cursor_activity(now);
                    this.on_up(ev.position.x.into(),ev.position.y.into());
                    cx.notify();
                }),
            )
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, _window, cx| {
                let now = this.now_ms();
                this.record_cursor_activity(now);
                this.on_up(ev.position.x.into(), ev.position.y.into());
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                let now = this.now_ms();
                this.record_cursor_activity(now);
                if this.settings.modal.is_some() {
                    if ev.keystroke.modifiers.control && ev.keystroke.key == "v" {
                        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                            this.settings.insert_modal_text(&text);
                        }
                    } else if ev.keystroke.modifiers.control && ev.keystroke.key == "a" {
                        this.settings.clear_modal_field();
                    } else if matches!(
                        ev.keystroke.key.as_str(),
                        "enter" | "return" | "escape" | "backspace" | "delete" | "tab"
                    ) {
                        this.settings.handle_modal_key(&ev.keystroke.key, now);
                    } else if let Some(text) = ev.keystroke.key_char.as_deref() {
                        this.settings.insert_modal_text(text);
                    } else {
                        this.settings.handle_modal_key(&ev.keystroke.key, now);
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if this.settings.editing_key.is_some() {
                    if ev.keystroke.modifiers.control && ev.keystroke.key == "v" {
                        if let Some(text)=cx.read_from_clipboard().and_then(|c|c.text()){this.settings.insert_text(&text);}
                    } else if ev.keystroke.modifiers.control && ev.keystroke.key == "a" {this.settings.editing_draft.clear();}
                    else if let Some(text)=ev.keystroke.key_char.as_deref(){this.settings.insert_text(text);}
                    else {this.settings.edit_key(&ev.keystroke.key,this.now_ms());}
                    cx.stop_propagation();
                } else {this.on_key(&ev.keystroke.key);}
                cx.notify();
            }))
            .child(canvas)
            .children(drag_regions.into_iter().map(|r|div().absolute().left(px(r.x)).top(px(r.y)).w(px(r.w)).h(px(r.h)).window_control_area(WindowControlArea::Drag)))
            .children(fold_obstacle.then(|| {
                div()
                    .id("fold-hit")
                    .absolute()
                    .left(px(fold_box.x))
                    .top(px(fold_box.y))
                    .w(px(fold_box.w))
                    .h(px(fold_box.h))
                    .cursor_pointer()
            }));

        // Leaving the window drops hover feedback.
        root = root.on_mouse_down_out(cx.listener(|this, _ev, _window, cx| {
            this.cursor = None;
            this.cancel_press();
            cx.notify();
        }));
        root.into_any_element()
    }
}

impl gpui_kit::EntityInputHandler for PlayerView {
    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.settings.open
            && (self.settings.modal.is_some() || self.settings.editing_key.is_some())
    }

    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui_kit::UTF16Selection> {
        Some(gpui_kit::UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.modal.is_some() {
            if self.settings.insert_modal_text(text) {
                cx.notify();
            }
        } else if self.settings.editing_key.is_some() {
            self.settings.insert_text(text);
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: gpui_kit::Bounds<gpui_kit::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui_kit::Bounds<gpui_kit::Pixels>> {
        if let Some(modal) = &self.settings.modal {
            let panel_r = self
                .panel_bounds
                .last()
                .copied()
                .unwrap_or(Box4::new(0.0, 0.0, 540.0, 440.0));
            let mp = self.panel_elastics[4].pull();
            let (box_r, f1, f2, _, _) = Settings::modal_rects_with_offset(panel_r, modal, mp);
            let r = match modal {
                settings::SettingsModal::CustomModel { .. } => f1,
                settings::SettingsModal::ArtistAlias { focus, .. } => {
                    if *focus == 1 {
                        f2.unwrap_or(f1)
                    } else {
                        f1
                    }
                }
                settings::SettingsModal::PrivacyPolicy => box_r,
            };
            return Some(gpui_kit::Bounds {
                origin: point(px(r.x + 12.0), px(r.y + 4.0)),
                size: gpui_kit::size(px(2.0), px(r.h - 8.0)),
            });
        }
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui_kit::Point<gpui_kit::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl PlayerView {
    /// The picker's version list: every fixture beside the loaded one, so
    /// "choose a version" really swaps documents instead of faking a choice.
    fn execute_settings_action(&mut self, action: &str) -> (String, Option<serde_json::Value>) {
        if action == "action.openRepo" {
            let url = "https://github.com/steedsha/OverLyrics";
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", url])
                    .creation_flags(0x0800_0000)
                    .spawn();
            }
            return ("已在浏览器中打开开源仓库".into(), None);
        }
        let Some(live) = self.live.as_ref() else {
            return (String::new(), None);
        };
        match action {
            "action.clearLyricsCache" => {
                let n = live.clear_lyrics_cache();
                (format!("已清理 {n} 项歌词缓存"), None)
            }
            "action.clearCoverCache" => {
                let n = live.clear_cover_cache();
                (format!("已清理 {n} 项封面缓存"), None)
            }
            "action.resetTrackOffsets" => {
                if live.reset_track_offsets().is_ok() {
                    self.update_sync();
                    ("已重置全部单曲偏移".into(), None)
                } else {
                    (String::new(), None)
                }
            }
            "action.resetDefaults" => {
                if let Ok(fresh) = live.reset_all_settings() {
                    self.settings.config = fresh.clone();
                    self.sync_profile_display();
                    self.update_sync();
                    ("已恢复默认设置".into(), Some(fresh))
                } else {
                    (String::new(), None)
                }
            }
            _ => (String::new(), None),
        }
    }

    fn notify_artist_alias_changed(&mut self, changed_artist: &str) {
        if let Some(live) = self.live.as_mut() {
            live.search.clear_cache();
            if let Some(track) = live.track.clone() {
                let cur_artist = oly_engine::strings::primary_artist(&track);
                if cur_artist.trim().eq_ignore_ascii_case(changed_artist.trim()) {
                    live.search.request(live.model.generation(), track, None);
                    live.status = "正在重新搜索歌词…".into();
                }
            }
        }
    }

    fn has_multiple_media_sessions(&self) -> bool {
        self.live.as_ref().is_some_and(|l| l.media_sessions.len() > 1)
    }

    fn friendly_media_name(app: &str) -> String {
        let low = app.to_ascii_lowercase();
        if low.contains("applemusic") {
            "Apple Music".into()
        } else if low.contains("spotify") {
            "Spotify".into()
        } else if low.contains("qqmusic") {
            "QQ 音乐".into()
        } else if low.contains("cloudmusic") {
            "网易云音乐".into()
        } else if low.contains("kugou") {
            "酷狗音乐".into()
        } else if low.contains("msedge") {
            "Microsoft Edge".into()
        } else if low.contains("chrome") {
            "Google Chrome".into()
        } else if low.contains("foobar2000") {
            "foobar2000".into()
        } else if low.contains("potplayer") {
            "PotPlayer".into()
        } else {
            let trimmed = app.split('!').next().unwrap_or(app);
            trimmed
                .strip_suffix(".exe")
                .or_else(|| trimmed.strip_suffix(".EXE"))
                .unwrap_or(trimmed)
                .to_string()
        }
    }

    fn candidates(&self) -> Vec<PickerEntry> {
        if self.picker_kind == PickerKind::Media {
            let mut out = Vec::new();
            if let Some(live) = self.live.as_ref() {
                let pinned = live.pinned_media_app.as_deref();
                out.push(PickerEntry {
                    title: "自动跟随".into(),
                    subtitle: "自动选择正在播放的媒体".into(),
                    match_badge: String::new(),
                    timing_badge: None,
                    has_translation: false,
                    smart_translation: false,
                    smart_translation_key: String::new(),
                    path: PathBuf::new(),
                    source: "media-auto".into(),
                    id: String::new(),
                    candidate_index: 0,
                    selected: pinned.is_none(),
                });
                for s in &live.media_sessions {
                    let status_label = if s.status == 4 { "播放中" } else { "已暂停" };
                    let sub = if s.title.trim().is_empty() {
                        s.app.clone()
                    } else {
                        s.title.clone()
                    };
                    out.push(PickerEntry {
                        title: Self::friendly_media_name(&s.app),
                        subtitle: sub,
                        match_badge: String::new(),
                        timing_badge: Some(status_label),
                        has_translation: false,
                        smart_translation: false,
                        smart_translation_key: String::new(),
                        path: PathBuf::new(),
                        source: "media-pin".into(),
                        id: s.app.clone(),
                        candidate_index: 0,
                        selected: pinned == Some(s.app.as_str()),
                    });
                }
            }
            return out;
        }
        let qa_pool=if self.cfg.scenario.as_deref()==Some("settings-split"){
            use oly_engine::types::{Candidate,Verified};
            let c=|source:&str,id:&str,percent:Option<f64>,word:bool,trans:bool|Candidate{source:source.into(),id:id.into(),title:"Hard Times".into(),artist:Some("Paramore".into()),percent,duration_sec:Some(182.),verified:Some(Verified{word:Some(word),trans:Some(trans),timeline:Some(true),lines:Some(58),..Default::default()}),..Default::default()};
            Some(vec![c("amlldb","qa-amll",Some(98.4),true,true),c("kugou","qa-line",Some(70.),false,false),c("qq","qa-below",Some(69.9),true,true),c("lrclib","qa-unknown",None,false,false)])
        }else{None};
        if let Some(pool)=self.live.as_ref().map(|live|live.candidates.as_slice()).or(qa_pool.as_deref()) {
            let identity=if qa_pool.is_some(){Some(("amlldb","qa-amll"))}else{self.doc_identity.as_ref().map(|(s,i)|(s.as_str(),i.as_str()))};
            let track_key=self.live.as_ref().and_then(|live|live.model.packet.as_ref()).map(|p|p.track_key()).unwrap_or_else(||if qa_pool.is_some(){"qa-track".into()}else{String::new()});
            let mut out = Vec::new();
            if let Some(live) = self.live.as_ref() {
                for entry in live.custom_lyrics.entries_for(&track_key) {
                    let selected = identity.is_some_and(|(s, id)| s == "custom" && id == entry.id);
                    out.push(PickerEntry {
                        title: format!("{} · 本地导入", entry.file_name),
                        subtitle: format!("{} · {} 行（右键删除）", entry.ext.to_uppercase(), entry.line_count),
                        match_badge: String::new(),
                        timing_badge: entry.word_timed.then_some("逐字"),
                        has_translation: entry.has_translation,
                        smart_translation: false,
                        smart_translation_key: String::new(),
                        path: PathBuf::new(),
                        source: "custom".into(),
                        id: entry.id.clone(),
                        candidate_index: 0,
                        selected,
                    });
                }
            }
            out.extend(super::versions::version_rows(pool,&self.settings.config,identity).into_iter().map(|r|{
                let smart_translation_key=if track_key.is_empty(){String::new()}else{oly_contracts::config::candidate_translation_key(&track_key,&r.source,&r.id)};
                let smart_translation=!smart_translation_key.is_empty() && super::translation::translation_enabled(&self.settings.config,&smart_translation_key);
                let match_badge = r.detail.strip_prefix("匹配 ").unwrap_or(&r.detail).to_owned();
                PickerEntry{title:r.title,subtitle:r.subtitle,match_badge,timing_badge:r.timing_badge,has_translation:r.has_translation,smart_translation,smart_translation_key,path:PathBuf::new(),source:r.source,id:r.id,candidate_index:r.candidate_index,selected:r.selected}}));
            return out;
        }
        let Some(path)=self.cfg.fixture.as_ref() else{return Vec::new();};
        let Some(dir)=(if path.is_dir(){Some(path.as_path())}else{path.parent()}) else{return Vec::new();};
        let mut out=Vec::new();
        if let Ok(entries)=std::fs::read_dir(dir){for entry in entries.flatten(){
            let p=entry.path();if p.extension().and_then(|v|v.to_str())!=Some("json"){continue;}
            let name=p.file_stem().and_then(|v|v.to_str()).unwrap_or("?").to_string();
            out.push(PickerEntry{title:name,subtitle:"本地回放样例".into(),match_badge:String::new(),timing_badge:None,has_translation:true,smart_translation:false,smart_translation_key:String::new(),selected:p==*path,path:p,source:"fixture".into(),id:String::new(),candidate_index:0});
        }}
        out.sort_by(|a,b|a.title.cmp(&b.title));out
    }

    /// Whether the picker is showing. The more panel hosts the entry that opens
    /// it, so both live under the same anchor.
    fn more_open_sub(&self) -> bool {
        self.picker_open
    }

    fn player_base_offset_ms(&self) -> f64 {
        if self.source != Source::Live {
            return 0.0;
        }
        let app = self
            .live
            .as_ref()
            .and_then(|l| l.model.packet.as_ref())
            .filter(|p| !p.idle)
            .map(|p| p.app.as_str());
        let bucket = oly_core::mapping::player_offset_key(app);
        let offsets = &self.settings.config["lyrics"]["playerSyncOffsets"];
        if bucket == "apple" || bucket == "default" {
            offsets.get(&bucket).and_then(serde_json::Value::as_f64).unwrap_or(0.0)
        } else {
            offsets
                .get(&bucket)
                .and_then(serde_json::Value::as_f64)
                .or_else(|| offsets.get("default").and_then(serde_json::Value::as_f64))
                .unwrap_or(0.0)
        }
    }

    fn lyric_time(&self)->f64 {self.position_ms+self.player_base_offset_ms()+self.sync.offset_ms() as f64}
    fn update_sync(&mut self) {
        use oly_core::offsets::{song_of_packet,SongRef};
        let song=if let Some(live)=self.live.as_ref(){song_of_packet(live.model.packet.as_ref())}else{self.doc.as_ref().map(|d|SongRef{title:d.title.clone(),artist:if d.artist.is_empty(){d.artists.join(" / ")}else{d.artist.clone()},album:d.album.clone(),album_artist:String::new(),duration_ms:self.duration_ms,app:"fixture".into()})};
        let provider=self.doc_provider.as_str();
        self.sync.select(song,provider);
        if let Some(error)=self.sync.poll(){log::warn!("{error}");if let Some(live)=self.live.as_mut(){live.status=error;}}
    }
    fn nudge_sync(&mut self, delta: i64) {
        match self.sync.nudge(delta) {
            Ok(ms) => {
                log::info!("[lyric-sync] offset_ms={ms}");
                self.pending_seek = true;
            }
            Err(error) => {
                log::warn!("{error}");
                if let Some(live) = self.live.as_mut() {
                    live.status = error;
                }
            }
        }
    }
    fn live_action(&mut self, action: oly_platform_win::smtc::control::Action) {
        let live_now = Self::live_now_ms();
        let Some(live) = self.live.as_mut() else { return; };
        let Some(request) = live.model.prepare_control(action, oly_platform_win::smtc::now_ms()) else {
            live.status = "当前播放器不支持此操作".into(); return;
        };
        let request_id=request.request_id;
        live.model.preview_control(action,live_now);
        if !live.host.dispatch(crate::commands::HostCommand::GuardedTransport(request)) {
            live.model.control_result(request_id,false);
            live.status = "无法连接到播放器".into();
        }
        log::info!("[control-preview] action={action:?} playing={} lyric_ms={:.1} media_ms={:.1}",live.model.effective_playing(),live.model.position(live_now),live.model.media_position(live_now));
    }
    fn can_seek_live(&self) -> bool {
        if self.source != Source::Live {
            return self.duration_ms > 0.0;
        }
        self.live.as_ref().is_some_and(|l| l.model.can_seek())
    }

    fn live_seek(&mut self, target_ms: f64) {
        let Some(live) = self.live.as_mut() else { return; };
        let Some(request) = live.model.prepare_seek(target_ms, oly_platform_win::smtc::now_ms()) else {
            return;
        };
        if live.host.dispatch(crate::commands::HostCommand::Seek(request)) {
            self.position_ms = target_ms;
            self.pending_seek = true;
        }
    }

    fn import_lyric_file(&mut self, path: &std::path::Path, open_picker: bool) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match live.import_custom_lyric(path) {
            Ok(entry) => {
                let doc = entry.doc();
                self.doc_auto_picked = false;
                self.auto_pick_persisted = false;
                self.doc_identity = Some(("custom".into(), entry.id));
                self.doc_provider = "custom".into();
                self.lyrics_auto_hidden = doc.instrumental;
                self.install_document(doc, false);
                self.picker_start = 0;
                if open_picker {
                    self.settings.open = false;
                    self.settings.close_choice();
                    self.close_volume_panel();
                    self.tool.set_more_open(true);
                    self.picker_open = true;
                }
            }
            Err(err) => {
                log::warn!("custom lyric import failed: {err}");
            }
        }
    }

    fn on_right_down(&mut self, x: f32, y: f32) {
        let hit = pointer_hit(&self.hot, &self.panel_bounds, x, y);
        if self.settings.open {
            match hit {
                Some(Hot::DropdownOption(opt_i)) => {
                    self.settings.delete_custom_model_option(opt_i as usize, self.now_ms());
                    return;
                }
                Some(Hot::SettingChoice(row_i)) => {
                    if let Some(item) = self.settings.items().get(row_i as usize).copied() {
                        if let super::settings::Control::AliasRow(alias_idx) = item.control {
                            if let Some((from, _)) = self.settings.artist_aliases().get(alias_idx).cloned() {
                                self.settings.remove_artist_alias(&from, self.now_ms());
                            }
                            return;
                        }
                    }
                }
                Some(Hot::SettingSlider(row_i)) => {
                    if self.settings.remove_player_offset(row_i as usize, self.now_ms()).is_some() {
                        self.sync_profile_display();
                        return;
                    }
                }
                _ => {
                    if let Some(pr) = self.panel_bounds.last().copied() {
                        if let Some((row_i, _, _)) = self
                            .settings
                            .rows(pr)
                            .into_iter()
                            .find(|(_, item, row)| {
                                matches!(item.control, super::settings::Control::PlayerOffset(_))
                                    && row.contains(x, y)
                            })
                        {
                            if self.settings.remove_player_offset(row_i, self.now_ms()).is_some() {
                                self.sync_profile_display();
                                return;
                            }
                        }
                    }
                }
            }
        }
        let Some(Hot::Candidate(i)) = hit else {
            return;
        };
        let list = self.candidates();
        let Some(entry) = list.get(i).filter(|e| e.source == "custom") else {
            return;
        };
        let removed_id = entry.id.clone();
        let was_selected = self
            .doc_identity
            .as_ref()
            .is_some_and(|(s, id)| s == "custom" && id == &removed_id);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if !live.remove_custom_lyric(&removed_id) {
            return;
        }
        if was_selected {
            let updated = self.candidates();
            if let Some(next_custom) = updated.iter().find(|e| e.source == "custom") {
                if let Some(doc) = self
                    .live
                    .as_ref()
                    .and_then(|l| l.custom_lyrics.get_doc(&next_custom.id))
                {
                    self.doc_identity = Some(("custom".into(), next_custom.id.clone()));
                    self.doc_provider = "custom".into();
                    self.lyrics_auto_hidden = doc.instrumental;
                    self.install_document(doc, false);
                    return;
                }
            }
            if let Some(live) = self.live.as_mut() {
                if let (Some(track), Some(candidate)) = (live.track.clone(), live.candidates.first().cloned()) {
                    live.search.request(live.model.generation(), track, Some(candidate));
                    live.status = "正在加载歌词…".into();
                } else {
                    self.doc = None;
                    self.doc_identity = None;
                    self.doc_provider.clear();
                    self.lines.clear();
                    self.rows.clear();
                }
            }
        }
    }

    fn live_toggle(&mut self) {
        use oly_platform_win::smtc::control::Action;
        let playing = self
            .live
            .as_ref()
            .map_or(self.playing, |live| live.model.effective_playing());
        self.live_action(if playing { Action::Pause } else { Action::Play });
    }

    fn menu_mode_track(&self) -> Option<(f32, f32)> {
        let r0 = self.hot.iter().find(|(h, _)| *h == Hot::MenuMode(0))?.1;
        let r3 = self.hot.iter().find(|(h, _)| *h == Hot::MenuMode(3))?.1;
        Some((r0.x - 1.0, (r3.right() - r0.x + 2.0).max(1.0)))
    }

    fn settings_page_axis(&self) -> Option<(bool, f32, f32)> {
        let vertical = self.settings.kind == settings::SettingsKind::Application;
        let r0 = self.hot.iter().find(|(h, _)| *h == Hot::SettingsPage(0))?.1;
        let last = Hot::SettingsPage((self.settings.pages().len() - 1) as u8);
        let rn = self.hot.iter().find(|(h, _)| *h == last)?.1;
        if vertical {
            Some((true, r0.y - 2.0, (rn.bottom() - r0.y + 4.0).max(1.0)))
        } else {
            Some((false, r0.x - 2.0, (rn.right() - r0.x + 4.0).max(1.0)))
        }
    }

    fn has_active_press(&self) -> bool {
        self.pressed.is_some()
            || self.drag_panel.is_some()
            || self.drag_thumb.is_some()
            || self.switch_drag.is_some()
            || self.inline_switch_drag.is_some()
            || self.segmented_drag.is_some()
    }

    fn build_segmented_lens_glass(
        &mut self,
        window: &mut Window,
        kind: PanelKind,
        lens_rect: Box4,
        swell: f32,
        content_alpha: f32,
        fb: Feedback,
        scale: f32,
        vertical: bool,
    ) -> Option<PlanPanel> {
        let swell = swell.max(0.0);
        if swell <= 0.005 {
            return None;
        }
        let lens_box = Box4::new(
            lens_rect.x + fb.pull[0] * 0.35,
            lens_rect.y + fb.pull[1] * 0.25,
            lens_rect.w,
            lens_rect.h,
        );
        let pad = 44.0_f32;
        let req_w = (((lens_box.w + pad * 2.0) / 64.0).ceil().max(2.0)) as f64 * 64.0;
        let req_h = (((lens_box.h + pad * 2.0) / 64.0).ceil().max(2.0)) as f64 * 64.0;
        if !self.ensure_glass(window, kind, (req_w, req_h), scale) {
            return None;
        }
        let panel = match kind {
            PanelKind::TabLens => self.tab_lens_glass.as_mut()?,
            _ => self.segmented_lens_glass.as_mut()?,
        };
        let (pw, ph) = panel.pixels();
        let bw = pw as f64 / scale.max(0.5) as f64;
        let bh = ph as f64 / scale.max(0.5) as f64;
        let (cx, cy) = lens_box.centre();
        let left = (cx - bw as f32 * 0.5).round().max(0.0);
        let top = (cy - bh as f32 * 0.5).round().max(0.0);
        let backing = (bw, bh);
        let local_x = (lens_box.x - left).max(0.0) as f64 * scale as f64;
        let local_y = (lens_box.y - top).max(0.0) as f64 * scale as f64;
        let local_w = lens_box.w as f64 * scale as f64;
        let local_h = lens_box.h as f64 * scale as f64;
        let radius = (lens_box.h * 0.5) as f64 * scale as f64;
        panel.set_shape(
            GlassRect {
                left: local_x,
                top: local_y,
                width: local_w,
                height: local_h,
            },
            radius,
        );
        let (_, glass_alpha) = super::chrome::segmented_crossfade(swell);
        let lens_alpha = (content_alpha * glass_alpha * 0.96).clamp(0.0, 0.96);
        if self.display.theme_mode == model::ThemeMode::Light {
            panel.set_tint(
                [0.96, 0.975, 1.0],
                0.10,
                [1.0, 1.0, 1.0, 0.95],
                [0.12, 0.16, 0.24, 0.58],
            );
        } else {
            panel.set_tint(
                [0.97, 0.985, 1.0],
                0.03,
                [1.0, 1.0, 1.0, 0.34],
                [0.60, 0.68, 0.82, 0.14],
            );
        }
        panel.set_lens_liquid(scale, lens_alpha, swell, vertical);
        panel.set_light([(0.38 + fb.focus[0] * 0.25).clamp(0.0, 1.0), 0.16, 0.92]);
        Some(PlanPanel {
            content_alpha,
            surface_alpha: lens_alpha,
            kind,
            rect: GlassRect {
                left: left as f64,
                top: top as f64,
                width: backing.0,
                height: backing.1,
            },
            surface: panel.surface(),
            snapshot: panel.snapshot(),
            hook: panel.hook(),
        })
    }

    fn on_down(&mut self, x: f32, y: f32, _active: bool) {
        self.cursor = Some((x, y));
        if self.settings.open {
            if let Some(modal) = self.settings.modal.clone() {
                let panel_r = self.panel_bounds.last().copied().unwrap_or(Box4::new(0.0, 0.0, 540.0, 440.0));
                let mp = self.panel_elastics[4].pull();
                let (box_r, f1, f2, cancel, save) = Settings::modal_rects_with_offset(panel_r, &modal, mp);
                if cancel.contains(x, y) {
                    self.pressed = Some(Hot::ModalCancel);
                    self.press_gesture.begin(x, y);
                } else if save.w > 0.0 && save.contains(x, y) {
                    self.pressed = Some(Hot::ModalSave);
                    self.press_gesture.begin(x, y);
                } else if f1.contains(x, y) || f2.is_some_and(|r| r.contains(x, y)) || !box_r.contains(x, y) {
                    self.settings.handle_modal_click(x, y, panel_r, mp, self.now_ms());
                } else {
                    self.panel_press_candidate = Some(4);
                    self.press_gesture.begin(x, y);
                }
                return;
            }
        }
        let hit = pointer_hit(&self.hot,&self.panel_bounds,x,y);
        let editing_same = if let Some(Hot::SettingChoice(i))=hit {
            self.settings.items().get(i as usize).is_some_and(|item|Some(item.key)==self.settings.editing_key && matches!(item.control,Control::Text{..}))
        } else {false};
        if self.settings.editing_key.is_some() && !editing_same {self.settings.finish_edit(self.now_ms(),true);}
        self.pressed_candidate=if let Some(Hot::Candidate(i))=hit{self.candidates().get(i).map(|r|(r.source.clone(),r.id.clone(),r.candidate_index))}else{None};
        self.pressed = hit;
        self.press_gesture.begin(x,y);
        self.cursor=Some((x,y));
        if self.settings.open_choice.is_some()
            && !matches!(hit, Some(Hot::DropdownOption(_) | Hot::SettingChoice(_)))
        {
            let in_dropdown = self.panel_bounds.last().is_some_and(|panel_r| {
                self.settings.dropdown_layout(*panel_r).is_some_and(|mut d| {
                    let dp = self.panel_elastics[4].pull();
                    d.translate(dp[0], dp[1]);
                    d.rect.contains(x, y)
                })
            });
            if in_dropdown {
                self.pressed = None;
                self.drag_panel = Some(4);
                self.panel_press_candidate = None;
                return;
            }
            self.settings.close_choice();
        }
        if let Some(Hot::CoverSwitch(i)) = hit {
            let spring_key = if i == 0 { "cover:showCover" } else { "cover:motionCover" };
            let setting_key = if i == 0 { "showCover" } else { "motionCover" };
            let current_on = self.settings.value(setting_key).as_bool().unwrap_or(true);
            if let Some((_, row_r)) = self.hot.iter().find(|(h, _)| *h == Hot::CoverSwitch(i)) {
                let track = cover_switch_track_rect(*row_r);
                if track.contains(x, y) {
                    self.inline_switch_drag = Some(InlineSwitchDrag {
                        key: spring_key.into(),
                        track,
                        origin: x,
                        fraction: if current_on { 1.0 } else { 0.0 },
                        moved: false,
                    });
                    return;
                }
            }
        }
        if let Some(Hot::SmartTranslation(i)) = hit {
            if let Some(entry) = self.candidates().get(i) {
                if !entry.has_translation && !entry.smart_translation_key.is_empty() {
                    if let Some((_, row_r)) = self.hot.iter().find(|(h, _)| *h == Hot::Candidate(i)) {
                        let track = picker_smart_switch_rect(*row_r);
                        if track.contains(x, y) {
                            self.inline_switch_drag = Some(InlineSwitchDrag {
                                key: entry.smart_translation_key.clone(),
                                track,
                                origin: x,
                                fraction: if entry.smart_translation { 1.0 } else { 0.0 },
                                moved: false,
                            });
                            return;
                        }
                    }
                }
            }
        }
        if let Some(Hot::SettingChoice(index))=hit {
            if let Some((_,item,row))=self.panel_bounds.last().and_then(|r|self.settings.rows(*r).into_iter().find(|(i,_,_)|*i==index as usize)) {
                let track=super::details::switch_track(row);
                if matches!(item.control,Control::Switch) && track.contains(x,y){
                    let fraction=if self.settings.value(item.key).as_bool().unwrap_or(false){1.}else{0.};
                    self.switch_drag=Some(super::details::SwitchDrag{index:index as usize,track,origin:x,fraction,moved:false});return;
                }
                if let Control::Segmented(options) = item.control {
                    let seg_track = Settings::segmented_track_rect(row, options.len());
                    if seg_track.contains(x, y) {
                        self.segmented_drag = Some((index as usize, item.key, seg_track, options.len()));
                        self.settings.begin_segmented_drag(item.key, x, seg_track, options.len());
                        return;
                    }
                }
            }
        }
        if matches!(hit, Some(Hot::MenuMode(_))) {
            self.staged_mode = None;
            if let Some((tx, tw)) = self.menu_mode_track() {
                self.mode_lens.update_drag(x, tx, tw, 4, 1.0);
            }
            return;
        }
        if matches!(hit, Some(Hot::SettingsPage(_))) {
            if let Some((vertical, origin, span)) = self.settings_page_axis() {
                let pos = if vertical { y } else { x };
                self.settings.tab_lens.update_drag(pos, origin, span, self.settings.pages().len(), 2.0);
            }
            return;
        }
        let panel_hit = self
            .panel_drag_targets
            .iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
            .map(|(idx, _)| *idx);
        if hit.is_none() {
            self.drag_panel = panel_hit;
            self.panel_press_candidate = None;
        } else {
            self.panel_press_candidate = panel_hit;
        }
        if hit.is_none() || matches!(hit,Some(Hot::Progress | Hot::VolumeTrack | Hot::Thumb | Hot::SettingSlider(_))) {self.activate(hit,x,y);}
    }
    fn on_up(&mut self,x:f32,y:f32) {
        if let Some(mut drag) = self.inline_switch_drag.take() {
            drag.moved |= (x - drag.origin).abs() > 2.0;
            if drag.moved {
                drag.fraction = super::details::switch_fraction(drag.track, x);
            }
            if drag.key == "__motion__" {
                let next = if drag.moved {
                    drag.fraction >= 0.5
                } else {
                    !self.display.motion_cover
                };
                self.change_setting("display.motionCover", serde_json::json!(next));
            } else if drag.key == "cover:showCover" || drag.key == "cover:motionCover" {
                let setting_key = if drag.key == "cover:showCover" {
                    "showCover"
                } else {
                    "motionCover"
                };
                let current_on = self.settings.value(setting_key).as_bool().unwrap_or(true);
                let next = if drag.moved {
                    drag.fraction >= 0.5
                } else {
                    !current_on
                };
                self.change_setting(setting_key, serde_json::json!(next));
                if drag.key == "cover:showCover" && !next {
                    self.picker_open = false;
                    self.tool.set_more_open(false);
                    self.panel_motion[0] = PanelMotion::default();
                    self.panel_motion[2] = PanelMotion::default();
                }
            } else {
                let current_on = self
                    .candidates()
                    .into_iter()
                    .find(|c| c.smart_translation_key == drag.key)
                    .map(|c| c.smart_translation)
                    .unwrap_or_else(|| {
                        self.settings.config["lyrics"]["translationOverrides"]
                            .get(&drag.key)
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    });
                let next = if drag.moved {
                    drag.fraction >= 0.5
                } else {
                    !current_on
                };
                let mut overrides = self.settings.config["lyrics"]["translationOverrides"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                overrides.insert(drag.key, serde_json::json!(next));
                self.change_setting("lyrics.translationOverrides", serde_json::Value::Object(overrides));
            }
            self.cancel_press();
            return;
        }
        if let Some(mut drag)=self.switch_drag.take(){
            drag.update(x);self.settings.finish_switch(drag.index,drag.fraction,drag.moved,self.now_ms());
            self.sync_profile_display();self.cancel_press();return;
        }
        if let Some((index, _, track, count)) = self.segmented_drag.take() {
            if self.settings.finish_segmented_drag(index, x, track, count, self.now_ms()) {
                self.sync_profile_display();
            }
            self.cancel_press();
            return;
        }
        self.press_gesture.update(x,y);
        let hit=self.pressed;
        if let Some(Hot::MenuMode(initial)) = hit {
            let slot = if let Some((tx, tw)) = self.menu_mode_track() {
                self.mode_lens.slot_at_release(tx, tw, 4, x) as u8
            } else {
                initial
            };
            self.mode_lens.end_drag();
            self.staged_mode = Some(slot);
            self.cancel_press();
            return;
        }
        if let Some(Hot::SettingsPage(initial)) = hit {
            let page = if let Some((vertical, origin, span)) = self.settings_page_axis() {
                let pos = if vertical { y } else { x };
                self.settings.tab_lens.slot_at(origin, span, self.settings.pages().len(), pos)
            } else {
                initial as usize
            };
            self.settings.tab_lens.end_drag();
            self.settings.finish_edit(self.now_ms(), true);
            self.settings.set_page(page);
            self.cancel_press();
            return;
        }
        if matches!(hit, Some(Hot::DropdownOption(_))) {
            if self.drag_panel.is_none() {
                if let Some(Hot::DropdownOption(opt_i)) = pointer_hit(&self.hot, &self.panel_bounds, x, y) {
                    self.activate(Some(Hot::DropdownOption(opt_i)), x, y);
                }
            }
            self.cancel_press();
            return;
        }
        let same=hit.is_some()&&pointer_hit(&self.hot,&self.panel_bounds,x,y)==hit;
        let click=self.press_gesture.release(same);
        if click&&!matches!(hit,Some(Hot::Progress | Hot::VolumeTrack | Hot::Thumb | Hot::SettingSlider(_))) {self.activate(hit,x,y);}
        if self.source == Source::Live && hit == Some(Hot::Progress) {
            let target = self
                .hot
                .iter()
                .find(|(h, _)| *h == Hot::Progress)
                .map(|(_, r)| ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64 * self.duration_ms)
                .or(self.seek_preview_ms);
            self.seek_preview_ms = None;
            if let Some(target_ms) = target {
                self.live_seek(target_ms);
            }
        }
        self.cancel_press();
    }
    fn cancel_press(&mut self) {
        self.seek_preview_ms=None;
        self.pressed_candidate=None;
        self.drag_panel=None;
        self.panel_press_candidate=None;
        self.inline_switch_drag=None;
        self.switch_drag=None;self.settings.switch_preview=None;
        self.segmented_drag=None;self.settings.cancel_segmented_drag();
        self.pressed=None;
        self.press_gesture.cancel();
        self.drag_thumb=None;
        self.scroll.set_thumb_held(false);
        self.mode_lens.end_drag();
        self.settings.tab_lens.end_drag();
    }
    fn click_at(&mut self,x:f32,y:f32) {self.on_down(x,y,true);self.on_up(x,y);}
    fn activate(&mut self,hit:Option<Hot>,x:f32,y:f32) {
        if self.source == Source::Live {
            use oly_platform_win::smtc::control::Action;
            match hit {
                Some(Hot::Play) => { self.live_toggle(); return; }
                Some(Hot::Prev) => { self.live_action(Action::Prev); return; }
                Some(Hot::Next) => { self.live_action(Action::Next); return; }
                _ => {}
            }
        }
        match hit {
            Some(Hot::Play) => {
                self.playing = !self.playing;
            }
            Some(Hot::Prev) => {
                self.position_ms = (self.position_ms - 5000.0).max(0.0);
                self.pending_seek = true;
            }
            Some(Hot::Next) => {
                self.position_ms = (self.position_ms + 5000.0).min(self.duration_ms.max(0.0));
                self.pending_seek = true;
            }
            Some(Hot::More) => {
                let open = self.tool.toggle_more();
                if open {
                    self.picker_open = false;
                }
                self.close_volume_panel();
                self.settings.close_choice();
                self.settings.open=false;
                if let Some((_, fb)) = self.feedback.iter_mut().find(|(h, _)| *h == Hot::More) {
                    fb.trigger_hop(1.3);
                }
            }
            Some(Hot::LyricsToggle) => {
                if self.lyrics_hidden() {
                    self.lyrics_user_hidden = false;
                    if self.lyrics_auto_hidden {
                        self.lyrics_user_force_shown = true;
                    }
                } else if self.lyrics_auto_hidden && self.lyrics_user_force_shown {
                    self.lyrics_user_force_shown = false;
                } else {
                    self.lyrics_user_force_shown = false;
                    self.lyrics_user_hidden = true;
                }
                if let Some((_, fb)) = self.feedback.iter_mut().find(|(h, _)| *h == Hot::LyricsToggle) {
                    fb.trigger_hop(1.3);
                }
            }
            Some(Hot::Fold)=>{self.pending_mode=Some(if self.tool.mode()==Some(Mode::Mini){1}else{0});}
            Some(Hot::Volume) => {
                self.settings.close_choice();
                self.settings.open=false;
                self.picker_open = false;
                let open = self.tool.toggle_volume();
                self.tool.set_more_open(false);
                if open {
                    if self.source == Source::Live {
                        self.open_live_volume(self.now_ms());
                    } else {
                        self.volume = VolumePanel::open("", Some(self.volume.value()), VolumeSource::Internal);
                        self.volume.on_read(Some(self.volume.value()), VolumeSource::Internal);
                    }
                } else {
                    self.close_volume_panel();
                }
                if let Some((_, fb)) = self.feedback.iter_mut().find(|(h, _)| *h == Hot::Volume) {
                    fb.trigger_hop(1.3);
                }
            }
            Some(Hot::Progress) => {
                if let Some((_, r)) = self.hot.iter().find(|(h, _)| *h == Hot::Progress) {
                    let f = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64;
                    let target = f * self.duration_ms;
                    if self.source == Source::Live {
                        if self.can_seek_live() {
                            self.seek_preview_ms = Some(target);
                        }
                    } else {
                        self.position_ms = target;
                        self.pending_seek = true;
                    }
                }
            }
            Some(Hot::Thumb) => {
                self.drag_thumb = Some((y, self.scroll.offset()));
                self.scroll.set_thumb_held(true);
            }
            Some(Hot::SyncMinus)=>self.nudge_sync(-200),
            Some(Hot::SyncPlus)=>self.nudge_sync(200),
            Some(Hot::LyricRow(idx)) => {
                if let Some(row) = self.rows.get(idx) {
                    let offset_ms = self.player_base_offset_ms() + self.sync.offset_ms() as f64;
                    let target_ms = (row.start_ms - offset_ms).clamp(0.0, self.duration_ms.max(0.0));
                    if self.source == Source::Live {
                        if self.can_seek_live() {
                            self.live_seek(target_ms);
                        }
                    } else {
                        self.position_ms = target_ms;
                        self.pending_seek = true;
                    }
                }
            }
            Some(Hot::MenuMode(mode))=>{self.pending_mode=Some(mode);},
            Some(Hot::MoreRow(1)) => {
                self.picker_kind = PickerKind::Lyrics;
                self.picker_start = 0;
                self.picker_open = true;
            }
            Some(Hot::MoreRow(2)) => {
                self.picker_kind = PickerKind::Cover;
                self.picker_start = 0;
                self.picker_open = true;
            }
            Some(Hot::MoreRow(i)) => {
                let has_media = self.has_multiple_media_sessions();
                if has_media && i == 3 {
                    self.picker_kind = PickerKind::Media;
                    self.picker_start = 0;
                    self.picker_open = true;
                } else if (has_media && i == 4) || (!has_media && i == 3) {
                    self.settings.set_kind(settings::SettingsKind::Application);
                    self.settings.scroll = 0.0;
                    self.settings.close_choice();
                    if matches!(self.tool.mode(),Some(Mode::Mini|Mode::Lyrics)) && self.settings_host.is_none() {
                        self.settings.open = false;
                        self.pending_settings_window = Some(0);
                    } else {
                        self.settings.open = true;
                    }
                    self.tool.set_more_open(false);
                    self.picker_open = false;
                }
            }
            Some(Hot::SettingsClose)=>{self.settings.finish_edit(self.now_ms(),true);self.settings.close_choice();self.settings.open=false;}
            Some(Hot::SettingsPage(page))=>{self.settings.finish_edit(self.now_ms(),true);self.settings.set_page(page as usize);}
            Some(Hot::SettingChoice(index))=>{
                if self.settings.items().get(index as usize).is_some_and(|item|Some(item.key)==self.settings.editing_key){return;}
                if self.settings.activate_row(index as usize, self.now_ms()) {
                    self.sync_profile_display();
                }
            }
            Some(Hot::DropdownOption(opt_i))=>{
                if self.settings.select_choice_option(opt_i as usize, self.now_ms()) {
                    self.sync_profile_display();
                }
            }
            Some(Hot::ModalCancel) => {
                self.settings.modal = None;
            }
            Some(Hot::ModalSave) => {
                self.settings.commit_modal(self.now_ms());
            }
            Some(Hot::SettingSlider(index))=>{self.slide_setting(index,x);}
            Some(Hot::WindowMinimize) => {
                self.pending_minimize = true;
            }
            Some(Hot::WindowFullscreen) => {
                self.pending_mode = Some(if self.last_fullscreen { self.pre_fullscreen_mode } else { 3 });
            }
            Some(Hot::WindowClose) => {
                self.pending_quit = true;
            }

            Some(Hot::VolumeTrack) => {
                if let Some((_, r)) = self.hot.iter().find(|(h, _)| *h == Hot::VolumeTrack) {
                    let f = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64;
                    let value = SLIDER_MIN + f * (SLIDER_MAX - SLIDER_MIN);
                    if let Some(write) = self.volume.on_slide(value, self.now_ms()) {
                        self.send_volume_write(write);
                    }
                }
            }
            Some(Hot::CoverSwitch(i)) => {
                let setting_key = if i == 0 { "showCover" } else { "motionCover" };
                let current_on = self.settings.value(setting_key).as_bool().unwrap_or(true);
                self.change_setting(setting_key, serde_json::json!(!current_on));
                if i == 0 && current_on {
                    self.picker_open = false;
                    self.tool.set_more_open(false);
                    self.panel_motion[0] = PanelMotion::default();
                    self.panel_motion[2] = PanelMotion::default();
                }
            }
            Some(Hot::SmartTranslation(i)) => {
                if let Some(entry) = self.candidates().get(i) {
                    let key = entry.smart_translation_key.clone();
                    if !key.is_empty() {
                        let on = !entry.smart_translation;
                        let mut overrides = self.settings.config["lyrics"]["translationOverrides"]
                            .as_object()
                            .cloned()
                            .unwrap_or_default();
                        overrides.insert(key, serde_json::json!(on));
                        self.change_setting("lyrics.translationOverrides", serde_json::Value::Object(overrides));
                    }
                }
            }
            Some(Hot::AlbumEnter) => {
                if self.tool.mode() == Some(Mode::Mini) {
                    self.pending_mode = Some(4);
                }
            }
            Some(Hot::AlbumReturn) => {
                if self.tool.mode() == Some(Mode::Album) {
                    self.pending_mode = Some(0);
                }
            }
            Some(Hot::PickerImport) => {
                if self.pending_import_rx.is_none() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.pending_import_rx = Some(rx);
                    std::thread::Builder::new()
                        .name("lyric-file-dialog".into())
                        .spawn(move || {
                            let _ = tx.send(oly_platform_win::file_dialog::pick_lyric_file());
                        })
                        .ok();
                }
            }
            Some(Hot::PickerRefresh) => {
                if let Some((_, fb)) = self.feedback.iter_mut().find(|(h, _)| *h == Hot::PickerRefresh) {
                    fb.trigger_hop(1.8);
                }
                self.picker_refresh_spinning = true;
                let locked_rows = self.candidates().len().clamp(1, 4);
                if let Some(live) = self.live.as_mut() {
                    live.search.clear_cache();
                    self.picker_start = 0;
                    let valid_track = !live
                        .model
                        .packet
                        .as_ref()
                        .is_none_or(oly_contracts::PlayerPacket::is_idle_state);
                    if let Some(track) = live.track.clone().filter(|_| valid_track) {
                        self.picker_refresh_active = true;
                        self.picker_locked_rows = Some(locked_rows);
                        live.search.request(live.model.generation(), track, None);
                        live.status = "正在搜索歌词…".into();
                    }
                }
            }
            Some(Hot::Candidate(i)) => {
                self.tool.set_more_open(true);
                self.picker_open = true;
                let visible=self.candidates();
                if self.picker_kind == PickerKind::Media {
                    if let Some(entry) = visible.get(i) {
                        if let Some(live) = self.live.as_mut() {
                            let target = if entry.source == "media-auto" {
                                None
                            } else {
                                Some(entry.id.clone())
                            };
                            live.pinned_media_app = target.clone();
                            let _ = live.host.dispatch(crate::commands::HostCommand::SetPinnedSession(target));
                        }
                    }
                    return;
                }
                if let Some(custom_entry) = visible.get(i).filter(|r| r.source == "custom") {
                    if let Some(doc) = self
                        .live
                        .as_ref()
                        .and_then(|l| l.custom_lyrics.get_doc(&custom_entry.id))
                    {
                        self.doc_auto_picked = false;
                        self.auto_pick_persisted = false;
                        self.doc_identity = Some(("custom".into(), custom_entry.id.clone()));
                        self.doc_provider = "custom".into();
                        self.lyrics_auto_hidden = doc.instrumental;
                        self.install_document(doc, false);
                    }
                    return;
                }
                let wanted=self.pressed_candidate.clone().or_else(||visible.get(i).map(|r|(r.source.clone(),r.id.clone(),r.candidate_index))).filter(|(source,id,_)|visible.iter().any(|r|&r.source==source && &r.id==id));
                if let Some(live) = self.live.as_mut() {
                    let selected=wanted.and_then(|(source,id,index)|live.candidates.get(index).filter(|c|c.source==source && c.id==id).or_else(||live.candidates.iter().find(|c|c.source==source && c.id==id)).cloned());
                    if let (Some(track), Some(candidate)) = (live.track.clone(), selected) {
                        live.search.request(live.model.generation(), track, Some(candidate));
                        live.status = "正在加载歌词…".into();

                    }
                    return;
                }
                let list = self.candidates();
                if let Some(entry) = list.get(i) {
                    let path = entry.path.clone();
                    self.load_document(&path);
                }
            }
            None => {
                if self.settings.open && !self.panel_bounds.last().is_some_and(|r|r.contains(x,y)) {self.settings.close_choice();self.settings.open=false;}
                // A pointer down outside both panels closes them, exactly like
                // the legacy capture-phase listener.
                if !self.panel_bounds.iter().any(|r|r.contains(x,y)) && !self.hot.iter().any(|(h, r)| r.contains(x, y) && !matches!(h, Hot::Progress)) {
                    self.tool.set_more_open(false);
                    self.close_volume_panel();
                }
            }
        }
    }

    fn change_setting(&mut self,key:&str,value:serde_json::Value) {
        self.settings.set(key,value,self.now_ms(),false);
        self.sync_profile_display();
        self.sync_settings_child_config = true;
    }
    fn slide_setting(&mut self,index:u8,x:f32) {
        if let Some((_,r))=self.hot.iter().find(|(h,_)|*h==Hot::SettingSlider(index)) {
            self.settings.slide(index as usize,(x-r.x)/r.w.max(1.0),self.now_ms());
            self.sync_profile_display();
        }
    }
    fn on_drag(&mut self, x: f32, y: f32) {
        if let Some(drag) = self.inline_switch_drag.as_mut() {
            drag.moved |= (x - drag.origin).abs() > 2.0;
            if drag.moved {
                drag.fraction = super::details::switch_fraction(drag.track, x);
                if let Some(spring) = self.smart_switch_springs.get_mut(&drag.key) {
                    spring.set_position(drag.fraction as f64);
                }
            }
            return;
        }
        if let Some(drag)=self.switch_drag.as_mut(){drag.update(x);if drag.moved{self.settings.preview_switch(drag.index,drag.fraction);}return;}
        if let Some((_, key, track, count)) = self.segmented_drag {
            self.settings.update_segmented_drag(key, x, track, count);
            return;
        }
        self.press_gesture.update(x,y);
        if matches!(self.pressed, Some(Hot::MenuMode(_))) {
            if let Some((tx, tw)) = self.menu_mode_track() {
                self.mode_lens.update_drag(x, tx, tw, 4, 1.0);
                let slot = self.mode_lens.slot_at(tx, tw, 4, x) as u8;
                self.pressed = Some(Hot::MenuMode(slot));
            }
            return;
        }
        if matches!(self.pressed, Some(Hot::SettingsPage(_))) {
            if let Some((vertical, origin, span)) = self.settings_page_axis() {
                let pos = if vertical { y } else { x };
                let count = self.settings.pages().len();
                self.settings.tab_lens.update_drag(pos, origin, span, count, 2.0);
                let slot = self.settings.tab_lens.slot_at(origin, span, count, pos);
                if self.settings.page != slot {
                    self.settings.set_page(slot);
                }
                self.pressed = Some(Hot::SettingsPage(slot as u8));
            }
            return;
        }
        if matches!(self.pressed, Some(Hot::DropdownOption(_))) {
            if let Some(Hot::DropdownOption(opt_i)) = pointer_hit(&self.hot, &self.panel_bounds, x, y) {
                self.pressed = Some(Hot::DropdownOption(opt_i));
            }
            return;
        }
        if let Some(Hot::SettingSlider(index))=self.pressed {self.slide_setting(index,x);return;}
        if self.pressed == Some(Hot::Progress) {
            if let Some((_, r)) = self.hot.iter().find(|(h, _)| *h == Hot::Progress) {
                let f = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64;
                let target = f * self.duration_ms;
                if self.source == Source::Live {
                    if self.can_seek_live() {
                        self.seek_preview_ms = Some(target);
                    }
                } else {
                    self.position_ms = target;
                    self.pending_seek = true;
                }
            }
            return;
        }
        if let Some((origin_y, origin_offset)) = self.drag_thumb {
            self.browse_idle_ms = self.now_ms();
            // The track is vertical, so the gesture is too: one pixel of thumb
            // travel covers `span / track` pixels of document. Dragging the
            // thumb *down* moves towards later content, which the engine stores
            // as a decreasing offset.
            let span = (self.scroll.engine().max_offset() - self.scroll.engine().min_offset()).max(1.0);
            let travel = (self.chrome.scroll_track.h - self.chrome.scroll_thumb.h).max(1.0) as f64;
            let factor = span / travel;
            let dy = (y - origin_y) as f64;
            self.scroll.move_to(origin_offset - dy * factor);
        }
        if self.pressed == Some(Hot::VolumeTrack) {
            if let Some((_, r)) = self.hot.iter().find(|(h, _)| *h == Hot::VolumeTrack) {
                let f = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64;
                let value = SLIDER_MIN + f * (SLIDER_MAX - SLIDER_MIN);
                if let Some(write) = self.volume.on_slide(value, self.now_ms()) {
                    self.send_volume_write(write);
                }
            }
        }
        if self.drag_panel.is_none()
            && self.press_gesture.dragged()
            && self.drag_thumb.is_none()
            && !matches!(
                self.pressed,
                Some(Hot::SettingSlider(_) | Hot::VolumeTrack | Hot::Progress)
            )
        {
            if let Some(idx) = self.panel_press_candidate.take() {
                self.drag_panel = Some(idx);
                self.pressed = None;
                self.pressed_candidate = None;
            }
        }
    }

    fn on_key(&mut self, key: &str) {
        if self.settings.handle_modal_key(key, self.now_ms()) {
            return;
        }
        if self.settings.edit_key(key,self.now_ms()){return;}
        if self.source == Source::Live && key == "space" { self.live_toggle(); return; }
        if self.source == Source::Live && matches!(key, "left" | "right") {
            return;
        }
        match key {
            "escape" => {
                if self.settings.open {
                    if self.settings.open_choice.is_some() {
                        self.settings.close_choice();
                    } else {
                        self.settings.open = false;
                    }
                    return;
                }
                // Escape unwinds one level at a time: the version list is a
                // child of the more panel, so it goes first and the panel stays
                // up. A second Escape closes the panel itself.
                if self.picker_open && self.tool.panels().more_open {
                    self.picker_open = false;
                } else if self.tool.panels().more_open || self.tool.panels().volume_open {
                    self.tool.set_more_open(false);
                    self.close_volume_panel();
                } else if self.last_fullscreen && self.settings_host.is_none() {
                    self.activate(Some(Hot::WindowFullscreen), 0.0, 0.0);
                }
            }
            "f11" => {
                if self.settings_host.is_none() {
                    self.activate(Some(Hot::WindowFullscreen), 0.0, 0.0);
                }
            }
            "space" => {
                self.playing = !self.playing;
            }
            "left" => {
                self.position_ms = (self.position_ms - 5000.0).max(0.0);
                self.pending_seek = true;
            }
            "right" => {
                self.position_ms = (self.position_ms + 5000.0).min(self.duration_ms.max(0.0));
                self.pending_seek = true;
            }
            _ => {}
        }
    }
}

/// One fixture file: the envelope keeps the document under `doc`.
fn load_fixture(path: &std::path::Path) -> anyhow::Result<(LyricDoc,String)> {
    let value:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let doc=value.get("doc").unwrap_or(&value);
    let provider=doc.get("provider").and_then(|p|p.as_str()).unwrap_or("fixture").to_string();
    Ok((serde_json::from_value(doc.clone())?,provider))
}

fn paint_frame(
    window: &mut Window,
    cx: &mut App,
    plan: &Plan,
    more_items: &[&str],
    candidates: &[PickerEntry],
    _picked: usize,
    volume_value: f64,
) {
    let light_window = plan.display.theme_mode == model::ThemeMode::Light && !plan.immersion && plan.mode == Mode::Window;
    let light_panel = plan.display.theme_mode == model::ThemeMode::Light;
    chrome::set_light_theme(if plan.settings_only { light_panel } else { light_window });
    let m = &plan.metrics;
    let feedback = |key: Hot| plan.feedback.iter().find(|(h,_)| *h == key).map(|(_,f)| *f).unwrap_or_default();
    let win_radius = if plan.fullscreen { 0.0 } else if !plan.immersion {8.0} else { plan.display.corner_radius };
    let panel_radius = if plan.settings_only{8.0}else{plan.display.corner_radius.clamp(8.0, 28.0)};

    if !plan.settings_only {
    // 1. Immersion background, full bleed, behind everything.
    if let Some(bg) = plan.bg {
        window.paint_custom_surface_rounded(
            gpui_kit::Bounds {
                origin: point(px(0.0), px(0.0)),
                size: gpui_kit::size(px(m.win_w), px(m.win_h)),
            },
            px(win_radius),
            bg,
        );
    } else {
        let bg_color = match plan.display.theme_mode {
            model::ThemeMode::Light if !plan.immersion && plan.mode == Mode::Window => {
                Hsla::from(rgb(0xFA_FC_FF)).opacity(0.90)
            }
            model::ThemeMode::Dark if !plan.immersion => {
                Hsla::from(rgb(0x14_16_1A)).opacity(0.84)
            }
            _ => Hsla::from(rgb(0x18_19_1C)).opacity(if plan.immersion { 1.0 } else { 0.29 }),
        };
        chrome::fill_round(
            window,
            0.0,
            0.0,
            m.win_w,
            m.win_h,
            win_radius,
            bg_color,
        );
    }

    let fg_alpha = plan.content_alpha.clamp(0.0, 1.0);
    let cg_alpha = plan.chrome_alpha.clamp(0.0, 1.0);
    if fg_alpha > 0.005 {
    // 2. Cover: motion first (it carries its own colour), else the still, else
    //    the reference's `.cover-blank` placeholder.
    let cover_vis = if plan.mode == Mode::Window { plan.cover_vis } else { 1.0 };
    let cover_alpha = (fg_alpha * cover_vis).clamp(0.0, 1.0);
    if m.art_height > 0.0 && cover_alpha > 0.01 {
        let cover = if plan.mode == Mode::Window && (plan.cover_scale - 1.0).abs() > 0.0005 {
            let s = plan.cover_scale.clamp(0.5, 1.15);
            let (cx_c, cy_c) = m.cover.centre();
            let cw = m.cover.w * s;
            let ch = m.cover.h * s;
            Box4::new(cx_c - cw * 0.5, cy_c - ch * 0.5, cw, ch)
        } else {
            m.cover
        };
        let scale_r = if m.cover.w > 1.0 { cover.w / m.cover.w } else { 1.0 };
        let radius = if plan.mode == Mode::Album {
            win_radius
        } else if matches!(plan.mode, Mode::Mini | Mode::Lyrics) {
            super::details::compact_cover_radius(plan.display.cover_radius)
        } else {
            (plan.display.cover_radius * scale_r * m.ui_scale).min(cover.w * 0.5)
        };
        let shadows: &[BoxShadow] = if plan.mode == Mode::Album {
            &[]
        } else if matches!(plan.mode, Mode::Mini | Mode::Lyrics) {
            &[BoxShadow::new(
                px(0.0),
                px(cover.w * 0.02),
                Hsla::from(rgb(0x000000)).opacity(0.24 * cover_alpha),
            )
            .blur_radius(px(cover.w * 0.035))]
        } else {
            &[
                BoxShadow::new(
                    px(0.0),
                    px(cover.w * 0.0367),
                    Hsla::from(rgb(0x000000)).opacity(0.52 * cover_alpha),
                )
                .blur_radius(px(cover.w * 0.05)),
                BoxShadow::new(
                    px(0.0),
                    px(cover.w * 0.0133),
                    Hsla::from(rgb(0x000000)).opacity(0.42 * cover_alpha),
                )
                .blur_radius(px(cover.w * 0.02165)),
            ]
        };
        window.paint_drop_shadows(
            gpui_kit::Bounds {
                origin: point(px(cover.x), px(cover.y)),
                size: gpui_kit::size(px(cover.w), px(cover.h)),
            },
            Corners::all(px(radius)),
            shadows,
        );
        let still_needed = plan.cover.is_some()
            && (plan.motion.is_none() || plan.motion_alpha < 0.999);
        if still_needed {
            if let Some(still) = plan.cover {
                let b = gpui_kit::Bounds {
                    origin: point(px(cover.x), px(cover.y)),
                    size: gpui_kit::size(px(cover.w), px(cover.h)),
                };
                if cover_alpha < 0.999 {
                    window.paint_custom_surface_rounded_opacity(b, px(radius), still, cover_alpha);
                } else {
                    window.paint_custom_surface_rounded(b, px(radius), still);
                }
            }
        }
        if let Some(motion) = plan.motion.filter(|_| plan.motion_alpha > 0.001) {
            window.paint_custom_surface_rounded_opacity(
                gpui_kit::Bounds {
                    origin: point(px(cover.x), px(cover.y)),
                    size: gpui_kit::size(px(cover.w), px(cover.h)),
                },
                px(radius),
                motion,
                (plan.motion_alpha * cover_alpha).clamp(0.0, 1.0),
            );
        } else if plan.cover.is_none() && cover_alpha > 0.01 {
            chrome::cover_blank(window, cx, cover, radius, cover_alpha);
        }
        if fg_alpha < 0.995 && cover_vis > 0.99 {
            chrome::fill_round(
                window,
                cover.x,
                cover.y,
                cover.w,
                cover.h,
                radius,
                Hsla::from(rgb(0x18_19_1C)).opacity(((1.0 - fg_alpha) * 0.9).clamp(0.0, 1.0)),
            );
        }
        if plan.mode == Mode::Mini {
            let h = feedback(Hot::AlbumEnter).hover.clamp(0.0, 1.0);
            if h > 0.01 {
                chrome::fill_round(
                    window,
                    cover.x,
                    cover.y,
                    cover.w,
                    cover.h,
                    radius,
                    Hsla::from(rgb(0x00_00_00)).opacity(0.42 * h * fg_alpha),
                );
                let (cx_c, cy_c) = cover.centre();
                icons::paint(
                    window,
                    cx,
                    Icon::CoverExpand,
                    Box4::new(cx_c - 10.0, cy_c - 10.0, 20.0, 20.0),
                    chrome::text(0.92 * h * fg_alpha),
                );
            }
        }
        chrome::stroke_round(
            window,
            cover.x,
            cover.y,
            cover.w,
            cover.h,
            radius,
            0.85,
            chrome::text(0.14 * cover_alpha),
        );
    }

    // 3. Lyrics, clipped to the lyric column so a row never spills over the
    //    cover or the chrome.
    let lyric_bottom=m.lyrics.bottom()-if plan.mode==Mode::Lyrics && plan.chrome_visible{164.}else{0.};
    let feather_top = m.lyrics.y + if m.feather_px > 4.0 { 4.0 } else { 0.0 };
    let glow_pad = (m.font * 0.45).min(18.0);
    let mask_left = (m.lyric_viewport.x - glow_pad).max(0.0);
    let mask_right = (m.lyric_viewport.right() + glow_pad).min(m.win_w);
    let mask = ContentMask {
        bounds: gpui_kit::Bounds {
            origin: point(px(mask_left), px(m.lyric_viewport.y)),
            size: gpui_kit::size(px((mask_right - mask_left).max(1.0)), px((m.lyric_viewport.h-if plan.mode==Mode::Lyrics && plan.chrome_visible{164.}else{0.}).max(1.))),
        },
    };
    if plan.lyrics_alpha > 0.005 {
    window.with_content_mask(Some(mask), |window| {
        if let Some(notice) = &plan.empty_notice {
            chrome::paint_text_box_centred(
                window,
                notice,
                m.lyrics,
                15.0,
                500.0,
                chrome::text(0.45 * plan.lyrics_alpha * fg_alpha),
            );
        }
        if let Some((r, alpha)) = plan.lyric_hover {
            chrome::fill_round(window, r.x, r.y, r.w, r.h, m.font * plan.display.line_radius, chrome::text(alpha));
        }
        for row in &plan.rows {
            paint::paint_row(
                window,
                &row.lines,
                row.bounds.x,
                row.top,
                row.lines.line_height,
                row.lines.font_size,
                &row.lines.words,
                &row.light,
                plan.lyric_position_ms,
                row.scale,
                row.wrapper,
                feather_top,
                lyric_bottom,
                m.feather_px,
                row.bounds.w,
                row.align,
                row.blur,
            );
            if plan.translation {
                let top = row.top
                    + row.lines.lines.len() as f32 * row.lines.line_height
                    + row.lines.trans_gap;
                for (i, trans) in row.lines.trans_lines.iter().enumerate() {
                    let x = row.bounds.x + row.align.offset(row.bounds.w, f32::from(trans.width));
                    paint::paint_translation(
                        window,
                        trans,
                        x,
                        top + i as f32 * row.lines.trans_line_height,
                        row.lines.trans_font,
                        row.translation_alpha,
                        [feather_top, lyric_bottom, m.feather_px],
                        row.scale,
                        row.align.pivot_x(row.bounds.x, row.bounds.w),
                        row.top + row.lines.height * 0.5,
                        row.wrapper,
                        row.blur,
                    );
                }
            }
            let top = row.top
                + row.lines.lines.len() as f32 * row.lines.line_height
                + if row.lines.trans_lines.is_empty() {
                    0.0
                } else {
                    row.lines.trans_gap + row.lines.trans_height()
                };
            for (i, roman) in row.lines.roman_lines.iter().enumerate() {
                let x = row.bounds.x + row.align.offset(row.bounds.w, f32::from(roman.width));
                paint::paint_translation(
                    window,
                    roman,
                    x,
                    top + i as f32 * row.lines.roman_line_height,
                    row.lines.roman_font,
                    row.roman_alpha,
                    [feather_top, lyric_bottom, m.feather_px],
                    row.scale,
                    row.align.pivot_x(row.bounds.x, row.bounds.w),
                    row.top + row.lines.height * 0.5,
                    row.wrapper,
                    row.blur,
                );
            }
        }
        if let Some(inter) = &plan.interlude {
            let d = inter.dot_diameter * inter.scale;
            let g = inter.dot_gap * inter.scale;
            if d > 0.0 && inter.opacity > 0.001 {
                let strip_w = inter.dot_diameter * 3.0 + inter.dot_gap * 2.0;
                let cx = inter.x + strip_w * 0.5;
                let cy = inter.y + inter.dot_diameter * 0.5;
                let start_x = cx - (d * 3.0 + g * 2.0) * 0.5;
                let dy = cy - d * 0.5;
                let feather = paint::feather(cy, feather_top, lyric_bottom, m.feather_px);
                for i in 0..3 {
                    let alpha = (inter.opacity * inter.dot_opacities[i] * feather).clamp(0.0, 1.0);
                    if alpha > 0.001 {
                        let dx = start_x + i as f32 * (d + g);
                        chrome::fill_round_subpixel(window, dx, dy, d, d, d * 0.5, chrome::text(alpha));
                    }
                }
            }
        }
        if let Some(bottom) = &plan.bottom {
            for (i, line) in bottom.lines.lines.iter().enumerate() {
                let x = bottom.x_base + bottom.align.offset(bottom.wrap_width, f32::from(line.width));
                paint::paint_translation(
                    window,
                    line,
                    x,
                    bottom.top + i as f32 * bottom.lines.line_height,
                    bottom.lines.font_size,
                    bottom.alpha,
                    [feather_top, lyric_bottom, m.feather_px],
                    1.0,
                    bottom.align.pivot_x(bottom.x_base, bottom.wrap_width),
                    bottom.top + bottom.lines.height * 0.5,
                    paint::WrapperTransform::default(),
                    bottom.blur,
                );
            }
        }
    });
    } // end lyrics_alpha > 0.005

    // 4. Cover caption, under the artwork.
    //
    //    `.cover` is a 20px-gap flex column and `.cover-meta` is shifted down
    //    another `--wide-controls-drop` (12px), so the title's line box starts
    //    32px under the artwork. Both lines are 22px at `line-height: 1.25`,
    //    the artist 4px under the title and in plain weight. `Cover` returns
    //    `null` only when both `showCover` and `showMeta` are off, so the
    //    caption still paints with the artwork hidden; `metrics` carries the
    //    right top edge for both cases.
        let album_scrim_alpha = (plan.compact_chrome_alpha.max(plan.album_meta_alpha) * cg_alpha).clamp(0.0, 1.0);
        if plan.mode == Mode::Album && album_scrim_alpha > 0.005 {
            let scrim_h = 196.0_f32.min(m.win_h);
            let scrim_top = m.win_h - scrim_h;
            let bottom_r = win_radius.min(scrim_h * 0.5);
            window.paint_quad(gpui_kit::quad(
                gpui_kit::bounds(
                    gpui_kit::point(px(0.0), px(scrim_top)),
                    gpui_kit::size(px(m.win_w), px(scrim_h)),
                ),
                gpui_kit::Corners {
                    top_left: px(0.0),
                    top_right: px(0.0),
                    bottom_left: px(bottom_r),
                    bottom_right: px(bottom_r),
                },
                gpui_kit::linear_gradient(
                    180.0,
                    gpui_kit::linear_color_stop(Hsla::from(rgb(0x00_00_00)).opacity(0.0), 0.0),
                    gpui_kit::linear_color_stop(
                        Hsla::from(rgb(0x00_00_00)).opacity((0.78 * album_scrim_alpha).clamp(0.0, 0.85)),
                        1.0,
                    ),
                ),
                gpui_kit::Edges::default(),
                Hsla::default(),
                gpui_kit::BorderStyle::Solid,
            ));
        }
        let album_meta_a = (plan.album_meta_alpha * cg_alpha).clamp(0.0, 1.0);
        if plan.mode == Mode::Album && m.column > 0.0 && album_meta_a > 0.005 {
            let max_w = (m.win_w - 32.0).max(1.0);
            let title_w = chrome::measure(window, &plan.track_title, 18.0, 650.0).0.min(max_w);
            let title_x = (m.win_w - title_w) * 0.5;
            chrome::paint_text_fitted(window, &plan.track_title, title_x, m.cover_meta_top, max_w, 18.0, 650., chrome::text(0.96 * album_meta_a));
            let artist_w = chrome::measure(window, &plan.track_artist, 13.0, 500.0).0.min(max_w);
            let artist_x = (m.win_w - artist_w) * 0.5;
            chrome::paint_text_fitted(window, &plan.track_artist, artist_x, m.cover_meta_top + 24.0, max_w, 13.0, 500., chrome::text(0.66 * album_meta_a));
        } else if plan.mode != Mode::Album && m.column > 0.0 && cover_alpha > 0.01 && !(plan.mode == Mode::Window && !plan.display.show_cover && cover_vis <= 0.01) {
        let compact=matches!(plan.mode,Mode::Mini|Mode::Album|Mode::Lyrics);
        let ws = if plan.mode == Mode::Window { m.ui_scale } else { 1.0 };
        let x=if compact{m.cover.x+m.art_height+if m.art_height>0.{13.}else{0.}}else{m.cover.x};
        let title_size=if compact{19.}else{22. * ws};let artist_size=if compact{13.5}else{17. * ws};
        let width=if compact{m.win_w-x-16.}else{m.column};
        let title_y = if compact { m.cover_meta_top } else { m.cover_meta_top - 3.0 * ws };
        let artist_y = if compact { m.cover_meta_top + 28.0 } else { m.cover_meta_top + 25.0 * ws };
        let meta_a = if plan.mode == Mode::Window { cover_alpha } else { fg_alpha };
        chrome::paint_text_fitted(window,&plan.track_title,x,title_y,width,title_size,650.,chrome::text(0.96 * meta_a));
        chrome::paint_text_fitted(window,&plan.track_artist,x,artist_y,width,artist_size,if compact{500.}else{420.},chrome::text((if compact{0.60}else{0.66}) * meta_a));
    }

    // 5. Playbar: one 6px rail across the column and the two time labels under
    //    it.
    //
    //    `.window-progress .track .lg-slider` widens the slider box to
    //    `calc(100% + 26px)` with `margin-inline: -13px`, but
    //    `.lg-slider-track` then sets `inset-inline: 13px`: the two cancel, and
    //    the visible rail is exactly the column. The overhang only exists to
    //    give the hit target room, which is why `Hot::Progress` is the wider
    //    box while the paint is not. The tail rule
    //    `.window-progress .time.now, .time.total { margin-inline: 0 }` also
    //    clears the base 13px label margins, so both labels sit flush with the
    //    column ends. The window form shows the time remaining on the right,
    //    with the reference's leading minus.
    if let Some((ir, ia)) = plan.island {
        chrome::set_light_theme(light_panel);
        let ia = (ia * cg_alpha).clamp(0.0, 1.0);
        if ia > 0.005 {
            if let Some(gp) = &plan.island_glass_panel {
                window.paint_custom_surface_with_scene(
                    gpui_kit::Bounds {
                        origin: point(px(gp.rect.left as f32), px(gp.rect.top as f32)),
                        size: gpui_kit::size(px(gp.rect.width as f32), px(gp.rect.height as f32)),
                    },
                    gp.surface,
                    gp.snapshot,
                    gp.hook,
                );
            } else {
                chrome::fill_round(
                    window,
                    ir.x,
                    ir.y,
                    ir.w,
                    ir.h,
                    18.0,
                    chrome::themed_surface(0.86 * ia),
                );
            }
            chrome::stroke_round(
                window,
                ir.x,
                ir.y,
                ir.w,
                ir.h,
                18.0,
                1.0,
                chrome::text(0.16 * ia),
            );
            let has_thumb = plan.cover.is_some();
            if let Some(still) = plan.cover {
                let thumb = gpui_kit::Bounds {
                    origin: point(px(ir.x + 10.0), px(ir.y + 8.0)),
                    size: gpui_kit::size(px(36.0), px(36.0)),
                };
                window.paint_custom_surface_rounded(thumb, px(8.0), still);
                chrome::stroke_round(window, ir.x + 10.0, ir.y + 8.0, 36.0, 36.0, 8.0, 0.75, chrome::text(0.16 * ia));
            }
            let text_x = if has_thumb { ir.x + 54.0 } else { ir.x + 16.0 };
            let meta_w = (ir.right() - 182.0 - text_x).max(60.0);
            chrome::paint_text_fitted(
                window,
                &plan.track_title,
                text_x,
                ir.y + 8.0,
                meta_w,
                13.0,
                600.0,
                chrome::text(0.95 * ia),
            );
            chrome::paint_text_fitted(
                window,
                &plan.track_artist,
                text_x,
                ir.y + 25.0,
                meta_w,
                11.0,
                450.0,
                chrome::text(0.62 * ia),
            );
        }
        chrome::set_light_theme(light_window);
    }
    let compact_cg_alpha = if matches!(plan.mode, Mode::Lyrics | Mode::Album) {
        (cg_alpha * plan.compact_chrome_alpha).clamp(0.0, 1.0)
    } else {
        cg_alpha
    };
    let show_bottom_progress = plan.display.show_progress
        && plan.chrome_visible
        && compact_cg_alpha > 0.005
        && (plan.mode != Mode::Window || cover_vis > 0.01 || plan.island.is_some());
    if show_bottom_progress {
    let progress = plan.chrome.progress;
    let fraction = if plan.duration_ms > 0.0 {
        (plan.position_ms / plan.duration_ms).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let bar_alpha = if let Some((_, ia)) = plan.island {
        (ia * cg_alpha).clamp(0.0, 1.0)
    } else if plan.mode == Mode::Window {
        cover_alpha
    } else {
        compact_cg_alpha
    };
    chrome::progress_rail_interactive_alpha(window, progress, fraction, feedback(Hot::Progress), bar_alpha);
    if plan.island.is_none() {
    // `.window-progress` is a 12px/4px/20px grid: the label row starts 16px
    // below the block, and a 13px line lands its ink 5px under that. The
    // measured reference ink is rows 633..642 at a block top of 611.
    let compact=matches!(plan.mode,Mode::Mini|Mode::Album|Mode::Lyrics);
    let ws = if plan.mode == Mode::Window { m.ui_scale } else { 1.0 };
    let time_size = if plan.mode == Mode::Album { 12.0 } else if compact { 13.0 } else { 14.0 * ws };
    let time_top = if plan.mode == Mode::Album { progress.y + 15.0 } else if compact { progress.y - 2.0 } else { progress.y + 9.0 + 7.0 * ws };
    let time_alpha = if plan.mode == Mode::Album { 0.68 } else { 0.54 } * bar_alpha;
    let elapsed = chrome::clock_text(plan.position_ms);
    let (elapsed_w, _) = chrome::measure(window, &elapsed, time_size, 400.0);
    chrome::paint_text(
        window,
        &elapsed,
        if compact{m.progress.x}else{progress.x},
        time_top,
        time_size,
        400.0,
        chrome::text(time_alpha),
    );
    let remaining = format!(
        "-{}",
        chrome::clock_text((plan.duration_ms - plan.position_ms).max(0.0))
    );
    let (remaining_w, _) = chrome::measure(window, &remaining, time_size, 400.0);
    let avail_w = if compact { m.progress.w } else { progress.w };
    if avail_w >= elapsed_w + remaining_w + 12.0 {
        chrome::paint_text(
            window,
            &remaining,
            if compact{m.progress.right()-remaining_w}else{progress.x+progress.w-remaining_w},
            time_top,
            time_size,
            400.0,
            chrome::text(time_alpha),
        );
    }
    }

    }

    // The scrollbar stays a thin hairline at rest and thickens into a liquid capsule while held.
    if cg_alpha > 0.005 {
        if let Some(panel)=&plan.scroll_glass {
            window.paint_custom_surface_with_scene(gpui_kit::Bounds{origin:point(px(panel.rect.left as f32),px(panel.rect.top as f32)),size:gpui_kit::size(px(panel.rect.width as f32),px(panel.rect.height as f32))},panel.surface,panel.snapshot,panel.hook);
        }
        if let Some((line, alpha)) = plan.scroll_line {
            chrome::fill_round(
                window,
                line.x,
                line.y,
                line.w,
                line.h,
                line.w * 0.5,
                chrome::text(alpha * cg_alpha),
            );
        }
    }

    // 6. Transport: clean icons at rest, subtle hover hop without a circle,
    //    lively toggle bounce on state change, and a clear refractive liquid lens while held/dragged.
    let paint_btn_lens_alpha = |window: &mut Window, target: Hot, alpha: f32| {
        if let Some((hot, panel, lens_box, bf)) = &plan.button_lens {
            if *hot == target {
                window.paint_custom_surface_with_scene(
                    gpui_kit::Bounds {
                        origin: point(px(panel.rect.left as f32), px(panel.rect.top as f32)),
                        size: gpui_kit::size(px(panel.rect.width as f32), px(panel.rect.height as f32)),
                    },
                    panel.surface,
                    panel.snapshot,
                    panel.hook,
                );
                let radius = lens_box.w.min(lens_box.h) * 0.5;
                let swell = if target == Hot::WindowClose {
                    (bf.hover * 0.45 + bf.press * 0.48).clamp(0.0, 1.0)
                } else {
                    (bf.press * 0.85).clamp(0.0, 1.0)
                };
                chrome::paint_segmented_lens(window, *lens_box, radius, swell, *bf, alpha);
            }
        }
    };
    let btn_alpha = if plan.mode == Mode::Window {
        if let Some((_, ia)) = plan.island {
            (ia * cg_alpha).clamp(0.0, 1.0)
        } else {
            cover_alpha
        }
    } else {
        compact_cg_alpha
    };
    if plan.chrome_visible && btn_alpha > 0.005 {
    let play = plan.chrome.play;
    let (pcx, pcy) = play.centre();
    let play_feedback = feedback(Hot::Play);
    paint_btn_lens_alpha(window, Hot::Play, btn_alpha);
    icons::paint_toggle(
        window,
        cx,
        Icon::Play,
        Icon::Pause,
        {let d=if plan.island.is_some(){22.}else{match plan.mode{Mode::Mini=>28.,Mode::Album=>32.,Mode::Lyrics=>34.,_=>36. * plan.metrics.ui_scale}};Box4::new(pcx-d/2.,pcy-d/2.,d,d)},
        chrome::text(0.98 * btn_alpha),
        play_feedback,
        plan.play_morph,
    );

    let prev = plan.chrome.prev;
    let prev_feedback = feedback(Hot::Prev);
    paint_btn_lens_alpha(window, Hot::Prev, btn_alpha);
    let (x, y) = prev.centre();
    icons::paint_elastic(
        window,
        cx,
        Icon::Prev,
        {let d=if plan.island.is_some(){18.}else{match plan.mode{Mode::Mini=>22.,Mode::Album=>24.,Mode::Lyrics=>26.,_=>28. * plan.metrics.ui_scale}};Box4::new(x-d/2.,y-d/2.,d,d)},
        chrome::text(0.90 * btn_alpha),
        prev_feedback,
    );

    if plan.control_pending {let p=plan.chrome.play;chrome::stroke_round(window,p.x+1.0,p.y+1.0,p.w-2.0,p.h-2.0,(p.w-2.0)*0.5,1.0,chrome::text(0.35 * btn_alpha));}
    let next = plan.chrome.next;
    let next_feedback = feedback(Hot::Next);
    paint_btn_lens_alpha(window, Hot::Next, btn_alpha);
    let (x, y) = next.centre();
    icons::paint_elastic(
        window,
        cx,
        Icon::Next,
        {let d=if plan.island.is_some(){18.}else{match plan.mode{Mode::Mini=>22.,Mode::Album=>24.,Mode::Lyrics=>26.,_=>28. * plan.metrics.ui_scale}};Box4::new(x-d/2.,y-d/2.,d,d)},
        chrome::text(0.90 * btn_alpha),
        next_feedback,
    );

    let more = plan.chrome.more;
    let aux_d = if plan.island.is_some() { 18.0 } else if plan.mode == Mode::Mini { 21.0 } else if plan.mode == Mode::Window { 24.0 * plan.metrics.ui_scale } else { 24.0 };
    if more.w > 0.0 && more.h > 0.0 {
        let (mx, my) = more.centre();
        let more_feedback = feedback(Hot::More);
        paint_btn_lens_alpha(window, Hot::More, btn_alpha);
        icons::paint_elastic(
            window,
            cx,
            Icon::More,
            Box4::new(mx - aux_d * 0.5, my - aux_d * 0.5, aux_d, aux_d),
            chrome::text((0.68 + 0.27 * more_feedback.hover) * btn_alpha),
            more_feedback,
        );
    }

    if plan.mode!=Mode::Mini {
    let volume = plan.chrome.volume;
    let (vx, vy) = volume.centre();
    let volume_feedback = feedback(Hot::Volume);
    paint_btn_lens_alpha(window, Hot::Volume, btn_alpha);
    icons::paint_elastic(
        window,
        cx,
        Icon::Volume,
        Box4::new(vx - aux_d * 0.5, vy - aux_d * 0.5, aux_d, aux_d),
        chrome::text((0.68 + 0.27 * volume_feedback.hover) * (if plan.volume_available { 1.0 } else { 0.45 }) * btn_alpha),
        volume_feedback,
    );

    let lyrics_btn = plan.chrome.lyrics_toggle;
    if lyrics_btn.w > 0.0 && lyrics_btn.h > 0.0 {
        let (lx, ly) = lyrics_btn.centre();
        let lf = feedback(Hot::LyricsToggle);
        paint_btn_lens_alpha(window, Hot::LyricsToggle, btn_alpha);
        let icon_color = if plan.lyrics_visible {
            chrome::text((0.95 + 0.05 * lf.hover) * btn_alpha)
        } else {
            chrome::text((0.38 + 0.22 * lf.hover) * btn_alpha)
        };
        icons::paint_elastic(
            window,
            cx,
            Icon::Lyrics,
            Box4::new(lx - aux_d * 0.5, ly - aux_d * 0.5, aux_d, aux_d),
            icon_color,
            lf,
        );
    }

    // 7. Replay badge: the experiment never pretends the buttons reach a real
    //    player.
    }
    }
    let fold_fb = feedback(Hot::Fold);
    let fold_shown = !plan.settings_only
        && cg_alpha > 0.005
        && matches!(plan.mode, Mode::Mini | Mode::Lyrics)
        && (super::details::fold_visible(plan.cursor, m.win_w, m.win_h)
            || fold_fb.hover > 0.01
            || fold_fb.press > 0.01);
    if fold_shown {
        let swell = fold_fb.hover.clamp(0.0, 1.0);
        let press = fold_fb.press.clamp(0.0, 1.0);
        if swell > 0.01 || press > 0.01 {
            chrome::fill_round_subpixel(
                window,
                m.win_w * 0.5 - 36.0,
                m.win_h - 14.0,
                72.0,
                13.0,
                6.5,
                chrome::text(((0.08 * swell + 0.06 * press) * cg_alpha).clamp(0.0, 0.2)),
            );
        }
        let lw = ((32.0 + 12.0 * swell - 3.0 * press) * (1.0 + fold_fb.squeeze[0] * 0.35)).max(24.0);
        let lh = ((3.0 + 2.0 * swell - 0.3 * press) * (1.0 + fold_fb.squeeze[1] * 0.35)).max(2.5);
        let lx = m.win_w * 0.5 - lw * 0.5 + fold_fb.pull[0] * 0.35;
        let ly = m.win_h - 7.5 - lh * 0.5 + fold_fb.pull[1] * 0.18;
        chrome::fill_round_subpixel(
            window,
            lx,
            ly,
            lw,
            lh,
            lh * 0.5,
            chrome::text(((0.52 + 0.40 * swell + 0.06 * press) * cg_alpha).clamp(0.0, 0.98)),
        );
    }
    let compact = matches!(plan.mode, Mode::Mini | Mode::Album | Mode::Lyrics);
    if !plan.settings_only && plan.window_controls_alpha > 0.01 {
        let wca = (plan.window_controls_alpha * fg_alpha).clamp(0.0, 1.0);
        let (_, [cap_min, cap_fs, cap_close]) = window_caption_rects(m.win_w, compact);
        let icon_d = if compact { 11.5 } else { 14.0 };
        let mut items = vec![
            (Hot::WindowMinimize, cap_min, Icon::Minimize),
            (
                Hot::WindowFullscreen,
                cap_fs,
                if plan.fullscreen {
                    Icon::FullscreenExit
                } else {
                    Icon::Fullscreen
                },
            ),
            (Hot::WindowClose, cap_close, Icon::Close),
        ];
        if plan.mode == Mode::Album {
            items.push((Hot::AlbumReturn, album_return_rect(), Icon::CoverCollapse));
        }
        for (hot_key, box_r, icon) in items {
            let fb = feedback(hot_key);
            paint_btn_lens_alpha(window, hot_key, wca);
            let (bcx, bcy) = box_r.centre();
            icons::paint_elastic(
                window,
                cx,
                icon,
                Box4::new(bcx - icon_d * 0.5, bcy - icon_d * 0.5, icon_d, icon_d),
                chrome::text((0.78 + 0.22 * fb.hover) * wca),
                fb,
            );
        }
    }
    if let Some(badge) = plan.badge.as_ref() {
        let (w, _) = chrome::measure(window, badge, 11.0, 600.0);
        let shift = if matches!(plan.mode, Mode::Mini | Mode::Lyrics) {
            84.0 * plan.window_controls_alpha
        } else {
            110.0 * plan.window_controls_alpha
        };
        let bx = m.win_w - w - 24.0 - shift;
        chrome::fill_round(window, bx - 8.0, 14.0, w + 16.0, 20.0, 6.0, chrome::text(0.10 * fg_alpha));
        chrome::paint_text(window, badge, bx, 18.0, 11.0, 600.0, chrome::accent_rgb(plan.display.accent_hex, 0.9 * fg_alpha));
    }

    } // end fg_alpha > 0.005
    }
    // 8. Panels last: each one samples the frame behind it in the same pass.
    chrome::set_light_theme(light_panel);
    for panel in &plan.panels {
        let bounds = gpui_kit::Bounds {
            origin: point(px(panel.rect.left as f32), px(panel.rect.top as f32)),
            size: gpui_kit::size(px(panel.rect.width as f32), px(panel.rect.height as f32)),
        };
        if !plan.settings_only && plan.immersion {chrome::elevation(window, Box4::new(panel.rect.left as f32, panel.rect.top as f32, panel.rect.width as f32, panel.rect.height as f32), panel_radius, panel.surface_alpha);}
        // Sidecar windows rely on DWM directly; main-window popovers run the
        // alpha-preserving glass shader so desktop areas stay translucent while
        // underlying cover/lyrics still receive frosted Gaussian blur.
        if plan.settings_only && matches!(panel.kind, PanelKind::Settings | PanelKind::Dropdown) {
            let r = Box4::new(
                panel.rect.left as f32,
                panel.rect.top as f32,
                panel.rect.width as f32,
                panel.rect.height as f32,
            );
            let radius = panel_radius;
            let sidecar_alpha = match plan.display.theme_mode {
                model::ThemeMode::Light => 0.88,
                model::ThemeMode::Dark => 0.84,
                model::ThemeMode::LiquidGlass => 0.29,
            };
            chrome::fill_round(
                window,
                r.x,
                r.y,
                r.w,
                r.h,
                radius,
                chrome::themed_surface(sidecar_alpha * panel.surface_alpha),
            );
            chrome::stroke_round(
                window,
                r.x,
                r.y,
                r.w,
                r.h,
                radius,
                1.0,
                chrome::themed_border(0.13 * panel.surface_alpha),
            );
        } else {
            window.paint_custom_surface_with_scene(bounds, panel.surface, panel.snapshot, panel.hook);
        }
        let text = |alpha: f32| chrome::text(alpha*panel.content_alpha);
        let accent = |alpha: f32| chrome::accent_rgb(plan.display.accent_hex, alpha*panel.content_alpha);
        match panel.kind {
            PanelKind::Thumb | PanelKind::SliderLens | PanelKind::ButtonLens | PanelKind::SegmentedLens | PanelKind::TabLens | PanelKind::Dropdown => {},
            PanelKind::Settings => {
                if let Some(s) = &plan.settings_paint {
                    settings::paint(
                        window,
                        cx,
                        s,
                        panel.content_alpha,
                        |i| feedback(Hot::SettingSlider(i)),
                        |i| feedback(Hot::SettingChoice(i)),
                        |i| feedback(Hot::SettingsPage(i)),
                        |i| feedback(Hot::DropdownOption(i)),
                        feedback(Hot::SettingsClose),
                        [feedback(Hot::ModalCancel), feedback(Hot::ModalSave)],
                    );
                }
            }
            PanelKind::More => {
                let x = panel.rect.left as f32;
                let top = panel.rect.top as f32;
                let r=Box4::new(x,top,panel.rect.width as f32,panel.rect.height as f32);
                let swell = plan.mode_swell.max(0.0);
                chrome::paint_segmented_well(
                    window,
                    plan.mode_well_rect,
                    plan.mode_well_rect.h * 0.5,
                    panel.content_alpha,
                );
                let active_mode = plan.active_mode;
                let active_fb = feedback(Hot::MenuMode(active_mode));
                let lens_box = Box4::new(
                    plan.mode_lens_rect.x + active_fb.pull[0] * 0.35,
                    plan.mode_lens_rect.y + active_fb.pull[1] * 0.25,
                    plan.mode_lens_rect.w,
                    plan.mode_lens_rect.h,
                );
                let lens_radius = lens_box.h * 0.5;
                let (rest_alpha, _) = chrome::segmented_crossfade(swell);
                chrome::paint_segmented_rest_pill(
                    window,
                    lens_box,
                    lens_radius,
                    plan.display.accent_hex,
                    rest_alpha,
                    panel.content_alpha,
                );
                let mode_font_sz = 12.0 * (1.0 + 0.10 * swell);
                let base_track_cx = r.x + r.w * 0.5;
                let spread = if r.w > 20.0 {
                    plan.mode_well_rect.w / (r.w - 16.0)
                } else {
                    1.0
                };
                let scaled_mode_slot = |i: u8| {
                    let b_base = input::mode_rect(r, i);
                    let cx = base_track_cx + (b_base.centre().0 - base_track_cx) * spread;
                    Box4::new(cx - b_base.w * 0.5, b_base.y, b_base.w, b_base.h)
                };
                // 1. Paint labels over the soft rest pill; the selected label carries the active accent.
                for i in 0..4u8 {
                    let b = scaled_mode_slot(i);
                    let selected = i == active_mode;
                    let mf = feedback(Hot::MenuMode(i));
                    if !selected && (mf.hover > 0.01 || mf.press > 0.01) && swell < 0.1 {
                        chrome::control_surface_alpha(window, b, mf, false, panel.content_alpha * 0.45);
                    }
                    chrome::paint_text_box_centred(
                        window,
                        input::MODE_LABELS[i as usize],
                        b,
                        mode_font_sz,
                        if selected { 650.0 } else { 550.0 },
                        if selected {
                            chrome::segmented_active_text_blended(plan.display.accent_hex, rest_alpha, 0.96 * panel.content_alpha)
                        } else {
                            text(0.62 + 0.22 * mf.hover)
                        },
                    );
                }
                // 2. While dragging/sliding, composite the transparent liquid droplet with edge blur & refraction.
                if swell > 0.005 {
                    if let Some(lens_glass) = &plan.mode_lens_glass {
                        window.paint_custom_surface_with_scene(
                            gpui_kit::Bounds {
                                origin: point(px(lens_glass.rect.left as f32), px(lens_glass.rect.top as f32)),
                                size: gpui_kit::size(px(lens_glass.rect.width as f32), px(lens_glass.rect.height as f32)),
                            },
                            lens_glass.surface,
                            lens_glass.snapshot,
                            lens_glass.hook,
                        );
                    }
                    chrome::paint_segmented_lens(
                        window,
                        lens_box,
                        lens_radius,
                        plan.mode_swell,
                        active_fb,
                        panel.content_alpha,
                    );
                }
                let row_count = if plan.show_media_row { 5 } else { 4 };
                for i in 0..row_count {
                    let item = if plan.show_media_row {
                        match i {
                            0 => more_items[0],
                            1 => more_items[1],
                            2 => more_items[2],
                            3 => input::MORE_MEDIA_LABEL,
                            _ => more_items[3],
                        }
                    } else {
                        more_items[i]
                    };
                    let b = input::row_rect_count(r, i as u8, row_count);
                    let f = feedback(Hot::MoreRow(i as u8));
                    if i == 0 || i == row_count - 1 {
                        chrome::fill_round(
                            window,
                            r.x + 12.0,
                            b.y - 3.0,
                            (r.w - 24.0).max(20.0),
                            0.75,
                            0.35,
                            text(0.08),
                        );
                    }
                    if i > 0 {
                        chrome::control_surface_alpha_radius(window, b, f, false, panel.content_alpha, 13.5);
                        chrome::chevron(window, b.right() - 16.0, b.y + (b.h - 8.0) * 0.5, text(0.36 + 0.30 * f.hover));
                    }
                    let icon = if plan.show_media_row {
                        match i {
                            0 => Icon::Sync,
                            1 => Icon::List,
                            2 => Icon::MotionCover,
                            3 => Icon::Play,
                            _ => Icon::Settings,
                        }
                    } else {
                        match i {
                            0 => Icon::Sync,
                            1 => Icon::List,
                            2 => Icon::MotionCover,
                            _ => Icon::Settings,
                        }
                    };
                    icons::paint(window,cx,icon,Box4::new(x+14.0,b.y+(b.h-16.0)*0.5,16.0,16.0),text(0.55 + 0.25 * f.hover));
                    chrome::paint_text_vcentred(window,item,x+38.0,b.centre().1,12.0,500.0,text(0.88 + 0.08 * f.hover));
                    if i == 0 {
                        let minus_b = input::sync_rect_count(r, false, row_count);
                        let plus_b = input::sync_rect_count(r, true, row_count);
                        chrome::paint_stepper(
                            window,
                            minus_b,
                            plus_b,
                            feedback(Hot::SyncMinus),
                            feedback(Hot::SyncPlus),
                            &plan.sync_label,
                            plan.sync_enabled,
                            panel.content_alpha,
                        );
                    }
                }
            }
            PanelKind::Volume => {
                let x = panel.rect.left as f32 + 14.0;
                let right_x = panel.rect.left as f32 + panel.rect.width as f32 - 14.0;
                let top = panel.rect.top as f32 + 12.0;
                chrome::paint_text(window, "音量", x, top, 13.0, 600.0, text(0.9));
                let status_str = plan.status.as_str();
                let status_w = chrome::measure(window, status_str, 12.0, 600.0).0.min(96.0);
                chrome::paint_text_fitted(
                    window,
                    status_str,
                    (right_x - status_w).max(x + 48.0),
                    top,
                    96.0,
                    12.0,
                    600.0,
                    text(0.72),
                );
                let f = (volume_value / 100.0).clamp(0.0, 1.0) as f32;
                chrome::paint_slider(
                    window,
                    x,
                    panel.rect.top as f32 + 62.0,
                    panel.rect.width as f32 - 28.0,
                    f,
                    if plan.volume_enabled { accent(0.95) } else { text(0.35) },
                    text(0.18),
                    if plan.volume_enabled {feedback(Hot::VolumeTrack)} else {Feedback::default()},
                    panel.content_alpha,
                );
                chrome::paint_text(window, plan.volume_label.as_str(), x, top+23.0, 11.0, 400.0, text(0.50));
                let vf = feedback(Hot::VolumeTrack);
                if plan.volume_enabled && vf.hover > 0.0 {
                    chrome::stroke_round(window,x+(panel.rect.width as f32-28.0)*f-11.0,panel.rect.top as f32+51.0,22.0,22.0,11.0,1.0,accent(vf.hover*0.55));
                }
            }
            PanelKind::Picker => {
                let x=panel.rect.left as f32+14.;let top=panel.rect.top as f32;
                let picker_title = match plan.picker_kind {
                    PickerKind::Media => "媒体",
                    PickerKind::Cover => "封面",
                    PickerKind::Lyrics => "歌词版本",
                };
                chrome::paint_text(window,picker_title,x,top+12.,13.5,650.,text(0.95));
                if plan.picker_kind == PickerKind::Cover {
                    let cover_rows = [
                        ("显示封面", Icon::CoverExpand, plan.display.show_cover, plan.cover_switch_fractions[0]),
                        ("动态封面", Icon::MotionCover, plan.display.motion_cover, plan.cover_switch_fractions[1]),
                    ];
                    for (idx, (label, icon, on, frac)) in cover_rows.into_iter().enumerate() {
                        let r = cover_switch_row_rect(
                            panel.rect.left as f32,
                            panel.rect.top as f32,
                            panel.rect.width as f32,
                            idx as u8,
                        );
                        let f = feedback(Hot::CoverSwitch(idx as u8));
                        if f.hover > 0.01 {
                            chrome::fill_round(window, r.x, r.y, r.w, r.h, 10.0, text(0.045 * f.hover));
                        }
                        icons::paint(
                            window,
                            cx,
                            icon,
                            Box4::new(r.x + 8.0, r.y + (r.h - 16.0) * 0.5, 16.0, 16.0),
                            text(if on { 0.78 + 0.16 * f.hover } else { 0.50 + 0.24 * f.hover }),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            label,
                            r.x + 32.0,
                            r.centre().1,
                            12.5,
                            500.0,
                            text(if on { 0.94 } else { 0.78 + 0.14 * f.hover }),
                        );
                        let track = cover_switch_track_rect(r);
                        chrome::paint_switch_with_accent(
                            window,
                            track.x,
                            track.y,
                            track.w,
                            track.h,
                            on,
                            frac,
                            f,
                            plan.display.accent_hex,
                            panel.content_alpha,
                        );
                    }
                } else {
                if plan.picker_kind == PickerKind::Lyrics {
                let import_box = picker_import_rect(panel.rect);
                let imf = feedback(Hot::PickerImport);
                if imf.hover > 0.01 || imf.press > 0.01 {
                    let (icx, icy) = import_box.centre();
                    chrome::fill_round(
                        window,
                        icx - 12.0,
                        icy - 12.0,
                        24.0,
                        24.0,
                        12.0,
                        text(0.08 * imf.hover + 0.06 * imf.press),
                    );
                }
                let (icx, icy) = import_box.centre();
                icons::paint_elastic(
                    window,
                    cx,
                    Icon::Plus,
                    Box4::new(icx - 8.0, icy - 8.0, 16.0, 16.0),
                    text(0.72 + 0.24 * imf.hover),
                    imf,
                );
                let refresh_box = picker_refresh_rect(panel.rect);
                let rf = feedback(Hot::PickerRefresh);
                if rf.hover > 0.01 || rf.press > 0.01 {
                    let (rcx, rcy) = refresh_box.centre();
                    chrome::fill_round(
                        window,
                        rcx - 12.0,
                        rcy - 12.0,
                        24.0,
                        24.0,
                        12.0,
                        text(0.08 * rf.hover + 0.06 * rf.press),
                    );
                }
                let (rcx, rcy) = refresh_box.centre();
                icons::paint_elastic_rotated(
                    window,
                    cx,
                    Icon::Refresh,
                    Box4::new(rcx - 8.0, rcy - 8.0, 16.0, 16.0),
                    text(0.72 + 0.24 * rf.hover),
                    rf,
                    plan.picker_refresh_angle,
                );
                }
                let capacity=(((panel.rect.height as f32-VERSION_HEADER)/VERSION_ROW).floor() as usize).min(4);
                let paint_badge = |window: &mut Window, bx: f32, cy: f32, label: &str, bg: Hsla, border: Hsla, fg: Hsla| -> f32 {
                    let tw = chrome::measure(window, label, 10.5, 600.).0;
                    let bw = tw + 12.;
                    let by = cy - 9.0;
                    chrome::fill_round(window, bx, by, bw, 18., 5.5, bg);
                    chrome::stroke_round(window, bx, by, bw, 18., 5.5, 0.85, border);
                    chrome::paint_text_vcentred(window, label, bx + 6., cy, 10.5, 600., fg);
                    bw + 5.
                };
                for (i,entry) in candidates.iter().enumerate().skip(plan.picker_start).take(capacity) {
                    let y=top+VERSION_HEADER+(i-plan.picker_start) as f32*VERSION_ROW;
                    let r=Box4::new(panel.rect.left as f32+8.,y,panel.rect.width as f32-16.,VERSION_ROW-4.);
                    let f=feedback(Hot::Candidate(i));
                if entry.selected {
                    chrome::fill_round(window, r.x, r.y, r.w, r.h, 10.0, accent(0.11));
                    chrome::stroke_round(window, r.x, r.y, r.w, r.h, 10.0, 1.25, accent(0.86));
                } else if f.hover > 0.01 {
                    chrome::fill_round(window, r.x, r.y, r.w, r.h, 10.0, text(0.045 * f.hover));
                }
                let ry = y;
                let top_cy = ry + 16.5;
                let title_alpha = if entry.selected { 0.98 } else { 0.88 + 0.12 * f.hover };
                let sub_alpha = 0.54 + 0.18 * f.hover;
                let has_smart_btn = !entry.has_translation && !entry.smart_translation_key.is_empty();
                let check_w = if entry.selected { 22.0 } else { 0.0 };
                let match_w = if entry.match_badge.is_empty() {
                    0.0
                } else {
                    chrome::measure(window, &entry.match_badge, 11.5, 550.0).0 + 6.0
                };
                let top_right_reserve = check_w + match_w + 8.0;
                let bot_right_reserve = if has_smart_btn { 104. } else { 8. };
                let row_mask=ContentMask{bounds:gpui_kit::bounds(point(px(r.x+3.),px(r.y+3.)),size(px((r.w-6.).max(20.)),px(r.h-6.)))};
                window.with_content_mask(Some(row_mask),|window|{
                    let tx = x + 3.0;
                    let title_max = (r.right() - top_right_reserve - tx).max(40.0);
                    let title_w = chrome::measure(window, &entry.title, 13.5, 650.).0.min(title_max);
                    chrome::paint_text_fitted_vcentred(window, &entry.title, tx, top_cy, title_max, 13.5, 650., text(title_alpha));
                    let mut bx = tx + title_w + 8.;
                    if entry.source == "custom" {
                        bx += paint_badge(window, bx, top_cy, "本地", accent(0.18), accent(0.45), text(0.92));
                    }
                    if let Some(badge) = entry.timing_badge {
                        let (bg, border, fg) = if badge == "逐字" {
                            (accent(0.24), accent(0.58), text(0.96))
                        } else if badge == "逐行" {
                            (text(0.12), text(0.26), text(0.86))
                        } else {
                            (text(0.08), text(0.18), text(0.66))
                        };
                        bx += paint_badge(window, bx, top_cy, badge, bg, border, fg);
                    }
                    if entry.has_translation {
                        let green = Hsla::from(rgb(0x30D158)).opacity((if light_panel { 0.14 } else { 0.20 }) * panel.content_alpha);
                        let green_border = Hsla::from(rgb(0x30D158)).opacity((if light_panel { 0.38 } else { 0.52 }) * panel.content_alpha);
                        let green_text = Hsla::from(rgb(if light_panel { 0x15692C } else { 0x7CE89A })).opacity(0.94 * panel.content_alpha);
                        paint_badge(window, bx, top_cy, "翻译", green, green_border, green_text);
                    }
                    let sub_max_w = (r.right() - bot_right_reserve - tx - 4.).max(40.);
                    chrome::paint_text_fitted_vcentred(window,&entry.subtitle,tx,ry+40.0,sub_max_w,11.,450.,text(sub_alpha));
                });
                if !entry.match_badge.is_empty() {
                    let mx = r.right() - 8.0 - check_w - (match_w - 6.0);
                    chrome::paint_text_vcentred(window, &entry.match_badge, mx, top_cy, 11.5, 550., text(0.58));
                }
                if entry.selected {
                    let cx_chk = r.right() - 22.0;
                    let cy_chk = top_cy - 8.0;
                    chrome::fill_round(window, cx_chk, cy_chk, 16.0, 16.0, 8.0, accent(0.95));
                    chrome::paint_text_box_centred(window, "✓", Box4::new(cx_chk, cy_chk, 16.0, 16.0), 11.0, 700., Hsla::from(rgb(0xFFFFFF)).opacity(0.98 * panel.content_alpha));
                }
                    if has_smart_btn {
                        let btn = picker_smart_btn_rect(r);
                        let sf = feedback(Hot::SmartTranslation(i));
                        let on = entry.smart_translation;
                        let frac = plan
                            .smart_switch_fractions
                            .iter()
                            .find(|(k, _)| k == &entry.smart_translation_key)
                            .map(|(_, v)| *v)
                            .unwrap_or(if on { 1.0 } else { 0.0 });
                        let label_alpha = if on { 0.88 } else { 0.56 + 0.20 * sf.hover };
                        chrome::paint_text(window, "智能翻译", btn.x, btn.y + 3.5, 11.0, 500., text(label_alpha));
                        chrome::paint_switch_with_accent(
                            window,
                            btn.right() - 34.0,
                            btn.y + 1.5,
                            34.0,
                            19.0,
                            on,
                            frac,
                            sf,
                            plan.display.accent_hex,
                            panel.content_alpha,
                        );
                    }
                }
                if capacity > 0 && candidates.len() > capacity {
                    let track_x = panel.rect.left as f32 + panel.rect.width as f32 - 6.0;
                    let track_y = top + VERSION_HEADER + 2.0;
                    let track_h = (capacity as f32 * VERSION_ROW - 8.0).max(16.0);
                    let thumb_h = ((capacity as f32 / candidates.len() as f32) * track_h).max(18.0);
                    let max_start = candidates.len().saturating_sub(capacity).max(1) as f32;
                    let frac = (plan.picker_start as f32 / max_start).clamp(0.0, 1.0);
                    let thumb_y = track_y + (track_h - thumb_h) * frac;
                    chrome::fill_round(window, track_x, track_y, 3.0, track_h, 1.5, text(0.08));
                    chrome::fill_round(window, track_x, thumb_y, 3.0, thumb_h, 1.5, text(0.32));
                }
                if capacity==0 && !candidates.is_empty(){chrome::paint_text(window,"当前空间不足，请调大窗口或切换模式",x,top+VERSION_HEADER+5.,11.,500.,text(0.55));}
                if candidates.is_empty(){
                    let empty_msg = if plan.track_title == "未在播放" { "未在播放" } else { "暂无匹配歌词" };
                    chrome::paint_text(window,empty_msg,x,top+VERSION_HEADER+12.,12.,500.,text(0.55));
                }
                }

            }
            PanelKind::Island => {}
        }
    }
    if let Some(lens) = &plan.slider_lens {
        window.paint_custom_surface_with_scene(
            gpui_kit::Bounds {
                origin: point(px(lens.rect.left as f32), px(lens.rect.top as f32)),
                size: gpui_kit::size(px(lens.rect.width as f32), px(lens.rect.height as f32)),
            },
            lens.surface,
            lens.snapshot,
            lens.hook,
        );
    }
    if !plan.settings_only && !plan.fullscreen && win_radius > 0.5 {
        chrome::stroke_round(
            window,
            0.0,
            0.0,
            m.win_w.max(1.0),
            m.win_h.max(1.0),
            win_radius,
            1.0,
            chrome::text(0.14),
        );
    }

    // 9. Cursor: the pointer is drawn by the OS; this only feeds hover.
    let _ = plan.cursor;
    let _ = plan.drag_thumb;
    let _ = panel_anchor_hint(plan.chrome.more);
    chrome::set_light_theme(false);
}

fn panel_anchor_hint(r: Box4) -> f32 {
    r.y
}

#[cfg(test)]
mod liquid_hit_tests {
    use super::*;
    #[::std::prelude::v1::test]
    fn panel_padding_blocks_controls_behind_it() {
        let panel=Box4::new(0.0,0.0,100.0,100.0);
        let hot=[(Hot::Play,panel),(Hot::MoreRow(0),Box4::new(10.0,10.0,80.0,30.0))];
        assert_eq!(pointer_hit(&hot,&[panel],4.0,4.0),None);
        assert_eq!(pointer_hit(&hot,&[panel],20.0,20.0),Some(Hot::MoreRow(0)));
        assert_eq!(pointer_hit(&hot,&[],4.0,4.0),Some(Hot::Play));
    }
}

fn offset_label(ms: i64) -> String {
    if ms > 0 {
        format!("+{ms}ms")
    } else {
        format!("{ms}ms")
    }
}

fn any_lyric_source_enabled(config: &serde_json::Value) -> bool {
    config
        .pointer("/lyrics/enabledSources")
        .and_then(serde_json::Value::as_object)
        .is_none_or(|sources| sources.values().any(|v| v.as_bool().unwrap_or(true)))
}

#[cfg(test)]mod input_timeline_tests {
 use super::*;
 #[::std::prelude::v1::test]fn fixture_source_survives_the_render_document_projection(){let path=std::env::temp_dir().join(format!("oly-fixture-test-{}.json",std::process::id()));std::fs::write(&path,r#"{"doc":{"provider":"kugou","lines":[{"text":"Sample line","start":1000,"end":3000}]}}"#).unwrap();let(doc,provider)=load_fixture(&path).unwrap();let _=std::fs::remove_file(&path);assert_eq!(provider,"kugou");assert!(!doc.lines.is_empty());}
 #[::std::prelude::v1::test]fn sync_labels_follow_the_original_direction(){assert_eq!(offset_label(0),"0ms");assert_eq!(offset_label(200),"+200ms");assert_eq!(offset_label(-200),"-200ms");}
}

#[cfg(test)] mod compact_regression_tests {
 use super::*;
 #[::std::prelude::v1::test] fn a_hidden_lyric_column_has_a_bounded_scroll_thumb() {
 let mut m=model::metrics(380.,176.,(1920.,1080.),false,&Default::default());
 m.lyrics=Box4::new(20.,190.,340.,1.);
 let chrome=Chrome::build(&m,(0.,0.,0.));
 assert!(chrome.scroll_thumb.h>0. && chrome.scroll_thumb.h<=chrome.scroll_track.h);
 }
 #[::std::prelude::v1::test] fn scroll_thumb_does_not_span_full_track_and_fold_area_stays_out_of_drag_regions() {
 let m=model::compact_metrics(380.,650.,true,&Default::default());
 let chrome=Chrome::build(&m,(0.,0.,10.));
 assert!(chrome.scroll_thumb.h<=chrome.scroll_track.h*0.66);
 let fb=super::super::details::fold_hit_box(m.win_w,m.win_h);
 let rs=input::drag_regions(&m,[fb]);
 assert!(!rs.iter().any(|r|r.contains(m.win_w*0.5,m.win_h-6.)));
 }
 #[::std::prelude::v1::test] fn settings_general_page_keeps_accent_index_and_wires_motion_cover() {
  let general = settings::items(4);
  assert_eq!(general.get(4).map(|i| i.key), Some("uiAccentColor"));
  let mut app=Settings::default();app.set_kind(settings::SettingsKind::Application);app.page=0;
  assert!(app.items().iter().any(|i| i.key == "motionCover"));
  assert!(app.items().iter().any(|i| i.key == "motionCoverCacheMB"));
  let cfg = serde_json::json!({"display": {"motionCover": false, "motionCoverCacheMB": 1200}});
  let d = Display::from_config(&cfg);
  assert!(!d.motion_cover);
  assert_eq!(d.motion_cache_mb, 1200);
 }
}

#[cfg(test)]
mod scenario_clock_tests {
    use super::*;
    #[::std::prelude::v1::test]
    fn slow_startup_preserves_a_painted_gap_before_a_menu_hit() {
        let mut s=Scenario{steps:vec![(100.,Act::OpenMore),(300.,Act::MoreRow(1)),(850.,Act::SettingsShot(false))],..Default::default()};
        assert_eq!(s.take_due(1200.),Some(Act::OpenMore));
        assert_eq!(s.take_due(1200.),None);
        assert_eq!(s.take_due(1399.),None);
        assert_eq!(s.take_due(1400.),Some(Act::MoreRow(1)));
        assert_eq!(s.take_due(1949.),None);
        assert_eq!(s.take_due(1950.),Some(Act::SettingsShot(false)));
        assert_eq!(s.elapsed(5100.),4000.);
    }
}

#[cfg(test)]
mod window_lyrics_toggle_tests {
    use super::*;

    #[::std::prelude::v1::test]
    fn window_toolbar_orders_controls_left_to_right() {
        let m = model::metrics(1272.0, 843.0, (1920.0, 1080.0), false, &Default::default());
        let c6 = Chrome::build(&m, (0.0, 0.0, 0.0));
        assert!(c6.volume.x < c6.prev.x);
        assert!(c6.prev.x < c6.play.x);
        assert!(c6.play.x < c6.next.x);
        assert!(c6.next.x < c6.more.x);
        assert!(c6.more.x < c6.lyrics_toggle.x);
        assert!((c6.play.centre().0 - m.cover.centre().0).abs() < 0.01);
        assert!((c6.lyrics_toggle.centre().0 - c6.more.centre().0) < (c6.more.centre().0 - c6.next.centre().0));
        assert!((c6.lyrics_toggle.right() - m.toolbar.right()).abs() < 0.01);
    }

    #[::std::prelude::v1::test]
    fn lyric_source_enabled_helper_detects_all_disabled() {
        assert!(any_lyric_source_enabled(&serde_json::json!({})));
        assert!(any_lyric_source_enabled(&serde_json::json!({
            "lyrics": { "enabledSources": { "qq": false, "kugou": true } }
        })));
        assert!(!any_lyric_source_enabled(&serde_json::json!({
            "lyrics": { "enabledSources": { "qq": false, "kugou": false, "netease": false } }
        })));
    }

    #[::std::prelude::v1::test]
    fn caption_rects_fit_window_and_compact_modes() {
        let (w_zone, [w_min, w_fs, w_close]) = window_caption_rects(1280.0, false);
        assert!(w_min.x < w_fs.x && w_fs.x < w_close.x);
        assert!(w_close.right() <= 1280.0 && w_zone.contains(w_close.centre().0, w_close.centre().1));
        assert_eq!(w_min.w, 28.0);

        let (c_zone, [c_min, c_fs, c_close]) = window_caption_rects(380.0, true);
        assert!(c_min.x < c_fs.x && c_fs.x < c_close.x);
        assert!(c_close.right() <= 380.0 && c_zone.contains(c_close.centre().0, c_close.centre().1));
        assert!(c_min.w < w_min.w && c_min.h < w_min.h);
    }
}
