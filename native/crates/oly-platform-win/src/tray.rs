//! Notification-area panel state.
//!
//! Placement arithmetic lives in [`crate::window::place_tray_panel`] because it
//! is pure geometry shared with the window tests; this module owns the small
//! state machine around it:
//!
//! * the panel is hidden, never destroyed — reopening keeps its scroll position
//!   and costs one show;
//! * losing focus closes it, unless an action (a colour dialog, say) holds it;
//! * the click that closed the panel through focus loss must not immediately
//!   reopen it, hence the 150 ms guard the reference bridge uses;
//! * the height follows the content between 180 and 680 logical pixels and is
//!   clamped before it ever reaches `resize`.

use crate::window::{
    place_tray_panel, RectF, TrayPoint, TRAY_GAP, TRAY_PANEL_MAX_H, TRAY_PANEL_MIN_H,
    TRAY_PANEL_WIDTH,
};

pub use crate::window::{
    TRAY_GAP as PANEL_GAP, TRAY_PANEL_MAX_H as PANEL_MAX_H, TRAY_PANEL_MIN_H as PANEL_MIN_H,
    TRAY_PANEL_WIDTH as PANEL_WIDTH,
};

/// Default height before the panel has measured its content.
pub const PANEL_DEFAULT_H: f64 = 400.0;

/// A click that blurred the panel this recently is not a reopen request.
pub const REOPEN_GUARD_MS: u64 = 150;

/// What a click on the icon should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayToggle {
    /// The panel was closed and is now open.
    Opened,
    /// The panel was open and is now closed.
    Closed,
    /// The panel closed moments ago; this click is the same gesture.
    Ignored,
}

/// Open/closed state plus the height and blur timestamp the host needs.
#[derive(Clone, Debug)]
pub struct TrayPanel {
    open: bool,
    height: f64,
    last_blur_ms: Option<u64>,
    held: bool,
}

impl Default for TrayPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl TrayPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            height: PANEL_DEFAULT_H,
            last_blur_ms: None,
            held: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn height(&self) -> f64 {
        self.height
    }

    pub fn is_held(&self) -> bool {
        self.held
    }

    /// A modal action (colour dialog, file picker) that must survive a blur.
    pub fn hold(&mut self, held: bool) {
        self.held = held;
    }

    /// Clamps and stores a renderer-reported height.
    pub fn set_height(&mut self, height: f64) -> f64 {
        if height.is_finite() {
            self.height = height.clamp(TRAY_PANEL_MIN_H, TRAY_PANEL_MAX_H);
        }
        self.height
    }

    /// The icon was clicked.
    pub fn toggle(&mut self, now_ms: u64) -> TrayToggle {
        if self.open {
            self.open = false;
            self.last_blur_ms = Some(now_ms);
            return TrayToggle::Closed;
        }
        if let Some(blurred) = self.last_blur_ms {
            if now_ms.saturating_sub(blurred) < REOPEN_GUARD_MS {
                return TrayToggle::Ignored;
            }
        }
        self.open = true;
        self.last_blur_ms = None;
        TrayToggle::Opened
    }

    /// The panel lost focus. Returns whether it closed.
    pub fn blur(&mut self, now_ms: u64) -> bool {
        if !self.open || self.held {
            return false;
        }
        self.open = false;
        self.last_blur_ms = Some(now_ms);
        true
    }

    /// Where the panel goes for an icon rectangle on a monitor work area.
    pub fn place(&self, anchor: RectF, area: RectF) -> TrayPoint {
        place_tray_panel(anchor, TRAY_PANEL_WIDTH, self.height, area)
    }
}

/// Re-exported so callers that only know about the tray do not need the window
/// module.
pub fn placement(anchor: RectF, height: f64, area: RectF) -> TrayPoint {
    place_tray_panel(anchor, TRAY_PANEL_WIDTH, height, area)
}

/// Kept public for the G3 matrix: the flyout is 8 px from the icon.
pub const GAP: f64 = TRAY_GAP;

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> RectF {
        RectF::new(0.0, 0.0, 1920.0, 1040.0)
    }

    #[test]
    fn a_toggle_opens_then_closes_and_guards_the_reopen() {
        let mut panel = TrayPanel::new();
        assert_eq!(panel.toggle(1_000), TrayToggle::Opened);
        assert!(panel.is_open());
        assert_eq!(panel.toggle(1_200), TrayToggle::Closed);
        assert!(!panel.is_open());
        // The click that closed it must not reopen it.
        assert_eq!(panel.toggle(1_300), TrayToggle::Ignored);
        assert!(!panel.is_open());
        assert_eq!(panel.toggle(1_200 + REOPEN_GUARD_MS), TrayToggle::Opened);
    }

    #[test]
    fn blur_closes_an_open_panel_once() {
        let mut panel = TrayPanel::new();
        panel.toggle(1_000);
        assert!(panel.blur(1_500));
        assert!(!panel.blur(1_600));
        assert!(!panel.is_open());
    }

    #[test]
    fn a_held_panel_survives_blur() {
        let mut panel = TrayPanel::new();
        panel.toggle(1_000);
        panel.hold(true);
        assert!(!panel.blur(1_500));
        assert!(panel.is_open());
        panel.hold(false);
        assert!(panel.blur(1_600));
    }

    #[test]
    fn the_height_is_clamped_to_the_reference_range() {
        let mut panel = TrayPanel::new();
        assert_eq!(panel.height(), PANEL_DEFAULT_H);
        assert_eq!(panel.set_height(10.0), TRAY_PANEL_MIN_H);
        assert_eq!(panel.set_height(10_000.0), TRAY_PANEL_MAX_H);
        assert_eq!(panel.set_height(f64::NAN), TRAY_PANEL_MAX_H);
        assert_eq!(panel.set_height(320.0), 320.0);
    }

    #[test]
    fn the_panel_sits_above_a_bottom_taskbar_icon() {
        let mut panel = TrayPanel::new();
        panel.set_height(400.0);
        let point = panel.place(RectF::new(1800.0, 1010.0, 24.0, 24.0), area());
        assert!(point.y + 400.0 <= 1010.0);
        // The icon is 108 px from the right edge, so the centred panel would
        // end at 1682; the work-area clamp pulls it back to 1920-260-8.
        assert_eq!(point.x, 1920.0 - 260.0 - TRAY_GAP);
        assert!(point.x + 260.0 <= 1920.0);
    }
}
