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

#[path = "../translation_view.rs"]
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


mod frame_paint;
mod handlers;
mod render;
#[cfg(test)]
mod tests;
mod types;

pub(super) use frame_paint::*;
pub use types::*;

/// The player.
pub struct PlayerView {
    cfg: ViewConfig,
    settings_host:Option<SettingsHost>,
    settings_child:Option<AnyWindowHandle>,
    settings_child_view:Option<WeakEntity<PlayerView>>,
    desktop_child: Option<AnyWindowHandle>,
    desktop_bridge: Arc<super::desktop_lyrics::SharedDesktopBridge>,
    desktop_candidates_cache: Arc<Vec<PickerEntry>>,
    desktop_candidates_next_ms: f64,
    pending_desktop_lyrics: bool,
    main_hwnd: Option<isize>,
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
    prev_cover_surface: Option<CustomSurfaceHandle>,
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
    always_on_top: bool,
    applied_always_on_top: Option<bool>,
    window_controls_alpha: f32,
    pending_minimize: bool,
    pending_quit: bool,
    picker_scroll_spring: oly_core::scroll::Spring,
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
    cover_fade: f32,
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
        let mut this = Self {settings_host:None,settings_child:None,settings_child_view:None,desktop_child:None,desktop_bridge:super::desktop_lyrics::SharedDesktopBridge::new(),desktop_candidates_cache:Arc::new(Vec::new()),desktop_candidates_next_ms:0.0,pending_desktop_lyrics:false,main_hwnd:None,pending_settings_window:None,sync_settings_child_config:false,pending_import_rx:None,native_background:None,doc_auto_picked:false,auto_pick_persisted:false,qa_frame_sample:None,smart_translation_worker:None,translation_signature:None,translated_rows:Vec::new(),search_config:None,pressed_candidate:None,
            cfg,
            focus: cx.focus_handle(),
            scene: None,
            bg_surface: None,
            bg_size: (0, 0),
            cover: None,
            cover_surface: None,
            prev_cover_surface: None,
            cover_size: (0, 0),
            motion_surface: None,
            motion_size: (0, 0),
            shaper,
            display: Display::default(),
            settings:Settings::default(),
            sync,pending_mode:None,switch_drag:None,segmented_drag:None,pre_fullscreen_mode:2,last_fullscreen:false,always_on_top:false,applied_always_on_top:None,window_controls_alpha:0.0,pending_minimize:false,pending_quit:false,picker_scroll_spring:{let mut s=oly_core::scroll::Spring::new(0.0);s.update_params(oly_core::scroll::SpringParams::new(220.0,26.0));s},picker_capacity:4,picker_kind:PickerKind::Lyrics,chrome_wake_ms:0.,cursor_idle_ms:0.,cursor_hide_pending:false,browse_idle_ms:0.,lyrics_user_hidden:false,lyrics_auto_hidden:start_idle,lyrics_user_force_shown:false,lyrics_center:{let mut s=oly_core::scroll::Spring::new(if start_idle {1.0} else {0.0});s.update_params(oly_core::scroll::SpringParams::new(140.0,22.0));s},lyrics_reveal_alpha:if start_idle {0.0} else {1.0},staged_mode:None,wide_size:None,last_window_bounds:None,
            mode_transition:None,geometry_memory:Default::default(),
            volume_req_id:0,volume_read_id:None,volume_read_deadline_ms:None,volume_cached:None,
            pending_motion_path:None,motion_debounce_deadline_ms:None,motion_dispatched:None,motion_needs_clear:false,motion_last_enabled:true,motion_fade:0.0,inline_switch_drag:None,bg_cover_hash:None,cover_fade:1.0,cover_accent_hex:None,island_wake_ms:-10_000.0,island_drop:0.0,cover_gone_at_ms:None,compact_chrome_alpha:1.0,album_meta_alpha:1.0,
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
        if !this.cfg.settings_only {
            this.always_on_top = this
                .settings
                .config
                .pointer("/window/alwaysOnTop")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        }
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
        let desktop_vol_active = self.desktop_child.is_some();
        if self.tool.panels().volume_open || desktop_vol_active {
            if !self.volume.app().is_empty() && active_app != self.volume.app() {
                if self.tool.panels().volume_open {
                    self.close_volume_panel();
                } else if !active_app.is_empty() {
                    self.open_live_volume(mono_now);
                } else {
                    self.volume_read_id = None;
                    self.volume_read_deadline_ms = None;
                    self.volume = VolumePanel::open("", None, VolumeSource::Internal);
                    self.volume.on_error();
                }
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
        self.cover_fade = if self.cover_surface.is_some() { 0.0 } else { 1.0 };
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
                if let Some(old) = self.prev_cover_surface.take() {
                    window.destroy_custom_surface(old);
                }
                self.cover_size = (0, 0);
                self.cover_fade = 1.0;
                self.cover_dirty = false;
            }
            return;
        };
        if self.cover_dirty {
            self.cover_dirty = false;
            let bytes = cover.rgba.pixels.as_slice();
            if self.cover_fade < 1.0 {
                if let Some(old_prev) = self.prev_cover_surface.take() {
                    window.destroy_custom_surface(old_prev);
                }
                self.prev_cover_surface = self.cover_surface.take();
            } else if let Some(old) = self.cover_surface.take() {
                if self.cover_size == (cover.width, cover.height) {
                    if let Err(error) = window.update_custom_surface(old, bytes) {
                        log::warn!("cover surface update: {error:#}");
                    }
                    self.cover_surface = Some(old);
                    return;
                }
                window.destroy_custom_surface(old);
            }
            match window.create_custom_surface(cover.width, cover.height, bytes) {
                Ok(handle) => {
                    self.cover_surface = Some(handle);
                    self.cover_size = (cover.width, cover.height);
                }
                Err(error) => {
                    log::warn!("cover surface: {error:#}");
                    self.cover_fade = 1.0;
                }
            }
        }
        if self.cover_fade < 1.0 {
            self.cover_fade = (self.cover_fade + (dt_ms.clamp(0.0, 100.0) as f32 / 550.0)).min(1.0);
        }
        if self.cover_fade >= 1.0 {
            if let Some(old_prev) = self.prev_cover_surface.take() {
                window.destroy_custom_surface(old_prev);
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
                            child.settings.set_scroll_immediate(child.settings.max_scroll(panel));
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
                if let Some(view)=&self.settings_child_view{let _=view.update(cx,|v,cx|{v.settings.profile=profile;v.settings.active_player=active_player;v.settings.set_kind(kind);v.settings.open=true;v.settings.page=page;v.settings.title=sheet_title;v.settings.set_scroll_immediate(0.0);v.settings.close_choice();v.sync_profile_display();cx.notify();});}
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
            window_background:WindowBackgroundAppearance::Transparent,is_resizable:false,is_minimizable:false,
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
                if let Some(root)=child.downcast::<gpui_kit::base::Root>(){let _=root.update(cx,|r,w,cx|{r.style().background=Some(gpui_kit::transparent_black().into());if let Some(h)=super::win32_window::hwnd_from_window(w){super::win32_window::disable_dwm_corners_and_border(h);if self.always_on_top {super::win32_window::set_always_on_top(h,true);}}cx.notify();});}
                self.settings_child=Some(child);log::info!("[settings-sidecar] side={side:?} page={page} host={host:?} bounds={rect:?}");
            }
            Err(error)=>{log::warn!("settings side window: {error}");self.settings.open=true;}
        }
    }

    pub fn restore_from_desktop_lyrics(&mut self) {
        self.desktop_child = None;
        if let Some(h) = self.main_hwnd {
            super::win32_window::set_window_visible(h, true);
        }
    }

    pub fn tick_live_for_desktop(&mut self) {
        let now_ms = self.now_ms();
        let live_now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(now_ms);
        self.advance(now_ms, live_now_ms);

        let mut candidates_dirty = false;
        for cmd in self.desktop_bridge.drain_commands() {
            match cmd {
                super::desktop_lyrics::DesktopCommand::Prev => {
                    self.live_action(oly_platform_win::smtc::control::Action::Prev);
                    candidates_dirty = true;
                }
                super::desktop_lyrics::DesktopCommand::PlayPause => {
                    self.live_toggle();
                }
                super::desktop_lyrics::DesktopCommand::Next => {
                    self.live_action(oly_platform_win::smtc::control::Action::Next);
                    candidates_dirty = true;
                }
                super::desktop_lyrics::DesktopCommand::SyncNudge(delta) => {
                    self.nudge_sync(delta);
                }
                super::desktop_lyrics::DesktopCommand::OpenVolume => {
                    self.open_live_volume(now_ms);
                }
                super::desktop_lyrics::DesktopCommand::SetVolumePct(pct) => {
                    if self.volume.app().trim().is_empty() {
                        self.open_live_volume(now_ms);
                    }
                    let clamped = pct.clamp(0.0, 100.0);
                    if self.volume.status().is_editable() {
                        if let Some(write) = self.volume.on_slide(clamped, now_ms) {
                            self.send_volume_write(write);
                        }
                    } else if let Some(app) = self
                        .live
                        .as_ref()
                        .and_then(|l| l.model.packet.as_ref())
                        .map(|p| p.app.clone())
                        .filter(|a| !a.trim().is_empty())
                    {
                        self.send_volume_write(VolumeWrite {
                            app,
                            value: clamped,
                            source: VolumeSource::AppMixer,
                            generation: 0,
                        });
                    }
                }
                super::desktop_lyrics::DesktopCommand::PickCandidate(i) => {
                    self.picker_kind = PickerKind::Lyrics;
                    self.pick_lyric_candidate_at(i);
                    candidates_dirty = true;
                }
                super::desktop_lyrics::DesktopCommand::ToggleSmartTranslation(i) => {
                    self.picker_kind = PickerKind::Lyrics;
                    self.toggle_smart_translation_at(i);
                    candidates_dirty = true;
                }
                super::desktop_lyrics::DesktopCommand::RefreshLyrics => {
                    self.picker_kind = PickerKind::Lyrics;
                    self.trigger_picker_refresh();
                    candidates_dirty = true;
                }
                super::desktop_lyrics::DesktopCommand::ImportLyrics => {
                    self.trigger_picker_import();
                    candidates_dirty = true;
                }
                super::desktop_lyrics::DesktopCommand::ReturnToMain
                | super::desktop_lyrics::DesktopCommand::Close => {
                    self.restore_from_desktop_lyrics();
                }
                super::desktop_lyrics::DesktopCommand::SaveConfig(cfg) => {
                    let patch = cfg.to_patch();
                    if let Some(live) = self.live.as_ref() {
                        let _ = live.config.patch(patch.clone());
                    }
                    if let Some(obj) = self.settings.config.as_object_mut() {
                        if let Some(dl) = patch.get("desktopLyrics") {
                            obj.insert("desktopLyrics".into(), dl.clone());
                        }
                    }
                }
            }
        }

        let pos = self.lyric_time();
        // Zero-allocation single pass over main lyric rows.
        let mut active_pair: Option<(usize, &Row)> = None;
        let mut first_main: Option<(usize, &Row)> = None;
        let mut last_main: Option<(usize, &Row)> = None;
        let mut prev_before_next: Option<(usize, &Row)> = None;
        let mut next_after_pos: Option<(usize, &Row)> = None;
        for (idx, r) in self.rows.iter().enumerate() {
            if r.is_bg || r.text.trim().is_empty() {
                continue;
            }
            if first_main.is_none() {
                first_main = Some((idx, r));
            }
            if pos >= r.start_ms && pos <= r.end_ms + 350.0 {
                active_pair = Some((idx, r));
            }
            if next_after_pos.is_none() {
                if r.start_ms > pos {
                    prev_before_next = last_main;
                    next_after_pos = Some((idx, r));
                }
            }
            last_main = Some((idx, r));
        }

        let mut interlude_range = None;
        if active_pair.is_none() && first_main.is_some() {
            if let Some((_, next_row)) = next_after_pos {
                let next_start = next_row.start_ms;
                let prev_end = prev_before_next.map(|(_, r)| r.end_ms).unwrap_or(0.0);
                if next_start - prev_end >= 3000.0 && pos >= prev_end && pos < next_start {
                    interlude_range = Some((prev_end, next_start));
                } else if let Some(prev) = prev_before_next {
                    active_pair = Some(prev);
                } else {
                    active_pair = first_main;
                }
            } else {
                active_pair = last_main;
            }
        }

        let (active_row_index, active_row) = match active_pair {
            Some((idx, r)) => (Some(idx), Some(r.clone())),
            None => (None, None),
        };
        let (track_title, track_artist) = self
            .live
            .as_ref()
            .and_then(|l| l.track.as_ref())
            .map(|t| {
                let raw_a = t.artist.as_deref().unwrap_or("");
                let (clean_a, _) = oly_engine::motion_cover::split_smtc_artist_album(raw_a, "");
                (t.title.clone(), clean_a)
            })
            .or_else(|| {
                self.doc
                    .as_ref()
                    .map(|d| (d.title.clone(), d.artist.clone()))
            })
            .unwrap_or_default();
        let volume_enabled = self.volume.status().is_editable();
        let volume_pct = if volume_enabled {
            self.volume.value()
        } else {
            self.volume_cached
                .as_ref()
                .map(|(_, v)| *v)
                .unwrap_or_else(|| self.volume.value())
        };
        let volume_status = if volume_enabled {
            format!("{}%", volume_pct.round() as i32)
        } else {
            "—".into()
        };
        let volume_label = self.volume.status().label().to_string();
        if candidates_dirty || now_ms >= self.desktop_candidates_next_ms {
            let saved_kind = self.picker_kind;
            self.picker_kind = PickerKind::Lyrics;
            self.desktop_candidates_cache = Arc::new(self.candidates());
            self.picker_kind = saved_kind;
            self.desktop_candidates_next_ms = now_ms + 350.0;
        }
        let candidates = Arc::clone(&self.desktop_candidates_cache);
        let refresh_searching = self.picker_refresh_active
            || self.live.as_ref().is_some_and(|l| l.status == "正在搜索歌词…");
        self.desktop_bridge
            .update_snapshot(super::desktop_lyrics::DesktopSnapshot {
                playing: self.playing,
                track_title,
                track_artist,
                display: self.display,
                position_ms: pos,
                active_row,
                active_row_index,
                interlude_range,
                sync_offset_ms: self.sync.offset_ms(),
                volume_pct,
                volume_enabled,
                volume_status,
                volume_label,
                candidates,
                refresh_searching,
            });
    }

    fn toggle_smart_translation_at(&mut self, i: usize) {
        if let Some(entry) = self.candidates().get(i) {
            let key = entry.smart_translation_key.clone();
            if !key.is_empty() {
                let on = !entry.smart_translation;
                let mut overrides = self.settings.config["lyrics"]["translationOverrides"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                overrides.insert(key, serde_json::json!(on));
                self.change_setting(
                    "lyrics.translationOverrides",
                    serde_json::Value::Object(overrides),
                );
            }
        }
    }

    fn trigger_picker_import(&mut self) {
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

    fn trigger_picker_refresh(&mut self) {
        self.picker_refresh_spinning = true;
        let locked_rows = self.candidates().len().clamp(1, 4);
        if let Some(live) = self.live.as_mut() {
            live.search.clear_cache();
            self.picker_scroll_spring.set_position(0.0);
            self.picker_scroll_spring.set_target_position(0.0);
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

    fn pick_lyric_candidate_at(&mut self, i: usize) {
        let visible = self.candidates();
        if self.picker_kind == PickerKind::Media {
            if let Some(entry) = visible.get(i) {
                if let Some(live) = self.live.as_mut() {
                    let target = if entry.source == "media-auto" {
                        None
                    } else {
                        Some(entry.id.clone())
                    };
                    live.pinned_media_app = target.clone();
                    let _ = live
                        .host
                        .dispatch(crate::commands::HostCommand::SetPinnedSession(target));
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
        let wanted = self
            .pressed_candidate
            .clone()
            .or_else(|| {
                visible
                    .get(i)
                    .map(|r| (r.source.clone(), r.id.clone(), r.candidate_index))
            })
            .filter(|(source, id, _)| visible.iter().any(|r| &r.source == source && &r.id == id));
        if let Some(live) = self.live.as_mut() {
            let selected = wanted.and_then(|(source, id, index)| {
                live.candidates
                    .get(index)
                    .filter(|c| c.source == source && c.id == id)
                    .or_else(|| {
                        live.candidates
                            .iter()
                            .find(|c| c.source == source && c.id == id)
                    })
                    .cloned()
            });
            if let (Some(track), Some(candidate)) = (live.track.clone(), selected) {
                live.search
                    .request(live.model.generation(), track, Some(candidate));
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

    fn open_desktop_lyrics_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.desktop_child.is_some() {
            if let Some(h) = self.main_hwnd {
                super::win32_window::set_window_visible(h, false);
            }
            return;
        }
        if let Some(child) = self.settings_child.take() {
            let _ = child.update(cx, |_, w, _| w.remove_window());
        }
        let mut cfg = super::desktop_lyrics::DesktopLyricsConfig::from_config(&self.settings.config);
        cfg.locked = false;
        let (w, h) = cfg.window_size();
        let (rx, ry) = cfg
            .saved_pos()
            .unwrap_or_else(|| super::desktop_lyrics::default_origin_for(cfg.orientation, window, cx));
        let (x, y) = super::desktop_lyrics::clamp_origin(rx, ry, w, h, window, cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(gpui_kit::bounds(
                point(px(x), px(y)),
                size(px(w), px(h)),
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("OverLyrics Desktop Lyrics".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            window_background: WindowBackgroundAppearance::Transparent,
            is_resizable: false,
            is_minimizable: false,
            focus: false,
            display_id: window.display(cx).map(|d| d.id()),
            ..Default::default()
        };
        let bridge = self.desktop_bridge.clone();
        let parent = cx.entity().downgrade();
        self.tick_live_for_desktop();
        match gpui_kit::open_window(options, cx, move |_, cx| {
            let text_system = cx.text_system().clone();
            cx.new(|_| super::desktop_lyrics::DesktopLyricsView::new(bridge, parent, cfg, text_system))
        }) {
            Ok((child, _)) => {
                if let Some(root) = child.downcast::<gpui_kit::base::Root>() {
                    let _ = root.update(cx, |r, _, cx| {
                        r.style().background = Some(gpui_kit::transparent_black().into());
                        cx.notify();
                    });
                }
                self.desktop_child = Some(child);
                if let Some(hwnd) = self.main_hwnd {
                    cx.defer(move |_| {
                        super::win32_window::set_window_visible(hwnd, false);
                    });
                }
            }
            Err(error) => log::warn!("desktop lyrics window: {error}"),
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