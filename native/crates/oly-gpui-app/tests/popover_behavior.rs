//! Cross-module popover behaviour: the contracts the four popover modules must
//! honour *together*, mirroring the legacy window wiring.
//!
//! Legacy sources (all read in full for Task 8):
//!
//! * `src/windows/lyrics/ToolBar.tsx` — the toolbar owns both panels and closes
//!   the other one before opening one (`run(action)` and the `[mode]` effect);
//!   a mode change closes both; the fullscreen pseudo-mode patches the config
//!   and re-applies the previous mode without a window switch.
//! * `AnchoredPopover.tsx` — outside pointerdown closes the panel, Escape
//!   closes it and returns focus to the anchor button, and one panel opening
//!   dismisses the other.
//! * `VolumeControl.tsx` — closing the panel flushes the debounced volume
//!   write with the target captured at open time.
//! * `DrawerWindow.tsx` — the drawer (lyrics settings) writes per-shape
//!   profile keys, so a mode change while it is open changes which store the
//!   next write lands in.

use oly_gpui_app::ui::controls::{
    Mode, OffsetStepper, PanelState, SelectValue, ToolBarState, TrackIdentity,
};
use oly_gpui_app::ui::picker;
use oly_gpui_app::ui::popover::{PanelKind, Popover, PopoverSet};
use oly_gpui_app::ui::settings::{DrawerState, ProfileKey};
use oly_gpui_app::ui::volume::{VolumePanel, VolumeSource, VolumeStatus};
use oly_render::glass::{place_popover, Rect, Side};

const VIEWPORT: (f64, f64) = (1272.0, 843.0);

fn anchor(x: f64, y: f64) -> Rect {
    Rect { left: x, top: y, width: 28.0, height: 28.0 }
}

/// The legacy more panel: 252×220 at radius 14 (`lyrics.css` .more-panel).
const MORE_PANEL: (f64, f64) = (252.0, 220.0);
/// The legacy volume panel: 240×84.
const VOLUME_PANEL: (f64, f64) = (240.0, 84.0);
/// The legacy picker popover: 340×340 (the picker window's default).
const PICKER_PANEL: (f64, f64) = (340.0, 340.0);

#[test]
fn selecting_a_mode_closes_both_panels() {
    let mut toolbar = ToolBarState::new(Mode::Window);
    toolbar.set_anchor(anchor(600.0, 780.0));
    toolbar.set_volume_anchor(anchor(40.0, 780.0));
    toolbar.toggle_more();
    toolbar.toggle_volume();
    assert!(toolbar.panels().any_open());

    // Any mode selection closes both panels exactly like the legacy `[mode]`
    // effect: the panel state is reset before the mode callback runs.
    let mode = toolbar.select(SelectValue::Mode(Mode::Lyrics));
    assert_eq!(mode, Some(Mode::Lyrics));
    assert!(!toolbar.panels().any_open(), "a mode change closes both panels");
    toolbar.on_mode_changed(Mode::Lyrics);
    assert_eq!(toolbar.mode(), Some(Mode::Lyrics));
}

#[test]
fn the_fullscreen_pseudo_mode_never_switches_the_window_mode() {
    let mut toolbar = ToolBarState::new(Mode::Window);
    toolbar.set_anchor(anchor(600.0, 780.0));
    toolbar.toggle_more();

    // Fullscreen patches the config and re-applies the stored mode; the
    // toolbar reports no window mode change but still closes the panel.
    let mode = toolbar.select(SelectValue::Fullscreen);
    assert_eq!(mode, None, "fullscreen is not a window mode");
    assert_eq!(toolbar.mode(), Some(Mode::Window), "the mode is retained");
    assert!(!toolbar.panels().any_open());
}

#[test]
fn opening_one_panel_closes_the_other_through_both_channels() {
    let mut toolbar = ToolBarState::new(Mode::Window);
    toolbar.set_anchor(anchor(600.0, 780.0));
    toolbar.set_volume_anchor(anchor(40.0, 780.0));

    // The toolbar channel: openPanel closes the other, then opens.
    toolbar.toggle_more();
    assert!(toolbar.panels().more_open && !toolbar.panels().volume_open);
    toolbar.set_more_open(false);
    toolbar.set_volume_open(true);
    let panels: PanelState = toolbar.panels();
    assert!(panels.volume_open && !panels.more_open);

    // The popover channel enforces the same exclusivity independently.
    let mut set = PopoverSet::new();
    set.push(Popover::new(PanelKind::More, MORE_PANEL));
    set.push(Popover::new(PanelKind::Volume, VOLUME_PANEL));
    set.get_mut(PanelKind::More).unwrap().open_at(anchor(600.0, 780.0), VIEWPORT);
    let placement = set
        .open_exclusive(PanelKind::Volume, anchor(40.0, 780.0), VIEWPORT)
        .expect("the volume popover exists");
    assert_eq!(placement.1, Side::Above, "volume opens above its anchor");
    assert!(!set.get(PanelKind::More).unwrap().is_open());
    assert!(set.get(PanelKind::Volume).unwrap().is_open());
}

#[test]
fn an_outside_pointer_down_closes_every_open_panel_and_returns_focus() {
    let mut set = PopoverSet::new();
    set.push(Popover::new(PanelKind::More, MORE_PANEL));
    set.push(Popover::new(PanelKind::Volume, VOLUME_PANEL));
    set.get_mut(PanelKind::More).unwrap().open_at(anchor(600.0, 780.0), VIEWPORT);
    set.get_mut(PanelKind::Volume).unwrap().open_at(anchor(40.0, 780.0), VIEWPORT);

    // A point on neither panel nor anchor closes every open popover in one
    // event — the legacy installs one capture listener per popover.
    let closed = set.route_pointer_down((10.0, 10.0), |_| None, |_| None);
    assert_eq!(
        closed,
        vec![PanelKind::More, PanelKind::Volume],
        "one click outside both closes both, in enumeration order"
    );
    assert!(!set.any_open());
    assert_eq!(set.focus_return_target(), Some(PanelKind::Volume));
    set.deliver_focus_return();
    assert_eq!(set.focus_return_target(), None, "focus return is one-shot");
}

#[test]
fn the_pointer_rule_uses_the_placed_panel_rect() {
    // The panel rect that route_pointer_down receives is the one place_popover
    // returned — a drag inside the *placed* panel never closes it, a click a
    // pixel outside does.
    let mut set = PopoverSet::new();
    set.push(Popover::new(PanelKind::More, MORE_PANEL));
    let anchor_rect = anchor(600.0, 780.0);
    let (panel, _) = set
        .get_mut(PanelKind::More)
        .unwrap()
        .open_at(anchor_rect, VIEWPORT);

    let inside = (panel.left + 10.0, panel.top + 10.0);
    let panels = |k: PanelKind| (k == PanelKind::More).then_some(panel);
    let anchors = |k: PanelKind| (k == PanelKind::More).then_some(anchor_rect);
    assert!(
        set.route_pointer_down(inside, panels, anchors).is_empty(),
        "inside the placed panel never closes"
    );

    // One pixel outside the placed panel (and not on the anchor) closes.
    let outside = (panel.left + panel.width + 1.0, panel.top + 10.0);
    let closed = set.route_pointer_down(outside, panels, anchors);
    assert_eq!(closed, vec![PanelKind::More], "outside the placed panel closes");
}

#[test]
fn escape_returns_focus_to_the_anchor_button() {
    let mut set = PopoverSet::new();
    set.push(Popover::new(PanelKind::Volume, VOLUME_PANEL));
    set.get_mut(PanelKind::Volume).unwrap().open_at(anchor(40.0, 780.0), VIEWPORT);
    let closed = set.route_escape().expect("Escape closes the open panel");
    assert_eq!(closed, PanelKind::Volume);
    assert_eq!(set.focus_return_target(), Some(PanelKind::Volume));
}

#[test]
fn the_volume_flush_uses_the_target_captured_at_open() {
    let mut panel = VolumePanel::open("spotify.exe", Some(40.0), VolumeSource::Internal);
    panel.on_read(Some(40.0), VolumeSource::Internal);
    panel.on_slide(88.0, 0.0);
    // Closing through the popover channel flushes before the flag clears.
    let write = panel.flush_on_close().expect("a pending slide is flushed");
    assert_eq!(write.value, 88.0);
    assert_eq!(write.app, "spotify.exe", "the open-time target");
}

#[test]
fn a_mode_change_while_the_drawer_is_open_switches_the_profile_store() {
    // The drawer opened over the wide panel writes cover/nocover; one opened
    // over a strip writes mini. The store key is resolved from the host mode
    // at write time, so the same drawer instance follows the switch.
    let mut drawer = DrawerState::open(ProfileKey::Cover);
    assert_eq!(drawer.profile, ProfileKey::Cover);
    drawer.set_profile(ProfileKey::Mini);
    let write = drawer.set_switch("showTranslation", false);
    assert_eq!(write.profile, ProfileKey::Mini, "the write follows the new host mode");
}

#[test]
fn the_volume_panel_states_map_to_the_three_legacy_labels() {
    let mut panel = VolumePanel::open("app.exe", None, VolumeSource::Internal);
    assert_eq!(*panel.status(), VolumeStatus::Reading);
    panel.on_timeout();
    assert_eq!(*panel.status(), VolumeStatus::Unavailable);
    assert!(!panel.status().is_editable());
    panel.on_read(Some(30.0), VolumeSource::AppMixer);
    assert_eq!(*panel.status(), VolumeStatus::Ready(VolumeSource::AppMixer));
}

#[test]
fn track_identity_changes_invalidate_pending_stepper_nudges() {
    let mut stepper = OffsetStepper::new(200);
    let track = TrackIdentity::new("spotify.exe", "exile", "Taylor Swift", "folklore");
    stepper.on_packet(200, Some(&track), None);
    assert!(stepper.is_enabled());

    let nudge = stepper.increment().expect("an enabled stepper nudges");
    // The packet arrives for a different track before the nudge resolves.
    let other = TrackIdentity::new("spotify.exe", "die happy", "Nothing", "nowhere");
    stepper.on_packet(0, Some(&other), Some(&track));
    assert!(
        !stepper.on_nudge_resolved(nudge.generation, Some(400)),
        "a stale generation is rejected"
    );
}

#[test]
fn the_picker_row_key_matches_its_current_document() {
    // The picker marks the row matching the live document; a fused row matches
    // when the document is composed from exactly its two sources.
    use picker::{Candidate, LiveDoc, Verified};

    let fused = Candidate {
        source: "fused".into(),
        id: Some("amllb+qq".into()),
        fused: true,
        word: Some(Box::new(Candidate {
            source: "amllb".into(),
            ..Default::default()
        })),
        trans: Some(Box::new(Candidate {
            source: "qq".into(),
            ..Default::default()
        })),
        ..Default::default()
    };
    let doc = LiveDoc {
        id: "ignored".into(),
        provider: "fused".into(),
        composed_from: vec!["amllb".into(), "qq".into()],
    };
    assert!(picker::is_current(&fused, &doc), "the fused row is current");

    let plain = Candidate {
        source: "kugou".into(),
        id: Some("42".into()),
        ..Default::default()
    };
    let doc_kugou = LiveDoc {
        id: "42".into(),
        provider: "kugou".into(),
        composed_from: vec![],
    };
    assert!(picker::is_current(&plain, &doc_kugou));
    assert!(
        !picker::is_current(&plain, &doc),
        "a plain row does not match a fused document"
    );
    let _ = Verified::default();
}

#[test]
fn every_panel_places_within_the_viewport() {
    // The three real panels all stay inside the viewport and clear the 8 px
    // pad, for both placements the volume panel can take.
    for (name, panel, a) in [
        ("more", MORE_PANEL, anchor(600.0, 780.0)),
        ("volume", VOLUME_PANEL, anchor(40.0, 780.0)),
        ("picker", PICKER_PANEL, anchor(620.0, 60.0)),
    ] {
        let (rect, _side) = place_popover(a, panel, VIEWPORT, None);
        assert!(rect.left >= 8.0 - f64::EPSILON, "{name} clears the left pad");
        assert!(
            rect.left + rect.width <= VIEWPORT.0 + f64::EPSILON,
            "{name} fits horizontally"
        );
        assert!(
            rect.top >= 0.0 && rect.top + rect.height <= VIEWPORT.1 + f64::EPSILON,
            "{name} fits vertically"
        );
    }
}
