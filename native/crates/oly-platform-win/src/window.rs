//! Window geometry, shape transitions, and the settings panel's lifecycle.
//!
//! Ported from `src-tauri/src/wins/mod.rs` and `src/render/{modes,window-transition}.ts`.
//! Everything here is pure arithmetic and explicit clocks so the rules can be
//! replayed in a test without a window: the Tauri version had to be read through
//! `AppHandle`s, which is exactly what made the reference behaviours ("the strip
//! is always mini + miniPos", "only the wide shape writes the rectangle",
//! "a collapse waits 100 ms") easy to break without noticing.
//!
//! The one rule that is not arithmetic — "the strip's size is never remembered"
//! — is enforced by [`geometry_patch`] returning different shapes per mode
//! rather than a generic rectangle, so a caller cannot accidentally write the
//! wide size from a compact mode.

/// A rectangle in logical pixels. Origin is the top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl RectF {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// One window shape's fixed size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModeSize {
    pub width: f64,
    pub height: f64,
}

impl ModeSize {
    pub fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

/// The four shapes. `album` exists but the toolbar never offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinMode {
    Mini,
    Album,
    Lyrics,
    Window,
}

impl WinMode {
    /// Sizes from `shared/display-constants.ts` (`miniWidth`/`miniHeight`/…).
    pub fn size(self) -> ModeSize {
        match self {
            WinMode::Mini => ModeSize::new(380.0, 176.0),
            WinMode::Album => ModeSize::new(350.0, 350.0),
            WinMode::Lyrics => ModeSize::new(380.0, 650.0),
            WinMode::Window => ModeSize::new(1280.0, 840.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WinMode::Mini => "mini",
            WinMode::Album => "album",
            WinMode::Lyrics => "lyrics",
            WinMode::Window => "window",
        }
    }

    /// The mode cache accepts exactly these four labels.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "mini" => Some(WinMode::Mini),
            "album" => Some(WinMode::Album),
            "lyrics" => Some(WinMode::Lyrics),
            "window" => Some(WinMode::Window),
            _ => None,
        }
    }
}

/// 04 §8.1: only leaving the wide panel waits for the old content to fade.
pub fn is_collapse(from: WinMode, to: WinMode) -> bool {
    from == WinMode::Window && to != WinMode::Window
}

/// `MODE_COLLAPSE_DELAY_MS`, or zero when the swap can land at once.
pub fn switch_delay(from: WinMode, to: WinMode) -> u64 {
    if is_collapse(from, to) { 100 } else { 0 }
}

/// Keep the top-left corner in place, shrinking only if the work area requires it.
///
/// This is `transitionTarget`: the compact header must stay where the wide
/// panel's header was, so shrinking does not slide the window across the desk.
pub fn transition_target(from: RectF, size: ModeSize, area: Option<RectF>) -> RectF {
    let Some(area) = area else {
        return RectF::new(from.x, from.y, size.width, size.height);
    };
    let width = size.width.min(area.width);
    let height = size.height.min(area.height);
    let max_x = area.x + area.width - width;
    let max_y = area.y + area.height - height;
    RectF::new(
        from.x.max(area.x).min(max_x),
        from.y.max(area.y).min(max_y),
        width,
        height,
    )
}

/// `p³(p(6p−15)+10)`: zero velocity *and* zero acceleration at both ends.
fn ease(p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    p * p * p * (p * (p * 6.0 - 15.0) + 10.0)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    (a + (b - a) * t).round()
}

/// One frame of the 380 ms geometry animation.
pub fn interpolate_bounds(from: RectF, to: RectF, progress: f64) -> RectF {
    let t = ease(progress);
    RectF::new(
        lerp(from.x, to.x, t),
        lerp(from.y, to.y, t),
        lerp(from.width, to.width, t),
        lerp(from.height, to.height, t),
    )
}

/// Every frame the animation would apply, starting at `from` and ending at `to`.
///
/// Used by the QA harness to prove the curve ends on the target and takes the
/// documented 380 ms rather than a fixed handful of frames.
pub fn animate_steps(from: RectF, to: RectF, duration_ms: u64, frame_hz: f64) -> Vec<RectF> {
    let mut out = Vec::new();
    if frame_hz <= 0.0 || duration_ms == 0 {
        out.push(from);
        out.push(to);
        return out;
    }
    let step = 1000.0 / frame_hz;
    let mut elapsed = 0.0f64;
    out.push(from);
    while elapsed < duration_ms as f64 {
        elapsed = (elapsed + step).min(duration_ms as f64);
        out.push(interpolate_bounds(from, to, elapsed / duration_ms as f64));
    }
    if out.last() != Some(&to) {
        out.push(to);
    }
    out
}

/// Where a never-placed strip is born: up and to the right of centre.
pub fn default_mini_origin(area: RectF) -> (f64, f64) {
    (
        area.x + area.width - WinMode::Mini.size().width - area.width * 0.3,
        area.y + area.height * 0.3,
    )
}

/// The startup rectangle and its shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RestoredGeometry {
    pub x: f64,
    pub y: f64,
    pub size: ModeSize,
}

/// Startup is always `mini` plus the remembered origin — never the wide size.
pub fn restore_geometry(saved: Option<(f64, f64)>, area: Option<RectF>) -> RestoredGeometry {
    let size = WinMode::Mini.size();
    let (mut x, mut y) = match saved {
        Some((x, y)) if x.is_finite() && y.is_finite() => (x, y),
        _ => area.map(default_mini_origin).unwrap_or((0.0, 0.0)),
    };
    if let Some(area) = area {
        let max_x = (area.x + area.width - size.width).max(area.x);
        let max_y = (area.y + area.height - size.height).max(area.y);
        x = x.clamp(area.x, max_x);
        y = y.clamp(area.y, max_y);
    }
    RestoredGeometry { x, y, size }
}

/// The wide panel's remembered rectangle, floored at 800×500 and clamped.
pub fn restore_wide(saved: Option<RectF>, screen: Option<(i32, i32)>) -> RectF {
    let saved = saved.unwrap_or(RectF::new(f64::NAN, f64::NAN, 1280.0, 840.0));
    let width = if saved.width.is_finite() {
        saved.width.max(800.0)
    } else {
        1280.0
    };
    let height = if saved.height.is_finite() {
        saved.height.max(500.0)
    } else {
        840.0
    };
    let (w, h, x, y) = sanitize_geometry(Some(saved.x), Some(saved.y), width, height, screen);
    RectF::new(x, y, w, h)
}

/// Clamp a rectangle so at least 80 px of it stays reachable on screen.
pub fn sanitize_geometry(
    x: Option<f64>,
    y: Option<f64>,
    w: f64,
    h: f64,
    bounds: Option<(i32, i32)>,
) -> (f64, f64, f64, f64) {
    let (sw, sh) = bounds
        .map(|(w, h)| (w as f64, h as f64))
        .unwrap_or((1920.0, 1080.0));
    let width = if !w.is_finite() || w < 120.0 || w > sw {
        sw.min(1280.0)
    } else {
        w
    };
    let height = if !h.is_finite() || h < 120.0 || h > sh {
        sh.min(840.0)
    } else {
        h
    };
    let fit = |value: Option<f64>, span: f64, size: f64| -> Option<f64> {
        let value = value?;
        if !value.is_finite() {
            return None;
        }
        let min = -size + 80.0;
        let max = (span - size).max(0.0);
        Some(value.clamp(min, max))
    };
    (
        width,
        height,
        fit(x, sw, width).unwrap_or(0.0),
        fit(y, sh, height).unwrap_or(0.0),
    )
}

/// What a geometry save may write. The compact shapes share one origin and must
/// never write a size; only the wide panel owns the rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GeometryWrite {
    MiniOrigin { x: f64, y: f64 },
    Wide {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        user_sized: bool,
    },
}

pub fn geometry_patch(mode: WinMode, rect: RectF) -> GeometryWrite {
    if mode == WinMode::Window {
        GeometryWrite::Wide {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            user_sized: true,
        }
    } else {
        GeometryWrite::MiniOrigin {
            x: rect.x,
            y: rect.y,
        }
    }
}

impl GeometryWrite {
    /// The config patch this write stands for (`window.miniPos` vs `window.*`).
    pub fn to_patch(self) -> serde_json::Value {
        match self {
            GeometryWrite::MiniOrigin { x, y } => serde_json::json!({
                "window": { "miniPos": { "x": x, "y": y } }
            }),
            GeometryWrite::Wide {
                x,
                y,
                width,
                height,
                user_sized,
            } => serde_json::json!({
                "window": {
                    "x": x, "y": y, "width": width, "height": height,
                    "userSized": user_sized
                }
            }),
        }
    }
}

/// One resize at a time; newer requests are coalesced and win.
#[derive(Clone, Debug, Default)]
pub struct ResizeLane {
    in_flight: Option<RectF>,
    pending: Option<RectF>,
}

impl ResizeLane {
    /// True when the request can start immediately.
    pub fn begin(&mut self, rect: RectF) -> bool {
        if self.in_flight.is_none() && self.pending.is_none() {
            self.in_flight = Some(rect);
            true
        } else {
            self.pending = Some(rect);
            false
        }
    }

    /// Complete the in-flight request, returning the coalesced follow-up.
    pub fn finish(&mut self) -> Option<RectF> {
        self.in_flight = None;
        self.pending.take()
    }

    pub fn in_flight(&self) -> Option<RectF> {
        self.in_flight
    }
}

/// The 100 ms collapse delay, cancellable by a second mode request.
#[derive(Clone, Debug, Default)]
pub struct CollapseTimer {
    deadline: Option<u64>,
    target: Option<WinMode>,
}

impl CollapseTimer {
    /// Returns true when a delay was scheduled; false means "apply now".
    pub fn schedule(&mut self, now_ms: u64, from: WinMode, to: WinMode) -> bool {
        self.deadline = None;
        self.target = None;
        let delay = switch_delay(from, to);
        if delay == 0 {
            return false;
        }
        self.deadline = Some(now_ms + delay);
        self.target = Some(to);
        true
    }

    pub fn pending_at(&self, now_ms: u64) -> bool {
        self.deadline.is_some_and(|deadline| now_ms < deadline)
    }

    /// The target whose delay has elapsed, consuming it.
    pub fn take(&mut self, now_ms: u64) -> Option<WinMode> {
        let deadline = self.deadline?;
        if now_ms < deadline {
            return None;
        }
        self.deadline = None;
        self.target.take()
    }
}

/// One step the settings panel's timer chain asks the host to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsEvent {
    /// Start the renderer's 240 ms exit animation.
    Closing,
    /// The exit animation is over; the window may be hidden.
    HideAt(u64),
    /// Hide, then reclaim after the idle window.
    DestroyAt(u64),
    Hide,
    Destroy,
}

/// The settings panel's open/hold/timer state.
///
/// The reference runs this on the host so a re-open, a colour-dialog hold and a
/// stale timer cannot interleave into "panel destroyed while the user is
/// dragging a slider".
#[derive(Clone, Debug, Default)]
pub struct SettingsLifecycle {
    open: bool,
    visible: bool,
    hold: bool,
    closed_at: u64,
    hide_at: Option<u64>,
    destroy_at: Option<u64>,
}

impl SettingsLifecycle {
    /// 240 ms exit + 60 ms buffer.
    pub const HIDE_MS: u64 = 300;
    /// The reference's WebView reclaim delay.
    pub const IDLE_MS: u64 = 10_000;
    /// A click that blurred the panel must not also reopen it.
    pub const REOPEN_GUARD_MS: u64 = 200;

    /// Returns true when this call actually opened the panel.
    pub fn open(&mut self, now_ms: u64) -> bool {
        if self.open {
            return false;
        }
        if self.closed_at != 0 && now_ms.saturating_sub(self.closed_at) < Self::REOPEN_GUARD_MS {
            return false;
        }
        self.open = true;
        self.visible = true;
        self.hide_at = None;
        self.destroy_at = None;
        true
    }

    /// Begin the close chain. Empty while held or already closed.
    pub fn close(&mut self, now_ms: u64) -> Vec<SettingsEvent> {
        if self.hold || !self.open {
            return Vec::new();
        }
        self.open = false;
        // The guard is anchored at the close *start*, not at the hide: the
        // click that blurred the panel arrives while the 240 ms exit is still
        // running, and it must not reopen what it just dismissed. Anchoring at
        // the hide instead would swallow a genuine press ten seconds later.
        self.closed_at = now_ms;
        let hide_at = now_ms + Self::HIDE_MS;
        let destroy_at = hide_at + Self::IDLE_MS;
        self.hide_at = Some(hide_at);
        self.destroy_at = Some(destroy_at);
        vec![
            SettingsEvent::Closing,
            SettingsEvent::HideAt(hide_at),
            SettingsEvent::DestroyAt(destroy_at),
        ]
    }

    pub fn hold(&mut self, held: bool) {
        self.hold = held;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Still on screen while the exit animation runs, even though `open` is false.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_held(&self) -> bool {
        self.hold
    }

    /// Fires the deadlines that have elapsed. A reopen in between cancels both.
    pub fn tick(&mut self, now_ms: u64) -> Vec<SettingsEvent> {
        let mut out = Vec::new();
        if !self.open {
            if let Some(hide_at) = self.hide_at {
                if now_ms >= hide_at {
                    self.hide_at = None;
                    self.visible = false;
                    out.push(SettingsEvent::Hide);
                }
            }
            if let Some(destroy_at) = self.destroy_at {
                if now_ms >= destroy_at {
                    self.destroy_at = None;
                    // The reference records the destruction time too, so a
                    // press aimed at the window that just vanished is not
                    // treated as a fresh open.
                    self.closed_at = now_ms;
                    out.push(SettingsEvent::Destroy);
                }
            }
        }
        out
    }
}

/// --------------------------------------------------------------------- tray
/// Tray flyout placement, from `src-tauri/src/tray.rs` `place_panel`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrayPoint {
    pub x: f64,
    pub y: f64,
}

/// 260 wide, 180..680 tall, 8 px from the icon.
pub const TRAY_PANEL_WIDTH: f64 = 260.0;
pub const TRAY_PANEL_MIN_H: f64 = 180.0;
pub const TRAY_PANEL_MAX_H: f64 = 680.0;
pub const TRAY_GAP: f64 = 8.0;

/// Above the icon if there is room, below if the taskbar is at the top, inside
/// the work area when the icon itself is outside it. Always clamped.
pub fn place_tray_panel(anchor: RectF, width: f64, height: f64, area: RectF) -> TrayPoint {
    let width = width.min(area.width);
    let height = height.min(area.height);

    let mut x = anchor.x + anchor.width / 2.0 - width / 2.0;
    let mut y = anchor.y - height - TRAY_GAP;
    if anchor.y + anchor.height <= area.y || y < area.y {
        y = anchor.y + anchor.height + TRAY_GAP;
    }
    if anchor.x + anchor.width <= area.x {
        x = anchor.x + anchor.width + TRAY_GAP;
    }
    if anchor.x >= area.x + area.width {
        x = anchor.x - width - TRAY_GAP;
    }

    let mx = TRAY_GAP.min(((area.width - width) / 2.0).max(0.0));
    let my = TRAY_GAP.min(((area.height - height) / 2.0).max(0.0));
    let min_x = area.x + mx;
    let max_x = (area.x + area.width - width - mx).max(min_x);
    let min_y = area.y + my;
    let max_y = (area.y + area.height - height - my).max(min_y);

    TrayPoint {
        x: x.clamp(min_x, max_x).round(),
        y: y.clamp(min_y, max_y).round(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compact_origin_write_never_carries_a_size() {
        let patch = geometry_patch(WinMode::Lyrics, RectF::new(4.0, 5.0, 380.0, 650.0)).to_patch();
        let window = &patch["window"];
        assert!(window.get("miniPos").is_some());
        assert!(window.get("width").is_none());
        assert!(window.get("height").is_none());
    }

    #[test]
    fn tray_panels_flip_below_a_top_taskbar() {
        let area = RectF::new(0.0, 0.0, 1920.0, 1080.0);
        // Taskbar at the bottom: panel sits above the icon.
        let above = place_tray_panel(RectF::new(1800.0, 1040.0, 24.0, 24.0), 260.0, 400.0, area);
        assert!(above.y + 400.0 <= 1040.0);
        // Taskbar at the top: no room above, so the panel goes below.
        let below = place_tray_panel(RectF::new(1800.0, 8.0, 24.0, 24.0), 260.0, 400.0, area);
        assert!(below.y >= 32.0);
        // A side taskbar outside the work area moves the panel inside.
        let side = place_tray_panel(RectF::new(1930.0, 500.0, 24.0, 24.0), 260.0, 400.0, area);
        assert!(side.x + 260.0 <= 1920.0);
    }

    #[test]
    fn the_settings_timer_chain_reports_hide_and_destroy_once() {
        let mut life = SettingsLifecycle::default();
        life.open(0);
        life.close(0);
        assert_eq!(life.tick(SettingsLifecycle::HIDE_MS - 1), Vec::new());
        assert_eq!(life.tick(SettingsLifecycle::HIDE_MS), vec![SettingsEvent::Hide]);
        assert_eq!(life.tick(SettingsLifecycle::HIDE_MS + 1), Vec::new());
        let destroy = SettingsLifecycle::HIDE_MS + SettingsLifecycle::IDLE_MS;
        assert_eq!(life.tick(destroy), vec![SettingsEvent::Destroy]);
    }
}
