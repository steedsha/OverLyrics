//! End-to-end state-machine flows across the shell's modules.
//!
//! Each test drives *several* widgets the way one window would, so the
//! contracts the individual unit tests pin cannot drift apart:
//!
//! * a mode change goes toolbar → geometry patch → collapse timer;
//! * the settings panel's timer chain runs against the same clock the volume
//!   debounce uses;
//! * one pointerdown closes both toolbar popovers and a volume panel closed by
//!   that gesture still flushes its pending value;
//! * the inbound mailbox coalesces packets but never loses a volume answer's
//!   identity;
//! * the tray's reopen guard and the settings reopen guard are separate timers
//!   and neither leaks into the other.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oly_contracts::PlayerPacket;
use oly_gpui_app::commands::{HostCommand, INBOX_CAPACITY, Inbox, ShellHost, ShellSink, UiEvent};
use oly_gpui_app::ui::controls::{Mode, SelectValue, ToolBarState};
use oly_gpui_app::ui::picker::{Candidate, Choice, LiveDoc, PickerCard};
use oly_gpui_app::ui::popover::{PanelKind, Popover, PopoverSet};
use oly_gpui_app::ui::settings::{DisplayConfig, DrawerState, Pane, ProfileKey, profile_key_for};
use oly_gpui_app::ui::volume::{VolumePanel, VolumeSource, VolumeStatus};
use oly_platform_win::hotkey::{Hotkey, Key, Modifiers, WINDOW_SHORTCUTS};
use oly_platform_win::smtc::{EventSink, VolumeEvent};
use oly_platform_win::tray::{REOPEN_GUARD_MS, TrayPanel, TrayToggle};
use oly_platform_win::window::{
    CollapseTimer, GeometryWrite, RectF, SettingsEvent, SettingsLifecycle, WinMode, geometry_patch,
};
use oly_render::glass::Rect;

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        left: x,
        top: y,
        width: w,
        height: h,
    }
}

fn packet(title: &str) -> PlayerPacket {
    PlayerPacket {
        app: "MSEdge".into(),
        title: title.into(),
        artist: "Gate Runner".into(),
        album: "Trace".into(),
        duration_ms: 180_000.0,
        position_ms: 28_149.0,
        status: Some(oly_contracts::state::PlaybackStatus::Code(4)),
        ..Default::default()
    }
}

fn volume_event(id: i64) -> VolumeEvent {
    VolumeEvent {
        app: "msedge.exe".into(),
        available: true,
        value: Some(0.5),
        mode: "mixer".into(),
        request_id: id,
        reason: None,
    }
}

// ---------------------------------------------------------------- mode flows

#[test]
fn a_wide_to_strip_switch_waits_one_hundred_ms_and_writes_only_the_origin() {
    // The user clicks 迷你 while the wide panel is up: the toolbar selects the
    // mode, the shell schedules the collapse, and only after the delay does the
    // geometry patch land — and it carries the strip origin, never a size.
    let mut bar = ToolBarState::new(Mode::Window);
    let selected = bar.select(SelectValue::Mode(Mode::Mini)).expect("a real mode");
    assert_eq!(selected, Mode::Mini);

    let mut timer = CollapseTimer::default();
    assert!(
        timer.schedule(1_000, WinMode::Window, WinMode::Mini),
        "leaving the wide panel is delayed"
    );
    assert!(timer.pending_at(1_050));
    assert_eq!(timer.take(1_099), None, "not before the 100 ms mark");
    assert_eq!(timer.take(1_100), Some(WinMode::Mini));

    let patch = geometry_patch(WinMode::Mini, RectF::new(1400.0, 300.0, 380.0, 176.0));
    match patch {
        GeometryWrite::MiniOrigin { x, y } => {
            assert_eq!((x, y), (1400.0, 300.0));
        }
        other => panic!("the compact shapes save an origin only, got {other:?}"),
    }
}

#[test]
fn a_second_click_inside_the_delay_lands_at_once_instead_of_queueing() {
    let mut timer = CollapseTimer::default();
    assert!(timer.schedule(1_000, WinMode::Window, WinMode::Mini));
    // 歌词 arrives 40 ms later: the pending mini target is dropped and the new
    // request is evaluated on its own (compact → compact has no delay).
    assert!(!timer.schedule(1_040, WinMode::Mini, WinMode::Lyrics));
    assert_eq!(timer.take(1_200), None, "the first target was cancelled");
}

#[test]
fn the_wide_shape_is_the_only_one_that_saves_a_rectangle() {
    let wide = geometry_patch(WinMode::Window, RectF::new(40.0, 60.0, 1280.0, 840.0));
    match wide {
        GeometryWrite::Wide {
            x,
            y,
            width,
            height,
            user_sized,
        } => {
            assert_eq!((x, y, width, height), (40.0, 60.0, 1280.0, 840.0));
            assert!(user_sized);
        }
        other => panic!("the wide panel saves the rectangle, got {other:?}"),
    }
    for mode in [WinMode::Mini, WinMode::Album, WinMode::Lyrics] {
        assert!(matches!(
            geometry_patch(mode, RectF::new(1.0, 2.0, 380.0, 176.0)),
            GeometryWrite::MiniOrigin { .. }
        ));
    }
}

// ------------------------------------------------------------- popover flows

#[test]
fn one_click_outside_both_toolbar_panels_closes_both_and_flushes_the_volume() {
    let mut bar = ToolBarState::new(Mode::Window);
    assert!(bar.toggle_more());
    assert!(bar.toggle_volume());
    assert!(bar.panels().any_open());

    let mut set = PopoverSet::new();
    set.push(Popover::new(PanelKind::More, (252.0, 220.0)));
    set.push(Popover::new(PanelKind::Volume, (240.0, 84.0)));
    set.get_mut(PanelKind::More).unwrap().open_at(rect(600.0, 800.0, 36.0, 36.0), (1272.0, 843.0));
    set.get_mut(PanelKind::Volume)
        .unwrap()
        .open_at(rect(700.0, 800.0, 36.0, 36.0), (1272.0, 843.0));

    let mut volume = VolumePanel::open("msedge.exe", Some(40.0), VolumeSource::AppMixer);
    volume.on_read(Some(40.0), VolumeSource::AppMixer);
    assert!(volume.on_slide(72.0, 2_000.0).is_none(), "a slide only debounces");

    let closing = set.route_pointer_down(
        (10.0, 10.0),
        |kind| match kind {
            PanelKind::More => Some(rect(211.0, 452.0, 252.0, 220.0)),
            PanelKind::Volume => Some(rect(8.0, 588.0, 240.0, 84.0)),
            PanelKind::Picker => None,
        },
        |kind| match kind {
            PanelKind::More => Some(rect(600.0, 800.0, 36.0, 36.0)),
            PanelKind::Volume => Some(rect(700.0, 800.0, 36.0, 36.0)),
            PanelKind::Picker => None,
        },
    );
    assert_eq!(closing.len(), 2, "both panels close from one pointerdown");
    assert!(!set.any_open());
    bar.set_more_open(false);
    bar.set_volume_open(false);
    assert!(!bar.panels().any_open());

    let flush = volume
        .flush_on_close()
        .expect("closing the panel writes the pending value");
    assert_eq!(flush.value, 72.0);
    assert_eq!(flush.app, "msedge.exe", "the target captured at open time");
    assert_eq!(volume.generation(), 1, "the flush bumps the generation");
}

#[test]
fn escape_walks_the_layers_down_one_at_a_time() {
    // The shipped Escape is a stack, not a toggle: the popover first, and only
    // once nothing is open does the same key reach the window.
    let mut set = PopoverSet::new();
    set.push(Popover::new(PanelKind::Volume, (240.0, 84.0)));
    set.get_mut(PanelKind::Volume)
        .unwrap()
        .open_at(rect(700.0, 800.0, 36.0, 36.0), (1272.0, 843.0));
    assert_eq!(set.route_escape(), Some(PanelKind::Volume));
    assert_eq!(set.focus_return_target(), Some(PanelKind::Volume));
    assert_eq!(set.route_escape(), None, "nothing left to close");
    set.deliver_focus_return();
    assert_eq!(set.focus_return_target(), None);
}

// ------------------------------------------------------------ settings flow

#[test]
fn the_settings_chain_reports_exit_hide_and_destroy_on_one_clock() {
    let mut life = SettingsLifecycle::default();
    assert!(life.open(1_000));
    assert!(life.is_open() && life.is_visible());

    let events = life.close(2_000);
    assert_eq!(
        events,
        vec![
            SettingsEvent::Closing,
            SettingsEvent::HideAt(2_300),
            SettingsEvent::DestroyAt(12_300),
        ]
    );
    // The panel is off the *open* list at once (a second Escape must not start
    // a second exit) but stays visible through the 240 ms animation.
    assert!(!life.is_open());
    assert!(life.is_visible());

    assert!(life.tick(2_299).is_empty());
    assert_eq!(life.tick(2_300), vec![SettingsEvent::Hide]);
    assert!(!life.is_visible());
    assert!(life.tick(2_301).is_empty(), "destroy is ten seconds out");
    assert_eq!(life.tick(12_300), vec![SettingsEvent::Destroy]);
}

#[test]
fn a_short_reopen_cancels_the_teardown_and_a_click_inside_the_guard_does_not() {
    let mut life = SettingsLifecycle::default();
    life.open(1_000);
    life.close(2_000);

    // The user reopens 100 ms after the exit began — still inside the 200 ms
    // reopen guard, so the click is treated as the same gesture.
    assert!(!life.open(2_100), "inside the reopen guard");
    // 500 ms later the panel accepts the reopen and both timers are cancelled.
    assert!(life.open(2_500));
    assert!(life.is_visible());
    assert!(life.tick(12_400).is_empty(), "no hide and no destroy fire");
}

#[test]
fn the_drawer_switches_stores_with_the_window_mode() {
    assert_eq!(profile_key_for(Mode::Window, true), ProfileKey::Cover);
    assert_eq!(profile_key_for(Mode::Window, false), ProfileKey::Nocover);
    // The three compact strips share one store — the reference build wrote
    // them to `mini` for exactly that reason.
    for mode in [Mode::Mini, Mode::Album, Mode::Lyrics] {
        assert_eq!(profile_key_for(mode, true), ProfileKey::Mini);
        assert_eq!(profile_key_for(mode, false), ProfileKey::Mini);
    }

    let mut drawer = DrawerState::open(ProfileKey::Cover);
    assert_eq!(drawer.pane, Pane::Type);
    drawer.set_pane(Pane::Fx);
    assert_eq!(drawer.pane.label(), "歌词效果");
    drawer.set_profile(ProfileKey::Mini);

    let mut config = DisplayConfig::default();
    let write = drawer.set_switch("showTranslation", true);
    write.apply(&mut config);
    assert_eq!(
        config
            .profiles
            .get("mini")
            .and_then(|store| store.get("showTranslation")),
        Some(&oly_gpui_app::ui::settings::Value::Bool(true)),
        "the write lands in the strip store, not the wide one"
    );
    assert!(!config.profiles.contains_key("cover"));
}

// -------------------------------------------------------------- volume flow

#[test]
fn a_volume_drag_debounces_then_writes_one_value() {
    let mut panel = VolumePanel::open("Spotify.exe", None, VolumeSource::Internal);
    assert_eq!(*panel.status(), VolumeStatus::Reading);
    panel.on_read(None, VolumeSource::AppMixer);
    assert_eq!(
        *panel.status(),
        VolumeStatus::Ready(VolumeSource::AppMixer),
        "no internal volume falls back to the app mixer"
    );
    assert!(panel.on_slide(30.0, 1_000.0).is_none());
    assert!(panel.on_slide(44.0, 1_040.0).is_none(), "the newest move replaces");
    assert!(panel.tick(1_100.0).is_none(), "still inside the 80 ms debounce");
    let write = panel.tick(1_120.0).expect("the debounce elapsed");
    assert_eq!(write.value, 44.0);
    assert_eq!(write.app, "Spotify.exe");
    assert!(panel.tick(9_999.0).is_none(), "a write fires once");
}

#[test]
fn an_unreachable_player_disables_the_slider_but_never_writes() {
    let mut panel = VolumePanel::open("gone.exe", Some(10.0), VolumeSource::Internal);
    panel.on_timeout();
    assert_eq!(*panel.status(), VolumeStatus::Unavailable);
    assert!(!panel.status().is_editable());
    assert!(panel.on_slide(80.0, 0.0).is_none());
    assert!(panel.tick(10_000.0).is_none());
    assert!(panel.flush_on_close().is_none(), "nothing pending to flush");
}

// --------------------------------------------------------------- inbox flow

#[test]
fn the_mailbox_keeps_the_newest_packet_and_every_volume_answer() {
    let inbox = Arc::new(Mutex::new(Inbox::default()));
    let sink = ShellSink::new(inbox.clone());

    for index in 0..40 {
        sink.state(packet(&format!("track {index}")));
    }
    sink.volume(volume_event(1));
    sink.volume(volume_event(2));

    let mut guard = inbox.lock().unwrap();
    let drained = guard.drain();
    assert_eq!(drained.len(), 3, "one state plus two volume answers");
    match &drained[0] {
        UiEvent::State(p) => assert_eq!(p.title, "track 39"),
        other => panic!("expected the newest packet, got {other:?}"),
    }
    let ids: Vec<i64> = drained[1..]
        .iter()
        .map(|event| match event {
            UiEvent::Volume(v) => v.request_id,
            other => panic!("expected a volume answer, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2]);
    assert_eq!(guard.state_dropped(), 39);
}

#[test]
fn an_idle_mark_supersedes_a_packet_but_a_later_packet_wins_again() {
    let inbox = Arc::new(Mutex::new(Inbox::default()));
    let sink = ShellSink::new(inbox.clone());
    sink.state(packet("a"));
    sink.idle();
    assert!(matches!(
        inbox.lock().unwrap().drain().as_slice(),
        [UiEvent::Idle]
    ));
    sink.idle();
    sink.state(packet("b"));
    let drained = inbox.lock().unwrap().drain();
    assert!(matches!(&drained[..], [UiEvent::State(p)] if p.title == "b"));
    assert_eq!(INBOX_CAPACITY, 32);
}

#[test]
fn a_detached_shell_refuses_to_pretend_a_player_was_reached() {
    let host = ShellHost::detached();
    assert!(!host.has_workers());
    assert!(!host.dispatch(HostCommand::Refresh));
    assert!(!host.dispatch(HostCommand::VolumeGet {
        app: "msedge.exe".into(),
        mode: "auto".into(),
        request_id: 1,
    }));
    host.follow("MSEdge");
    assert_eq!(host.current_app(), "MSEdge");
}

#[test]
fn the_sink_is_usable_from_a_worker_thread() {
    // The worker side must not need the UI thread: this is the same Arc the
    // SMTC poller holds.
    let inbox = Arc::new(Mutex::new(Inbox::default()));
    let sink = ShellSink::new(inbox.clone());
    let shared: Arc<dyn EventSink> = Arc::new(sink);
    let handle = std::thread::spawn(move || {
        shared.state(packet("from worker"));
        shared.volume(volume_event(9));
    });
    handle.join().unwrap();
    let drained = inbox.lock().unwrap().drain();
    assert_eq!(drained.len(), 2);
    std::thread::sleep(Duration::from_millis(1));
}

// ----------------------------------------------------------------- tray flow

#[test]
fn the_tray_guard_swallows_the_click_that_closed_the_panel() {
    let mut tray = TrayPanel::new();
    assert_eq!(tray.toggle(1_000), TrayToggle::Opened);
    assert!(tray.is_open());
    // A blur closes it; the click that caused the blur arrives right after.
    assert!(tray.blur(1_100));
    assert_eq!(tray.toggle(1_150), TrayToggle::Ignored);
    assert_eq!(tray.toggle(1_100 + REOPEN_GUARD_MS), TrayToggle::Opened);
    // A held panel survives the blur (the colour dialog case).
    tray.hold(true);
    assert!(!tray.blur(2_000));
    assert!(tray.is_open());
}

#[test]
fn the_tray_height_is_clamped_before_it_reaches_a_resize() {
    let mut tray = TrayPanel::new();
    assert_eq!(tray.set_height(10.0), 180.0);
    assert_eq!(tray.set_height(2_000.0), 680.0);
    assert_eq!(tray.set_height(420.0), 420.0);
    let placement = tray.place(RectF::new(1_800.0, 1_000.0, 24.0, 24.0), RectF::new(0.0, 0.0, 1920.0, 1_040.0));
    assert!(placement.x >= 0.0 && placement.y >= 0.0);
}

// ------------------------------------------------------------- keyboard flow

#[test]
fn the_published_shortcuts_all_parse_and_escape_matches_exactly() {
    for (spec, label) in WINDOW_SHORTCUTS {
        assert!(Hotkey::parse(spec).is_some(), "{spec} must parse");
        assert!(!label.is_empty());
    }
    let escape = Hotkey::parse("Escape").expect("escape");
    assert!(escape.matches(Modifiers::NONE, Key::Escape));
    assert!(
        !escape.matches(Modifiers::ctrl(), Key::Escape),
        "a modified Escape is a different chord"
    );
    let play = Hotkey::parse("Space").expect("space");
    assert!(play.matches(Modifiers::NONE, Key::Space));
}

// --------------------------------------------------------------- picker flow

#[test]
fn a_pick_locks_the_card_and_a_failure_unlocks_it() {
    let row = Candidate {
        source: "kugou".into(),
        id: Some("k1".into()),
        title: Some("稻香".into()),
        percent: Some(97.0),
        ..Default::default()
    };
    let live = LiveDoc {
        id: "other".into(),
        provider: "netease".into(),
        composed_from: Vec::new(),
    };
    let mut card = PickerCard::new();
    assert_eq!(card.choose(&row, &live), Choice::Picked);
    assert!(card.busy().is_some());
    assert_eq!(card.choose(&row, &live), Choice::Ignored, "a second click is ignored");
    assert_eq!(card.on_pick_result(false, Some("切换失败")), Choice::Ignored);
    assert!(card.error().is_some());
    assert!(card.busy().is_none(), "the failure unlocks the card");

    assert_eq!(card.choose(&row, &live), Choice::Picked);
    assert_eq!(card.on_pick_result(true, None), Choice::Closed);
    assert!(card.is_closing());
}
