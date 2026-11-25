//! Window geometry and lifecycle, pinned against the reference implementation
//! in `src-tauri/src/wins/mod.rs` and `src/render/{modes,window-transition}.ts`.
//!
//! These are the rules that a native port is most likely to get subtly wrong,
//! because they are all "obvious" until you compare them with the shipped build:
//!
//! * the strip's size is never remembered — only its origin;
//! * the wide panel's rectangle is never inherited by the compact shapes;
//! * a collapse waits 100 ms so the old content can fade, an expand does not;
//! * the 380 ms geometry curve has zero velocity *and* zero acceleration at both
//!   ends (a plain ease-in-out visibly jolts on the first frame);
//! * only one resize may be in flight, or the window lags behind the pointer by
//!   a queue of stale rectangles;
//! * the settings panel's idle reclaim must not destroy a panel that was
//!   reopened while the timer was pending.

use oly_platform_win::window::{
    ModeSize, RectF, ResizeLane, SettingsEvent, SettingsLifecycle, WinMode, animate_steps,
    default_mini_origin, geometry_patch, interpolate_bounds, is_collapse, restore_geometry,
    restore_wide, sanitize_geometry, switch_delay, transition_target,
};

fn area() -> RectF {
    RectF::new(0.0, 0.0, 1920.0, 1040.0)
}

// ---------------------------------------------------------------- mode sizes

#[test]
fn the_four_shapes_match_the_reference_table() {
    assert_eq!(WinMode::Mini.size(), ModeSize::new(380.0, 176.0));
    assert_eq!(WinMode::Album.size(), ModeSize::new(350.0, 350.0));
    assert_eq!(WinMode::Lyrics.size(), ModeSize::new(380.0, 650.0));
    assert_eq!(WinMode::Window.size(), ModeSize::new(1280.0, 840.0));
    // The mode cache only accepts these four labels.
    for mode in ["mini", "album", "lyrics", "window"] {
        assert!(WinMode::parse(mode).is_some(), "{mode} must parse");
    }
    assert!(WinMode::parse("compact").is_none());
    assert_eq!(WinMode::parse("window"), Some(WinMode::Window));
}

#[test]
fn only_leaving_the_wide_panel_waits_for_the_fade() {
    // 04 §8.1: the wide layout would be squeezed into a strip for one frame
    // otherwise, which reads as the cover flashing in the left edge.
    assert!(is_collapse(WinMode::Window, WinMode::Mini));
    assert!(is_collapse(WinMode::Window, WinMode::Lyrics));
    assert!(is_collapse(WinMode::Window, WinMode::Album));
    assert_eq!(switch_delay(WinMode::Window, WinMode::Mini), 100);
    // Compact → compact and anything → window land at once.
    assert!(!is_collapse(WinMode::Mini, WinMode::Lyrics));
    assert!(!is_collapse(WinMode::Mini, WinMode::Window));
    assert_eq!(switch_delay(WinMode::Mini, WinMode::Lyrics), 0);
    assert_eq!(switch_delay(WinMode::Lyrics, WinMode::Window), 0);
}

#[test]
fn a_transition_keeps_the_origin_until_the_work_area_forces_a_move() {
    let from = RectF::new(1700.0, 300.0, 1280.0, 840.0);
    let target = transition_target(from, WinMode::Mini.size(), Some(area()));
    // 1700 + 380 > 1920, so x clamps; y still fits.
    assert_eq!(target.width, 380.0);
    assert_eq!(target.height, 176.0);
    assert_eq!(target.x, 1920.0 - 380.0);
    assert_eq!(target.y, 300.0);
    // A strip that already fits keeps its origin untouched.
    let fits = transition_target(
        RectF::new(1400.0, 300.0, 1280.0, 840.0),
        WinMode::Mini.size(),
        Some(area()),
    );
    assert_eq!(fits.x, 1400.0);
    // With no work area the rectangle is reused verbatim.
    let free = transition_target(from, WinMode::Mini.size(), None);
    assert_eq!(free.x, from.x);
    assert_eq!(free.y, from.y);
}

#[test]
fn the_geometry_curve_has_zero_velocity_at_both_ends() {
    let from = RectF::new(0.0, 0.0, 380.0, 176.0);
    let to = RectF::new(100.0, 50.0, 1280.0, 840.0);
    // The quintic `p³(p(6p−15)+10)` starts and ends flat: the first and last
    // steps must be much smaller than the middle step.
    let first = interpolate_bounds(from, to, 0.05);
    let mid = interpolate_bounds(from, to, 0.5);
    assert_eq!(interpolate_bounds(from, to, 0.0), from);
    assert_eq!(interpolate_bounds(from, to, 1.0), to);
    assert!(
        (first.width - from.width).abs() < 3.0,
        "first frame must not jump: {}",
        first.width
    );
    assert!(
        (mid.width - (from.width + to.width) / 2.0).abs() < 1.0,
        "midpoint is the average: {}",
        mid.width
    );
    // Monotonic and clamped outside [0,1].
    assert_eq!(interpolate_bounds(from, to, -1.0), from);
    assert_eq!(interpolate_bounds(from, to, 2.0), to);
    let mut previous = from.width;
    for step in 1..=20 {
        let w = interpolate_bounds(from, to, step as f64 / 20.0).width;
        assert!(w >= previous, "width must not go backwards");
        previous = w;
    }
}

#[test]
fn the_380ms_animation_ends_exactly_on_the_target() {
    let from = RectF::new(0.0, 0.0, 380.0, 176.0);
    let to = RectF::new(60.0, 40.0, 1280.0, 840.0);
    let frames = animate_steps(from, to, 380, 60.0);
    assert_eq!(frames.first(), Some(&from));
    assert_eq!(frames.last(), Some(&to));
    // 380 ms at 60 Hz is 23 frames plus the terminal one; it must not be a
    // fixed 10-frame "animation" that finishes early.
    assert!(frames.len() >= 20, "got {} frames", frames.len());
}

// ------------------------------------------------------------- persistence

#[test]
fn a_saved_wide_rectangle_never_leaks_into_the_strip() {
    // The strip is always mini + miniPos, even when a 1280x840 rectangle is
    // stored: that bug came back as a 350x160 strip in the reference build.
    let restored = restore_geometry(Some((800.0, 400.0)), Some(area()));
    assert_eq!(restored.size, ModeSize::new(380.0, 176.0));
    assert_eq!((restored.x, restored.y), (800.0, 400.0));

    // First launch: up and to the right of centre.
    let born = restore_geometry(None, Some(area()));
    let expected = default_mini_origin(area());
    assert_eq!((born.x, born.y), expected);
    assert_eq!(expected, (1920.0 - 380.0 - 1920.0 * 0.3, 1040.0 * 0.3));
}

#[test]
fn a_strip_saved_on_a_missing_monitor_comes_back_inside_the_work_area() {
    // -4000 is off every attached display; the strip must be reachable.
    let restored = restore_geometry(Some((-4000.0, -4000.0)), Some(area()));
    assert!(restored.x >= 0.0 && restored.x + 380.0 <= 1920.0, "{restored:?}");
    assert!(restored.y >= 0.0 && restored.y + 176.0 <= 1040.0, "{restored:?}");
}

#[test]
fn the_wide_rectangle_is_clamped_for_a_missing_monitor() {
    let restored = restore_wide(Some(RectF::new(5000.0, 5000.0, 1600.0, 1000.0)), Some((1920, 1080)));
    assert_eq!(restored.width, 1600.0);
    assert_eq!(restored.height, 1000.0);
    assert!(restored.x + restored.width <= 1920.0);
    assert!(restored.y + restored.height <= 1080.0);
    // Older builds allowed 320x120; the wide panel needs room for its footer.
    let small = restore_wide(Some(RectF::new(10.0, 10.0, 320.0, 120.0)), Some((1920, 1080)));
    assert_eq!((small.width, small.height), (800.0, 500.0));
}

#[test]
fn sanitize_geometry_keeps_at_least_eighty_pixels_on_screen() {
    // A 4000x3000 rectangle on a 1920x1080 display is capped to the default
    // wide size, and its origin may go negative but not fully off screen.
    let (w, h, x, y) = sanitize_geometry(Some(-5000.0), Some(-5000.0), 4000.0, 3000.0, Some((1920, 1080)));
    assert_eq!((w, h), (1280.0, 840.0));
    assert!(x >= -w + 80.0, "x={x}");
    assert!(y >= -h + 80.0, "y={y}");
}

#[test]
fn only_the_wide_shape_writes_the_saved_rectangle() {
    // mini/album/lyrics share one origin and must never touch width/height.
    let mini = geometry_patch(WinMode::Mini, RectF::new(10.0, 20.0, 380.0, 176.0));
    assert_eq!(mini, oly_platform_win::window::GeometryWrite::MiniOrigin { x: 10.0, y: 20.0 });
    let lyrics = geometry_patch(WinMode::Lyrics, RectF::new(11.0, 21.0, 380.0, 650.0));
    assert_eq!(lyrics, oly_platform_win::window::GeometryWrite::MiniOrigin { x: 11.0, y: 21.0 });
    let wide = geometry_patch(WinMode::Window, RectF::new(30.0, 40.0, 1280.0, 840.0));
    assert_eq!(
        wide,
        oly_platform_win::window::GeometryWrite::Wide {
            x: 30.0,
            y: 40.0,
            width: 1280.0,
            height: 840.0,
            user_sized: true,
        }
    );
}

// ------------------------------------------------------------ resize lane

#[test]
fn only_one_resize_is_in_flight_and_the_latest_wins() {
    let mut lane = ResizeLane::default();
    assert!(lane.begin(RectF::new(0.0, 0.0, 400.0, 200.0)));
    // A second request while the first is pending is coalesced, not queued N×.
    assert!(!lane.begin(RectF::new(0.0, 0.0, 410.0, 210.0)));
    assert!(!lane.begin(RectF::new(0.0, 0.0, 420.0, 220.0)));
    let next = lane.finish().expect("a coalesced request survives");
    assert_eq!(next.width, 420.0, "the newest request wins");
    assert!(lane.finish().is_none(), "the lane is empty again");
    // No request at all is a no-op, not a panic.
    assert!(lane.finish().is_none());
}

// -------------------------------------------------------- settings timers

#[test]
fn closing_the_settings_panel_reports_the_exit_then_the_hide_then_the_destroy() {
    let mut life = SettingsLifecycle::default();
    assert!(life.open(1_000));
    // Reopening while open is idempotent: one entrance, one exit.
    assert!(!life.open(1_050));
    let events = life.close(2_000);
    assert_eq!(
        events,
        vec![
            SettingsEvent::Closing,
            SettingsEvent::HideAt(2_000 + 300),
            SettingsEvent::DestroyAt(2_000 + 300 + 10_000),
        ]
    );
    // Before the hide deadline nothing happens; at it, the panel hides once.
    assert!(life.tick(2_299).is_empty());
    assert_eq!(life.tick(2_300), vec![SettingsEvent::Hide]);
    assert!(life.tick(2_301).is_empty(), "hide is not repeated");
    assert_eq!(life.tick(12_300), vec![SettingsEvent::Destroy]);
    assert!(!life.is_open());
}

#[test]
fn reopening_before_the_hide_deadline_cancels_the_teardown() {
    let mut life = SettingsLifecycle::default();
    life.open(1_000);
    life.close(2_000);
    // A quick reopen must not be destroyed by the old timer. The reopen guard
    // is anchored at the close start (200 ms), so the press has to be later
    // than that but still before the 300 ms hide.
    assert!(life.open(2_250));
    assert!(life.tick(2_300).is_empty(), "stale hide must be dropped");
    assert!(life.tick(12_300).is_empty(), "stale destroy must be dropped");
    assert!(life.is_open());
}

#[test]
fn a_reopen_inside_the_guard_window_is_ignored() {
    let mut life = SettingsLifecycle::default();
    life.open(1_000);
    life.close(2_000);
    // The click that blurred the panel must not also reopen it: the guard is
    // measured from the close, so 150 ms in is the same gesture.
    assert!(!life.open(2_150), "inside the 200 ms guard");
    assert!(life.open(2_250), "outside the guard, still mid-exit");
    assert!(life.is_open(), "a rewrite cancels the pending hide and destroy");
    life.close(5_000);
    life.tick(5_300); // hidden; the destroy is ten seconds out
    assert_eq!(life.tick(15_300), vec![SettingsEvent::Destroy]);
    // The reference re-anchors the guard at the destruction, so a press aimed
    // at the panel that just vanished is not treated as a fresh open.
    assert!(!life.open(15_400), "the destroy re-anchors the guard");
    assert!(life.open(15_600), "and the guard expires on its own");
}

#[test]
fn the_colour_dialog_hold_blocks_the_close_and_the_idle_reclaim() {
    let mut life = SettingsLifecycle::default();
    life.open(1_000);
    life.hold(true);
    assert!(life.close(2_000).is_empty(), "a held panel does not close");
    assert!(life.is_open());
    life.hold(false);
    let events = life.close(3_000);
    assert_eq!(events.first(), Some(&SettingsEvent::Closing));
    // The panel stops accepting interaction at once (a second close is a no-op
    // and a reopen goes through the guard), but it stays on screen until the
    // exit animation ends.
    assert!(!life.is_open(), "closed as soon as the exit starts");
    assert!(life.is_visible(), "still on screen until the exit animation ends");
}

#[test]
fn a_collapse_timer_is_aborted_when_another_mode_is_requested() {
    // The renderer schedules the actual resize 100 ms after the click; a second
    // click in that window must land at once, not queue behind the first.
    let mut life = oly_platform_win::window::CollapseTimer::default();
    assert!(life.schedule(1_000, WinMode::Window, WinMode::Mini));
    assert!(life.pending_at(1_050));
    // A different transition aborts the pending one and schedules its own.
    assert!(life.schedule(1_060, WinMode::Window, WinMode::Lyrics));
    assert_eq!(life.take(1_099), None);
    assert_eq!(life.take(1_160), Some(WinMode::Lyrics));
}
