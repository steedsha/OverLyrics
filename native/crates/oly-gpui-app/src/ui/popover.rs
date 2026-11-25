//! Popover open/close discipline, ported from `AnchoredPopover.tsx` +
//! `useOutsideDismiss`.
//!
//! The legacy discipline, kept exactly:
//!
//! * **Outside pointerdown** (capture): if the composed path contains neither
//!   the panel nor the anchor button, close. Every open popover installs the
//!   same listener on the same event, so one pointerdown outside both panels
//!   closes both ("一次点击同时收起 more+volume"). A drag that starts inside
//!   a panel never closes it — the path *includes* the panel.
//! * **Escape**: preventDefault, close, return focus to the anchor button.
//! * **Opening** focuses the first non-disabled focusable inside the panel
//!   (`preventScroll`), and focus-return on close goes to the anchor.
//! * Placement recomputes on open and on resize/scroll/panel-resize via
//!   [`oly_render::glass::place_popover`].
//!
//! This module is the pure state machine for that discipline; the GPUI
//! element wiring lives with the app shell (Task 10).

use oly_render::glass::{place_popover, Rect, Side};

/// Which panel a popover hosts. The picker window (`picker-overlay.html`)
/// reuses the same anchor as the more panel; the volume panel is the other
/// surface on the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    More,
    Volume,
    Picker,
}

/// One popover lifecycle.
#[derive(Debug)]
pub struct Popover {
    kind: PanelKind,
    panel: (f64, f64),
    /// True while the entrance/exit animation plays; the panel stays
    /// interactive (legacy: it is a real `:popover-open` surface for the whole
    /// 452 ms).
    open: bool,
}

impl Popover {
    pub fn new(kind: PanelKind, panel: (f64, f64)) -> Self {
        Self {
            kind,
            panel,
            open: false,
        }
    }

    pub fn kind(&self) -> PanelKind {
        self.kind
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn panel_size(&self) -> (f64, f64) {
        self.panel
    }

    /// Opens the panel, targeting the anchor button. The caller re-places on
    /// every frame; the placement is returned so tests can pin it.
    pub fn open_at(
        &mut self,
        anchor: Rect,
        viewport: (f64, f64),
    ) -> (Rect, Side) {
        self.open = true;
        place_popover(anchor, self.panel, viewport, None)
    }

    /// Closes the panel. The legacy plays a 90 ms exit before unmounting; the
    /// shell handles the timing, this only records the state change.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Does this pointerdown land outside both this panel and its anchor?
    /// `panel_rect`/`anchor` are in window coordinates; `point` is the event
    /// position. Outside → the popover must close.
    pub fn should_close_on_pointer(
        &self,
        point: (f64, f64),
        panel_rect: Option<Rect>,
        anchor: Option<Rect>,
    ) -> bool {
        if !self.open {
            return false;
        }
        let in_rect = |r: &Rect| {
            point.0 >= r.left
                && point.0 < r.left + r.width
                && point.1 >= r.top
                && point.1 < r.top + r.height
        };
        if panel_rect.is_some_and(|r| in_rect(&r)) {
            return false;
        }
        if anchor.is_some_and(|r| in_rect(&r)) {
            return false;
        }
        true
    }
}

/// The set of popovers one window manages, plus the focus bookkeeping the
/// legacy does with the DOM (`focus({preventScroll})` on open, focus-return to
/// the anchor on Escape/close).
#[derive(Debug, Default)]
pub struct PopoverSet {
    popovers: Vec<Popover>,
    /// The anchor that last had focus, so Escape can hand focus back.
    focus_return: Option<PanelKind>,
}

impl PopoverSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, popover: Popover) {
        self.popovers.push(popover);
    }

    pub fn get(&self, kind: PanelKind) -> Option<&Popover> {
        self.popovers.iter().find(|p| p.kind == kind)
    }

    pub fn get_mut(&mut self, kind: PanelKind) -> Option<&mut Popover> {
        self.popovers.iter_mut().find(|p| p.kind == kind)
    }

    pub fn any_open(&self) -> bool {
        self.popovers.iter().any(|p| p.is_open())
    }

    /// One pointerdown routed to every open popover. Returns the panels that
    /// must close — for two open popovers and a point outside both, that is
    /// both of them (the legacy listens per-popover on one event).
    pub fn route_pointer_down(
        &mut self,
        point: (f64, f64),
        panels: impl Fn(PanelKind) -> Option<Rect>,
        anchors: impl Fn(PanelKind) -> Option<Rect>,
    ) -> Vec<PanelKind> {
        let mut closing = Vec::new();
        for kind in [PanelKind::More, PanelKind::Volume, PanelKind::Picker] {
            if let Some(p) = self.get_mut(kind) {
                if p.should_close_on_pointer(point, panels(kind), anchors(kind)) {
                    p.close();
                    self.focus_return = Some(kind);
                    closing.push(kind);
                }
            }
        }
        closing
    }

    /// Escape closes the topmost open popover and returns focus to its anchor.
    pub fn route_escape(&mut self) -> Option<PanelKind> {
        for kind in [PanelKind::More, PanelKind::Volume, PanelKind::Picker] {
            if let Some(p) = self.get_mut(kind) {
                if p.is_open() {
                    p.close();
                    self.focus_return = Some(kind);
                    return Some(kind);
                }
            }
        }
        None
    }

    /// The anchor that should receive focus after a dismissal.
    pub fn focus_return_target(&self) -> Option<PanelKind> {
        self.focus_return
    }

    /// Focus landing delivered (the shell focuses the anchor element).
    pub fn deliver_focus_return(&mut self) {
        self.focus_return = None;
    }

    /// Opens one panel and closes every other one. The legacy actually keeps
    /// more+volume independent, but the toolbar's `run()` helper closes both
    /// on every action that opens a window; the picker in a compact mode is
    /// native, so the in-page card closes the other panels. This mirrors that.
    pub fn open_exclusive(&mut self, kind: PanelKind, anchor: Rect, viewport: (f64, f64)) -> Option<(Rect, Side)> {
        for p in self.popovers.iter_mut() {
            p.close();
        }
        let placement = self.get_mut(kind)?.open_at(anchor, viewport);
        Some(placement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { left: x, top: y, width: w, height: h }
    }

    #[test]
    fn a_point_outside_both_panels_closes_both() {
        let mut set = PopoverSet::new();
        set.push(Popover::new(PanelKind::More, (252.0, 220.0)));
        set.push(Popover::new(PanelKind::Volume, (240.0, 84.0)));
        set.get_mut(PanelKind::More).unwrap().open = true;
        set.get_mut(PanelKind::Volume).unwrap().open = true;

        let more_panel = rect(211.34, 452.25, 252.0, 220.0);
        let volume_panel = rect(8.0, 588.25, 240.0, 84.0);
        let anchors = |_: PanelKind| Some(rect(600.0, 800.0, 36.0, 36.0));
        let closing = set.route_pointer_down(
            (640.0, 100.0),
            |k| match k {
                PanelKind::More => Some(more_panel),
                PanelKind::Volume => Some(volume_panel),
                _ => None,
            },
            anchors,
        );
        assert_eq!(closing.len(), 2, "one click outside both closes both");
        assert!(!set.any_open());
    }

    #[test]
    fn a_drag_inside_the_panel_never_closes_it() {
        let mut set = PopoverSet::new();
        set.push(Popover::new(PanelKind::More, (252.0, 220.0)));
        set.get_mut(PanelKind::More).unwrap().open = true;
        let more_panel = rect(211.34, 452.25, 252.0, 220.0);
        let closing = set.route_pointer_down(
            (300.0, 500.0),
            |k| match k {
                PanelKind::More => Some(more_panel),
                _ => None,
            },
            |_: PanelKind| None,
        );
        assert!(closing.is_empty());
        assert!(set.get(PanelKind::More).unwrap().is_open());
    }

    #[test]
    fn a_pointer_on_the_anchor_button_toggles_not_closes() {
        let mut set = PopoverSet::new();
        set.push(Popover::new(PanelKind::More, (252.0, 220.0)));
        set.get_mut(PanelKind::More).unwrap().open = true;
        let anchor = rect(600.0, 800.0, 36.0, 36.0);
        let closing = set.route_pointer_down(
            (610.0, 810.0),
            |k| match k {
                PanelKind::More => Some(rect(211.34, 452.25, 252.0, 220.0)),
                _ => None,
            },
            |k| match k {
                PanelKind::More => Some(anchor),
                _ => None,
            },
        );
        assert!(closing.is_empty(), "the button's own click handler toggles");
    }

    #[test]
    fn escape_closes_the_topmost_and_returns_focus() {
        let mut set = PopoverSet::new();
        set.push(Popover::new(PanelKind::More, (252.0, 220.0)));
        set.push(Popover::new(PanelKind::Volume, (240.0, 84.0)));
        set.get_mut(PanelKind::Volume).unwrap().open = true;
        let closed = set.route_escape();
        assert_eq!(closed, Some(PanelKind::Volume));
        assert_eq!(set.focus_return_target(), Some(PanelKind::Volume));
        set.deliver_focus_return();
        assert_eq!(set.focus_return_target(), None);
    }

    #[test]
    fn opening_one_panel_closes_the_others() {
        let mut set = PopoverSet::new();
        set.push(Popover::new(PanelKind::More, (252.0, 220.0)));
        set.push(Popover::new(PanelKind::Volume, (240.0, 84.0)));
        set.get_mut(PanelKind::More).unwrap().open = true;
        let anchor = rect(600.0, 800.0, 36.0, 36.0);
        let (placement, _) = set
            .open_exclusive(PanelKind::Volume, anchor, (1272.0, 843.0))
            .expect("the volume popover exists");
        assert!(placement.width > 0.0);
        assert!(!set.get(PanelKind::More).unwrap().is_open());
        assert!(set.get(PanelKind::Volume).unwrap().is_open());
    }
}
