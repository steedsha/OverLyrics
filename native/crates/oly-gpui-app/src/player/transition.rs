//! Smooth window geometry and content transitions across Mini, Lyrics, Window, and Fullscreen modes.

use crate::ui::controls::Mode;

pub const MINI_WIDTH: f32 = 380.0;
pub const MINI_HEIGHT: f32 = 176.0;
pub const ALBUM_SIZE: f32 = 350.0;
pub const LYRICS_HEIGHT: f32 = 650.0;
pub const COMPACT_MORPH_MS: f32 = 380.0;
pub const CROSS_FADE_OUT_MS: f32 = 50.0;
pub const CROSS_MORPH_MS: f32 = 250.0;
pub const CROSS_FADE_IN_MS: f32 = 80.0;
pub const FULLSCREEN_FADE_OUT_MS: f32 = 70.0;
pub const FULLSCREEN_FADE_IN_MS: f32 = 120.0;
pub const FULLSCREEN_SETTLE_TIMEOUT_MS: f64 = 1500.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowShape {
    Mini,
    Album,
    Lyrics,
    Window,
    Fullscreen,
}

impl WindowShape {
    pub fn from_mode(mode: Mode, fullscreen: bool) -> Self {
        if fullscreen {
            Self::Fullscreen
        } else {
            match mode {
                Mode::Mini => Self::Mini,
                Mode::Album => Self::Album,
                Mode::Lyrics => Self::Lyrics,
                Mode::Window => Self::Window,
            }
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Mini,
            1 => Self::Lyrics,
            3 => Self::Fullscreen,
            4 => Self::Album,
            _ => Self::Window,
        }
    }

    pub fn to_mode(self) -> Mode {
        match self {
            Self::Mini => Mode::Mini,
            Self::Album => Mode::Album,
            Self::Lyrics => Mode::Lyrics,
            Self::Window | Self::Fullscreen => Mode::Window,
        }
    }

    pub fn is_compact(self) -> bool {
        matches!(self, Self::Mini | Self::Album | Self::Lyrics)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }
}

pub fn quintic(t: f32) -> f32 {
    let p = t.clamp(0.0, 1.0);
    p * p * p * (p * (6.0 * p - 15.0) + 10.0)
}

pub fn interpolate_bounds(from: RectF, to: RectF, eased: f32) -> RectF {
    let p = eased.clamp(0.0, 1.0);
    RectF {
        x: from.x + (to.x - from.x) * p,
        y: from.y + (to.y - from.y) * p,
        w: (from.w + (to.w - from.w) * p).max(1.0),
        h: (from.h + (to.h - from.h) * p).max(1.0),
    }
}

pub fn clamp_to_work_area(mut rect: RectF, work: Option<RectF>) -> RectF {
    let Some(work) = work else {
        return rect;
    };
    if work.w > 0.0 && work.h > 0.0 {
        rect.w = rect.w.min(work.w).max(1.0);
        rect.h = rect.h.min(work.h).max(1.0);
        rect.x = rect.x.clamp(work.x, (work.right() - rect.w).max(work.x));
        rect.y = rect.y.clamp(work.y, (work.bottom() - rect.h).max(work.y));
    }
    rect
}

pub fn target_size_for_shape(
    shape: WindowShape,
    wide_size: (f32, f32),
    work: Option<RectF>,
) -> (f32, f32) {
    match shape {
        WindowShape::Mini => (MINI_WIDTH, MINI_HEIGHT),
        WindowShape::Album => (ALBUM_SIZE, ALBUM_SIZE),
        WindowShape::Lyrics => (MINI_WIDTH, LYRICS_HEIGHT),
        WindowShape::Window => wide_size,
        WindowShape::Fullscreen => work
            .filter(|w| w.w > 0.0 && w.h > 0.0)
            .map(|w| (w.w, w.h))
            .unwrap_or(wide_size),
    }
}

pub fn transition_target(
    cur: RectF,
    from: WindowShape,
    to: WindowShape,
    wide_size: (f32, f32),
    work: Option<RectF>,
    saved_wide: Option<RectF>,
    saved_compact_origin: Option<(f32, f32)>,
) -> RectF {
    let (tw, th) = target_size_for_shape(to, wide_size, work);
    let raw = match (from, to) {
        (
            WindowShape::Mini | WindowShape::Album | WindowShape::Lyrics,
            WindowShape::Mini | WindowShape::Album | WindowShape::Lyrics,
        ) => RectF::new(cur.x, cur.y, tw, th),
        (_, WindowShape::Mini | WindowShape::Album | WindowShape::Lyrics) => {
            if let Some((cx, cy)) = saved_compact_origin {
                RectF::new(cx, cy, tw, th)
            } else {
                RectF::new(
                    cur.x + (cur.w - tw) * 0.5,
                    cur.y + (cur.h - th) * 0.5,
                    tw,
                    th,
                )
            }
        }
        (_, WindowShape::Window) => {
            if let Some(wide) = saved_wide {
                wide
            } else {
                RectF::new(
                    cur.x + (cur.w - tw) * 0.5,
                    cur.y + (cur.h - th) * 0.5,
                    tw,
                    th,
                )
            }
        }
        (_, WindowShape::Fullscreen) => work.unwrap_or(cur),
    };
    if to == WindowShape::Fullscreen {
        raw
    } else {
        clamp_to_work_area(raw, work)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionKind {
    Compact,
    CrossFadeMorph,
    FullscreenFade,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionStep {
    pub bounds: Option<RectF>,
    pub render_mode: Mode,
    pub content_alpha: f32,
    pub chrome_alpha: f32,
    pub trigger_fullscreen_toggle: bool,
    pub waiting_fullscreen: bool,
    pub finished: bool,
}

#[derive(Clone, Debug)]
pub struct ModeTransition {
    pub from_shape: WindowShape,
    pub to_shape: WindowShape,
    pub kind: TransitionKind,
    pub from_bounds: RectF,
    pub to_bounds: RectF,
    pub current_bounds: RectF,
    pub start_ms: Option<f64>,
    pub fade_out_ms: f32,
    pub morph_ms: f32,
    pub fade_in_ms: f32,
    pub fullscreen_sent_ms: Option<f64>,
    pub fullscreen_settled_ms: Option<f64>,
    pub fullscreen_bounds_applied: bool,
}

impl ModeTransition {
    pub fn new(
        from_shape: WindowShape,
        to_shape: WindowShape,
        from_bounds: RectF,
        to_bounds: RectF,
    ) -> Self {
        let kind = match (from_shape, to_shape) {
            (WindowShape::Fullscreen, _) | (_, WindowShape::Fullscreen) => {
                TransitionKind::FullscreenFade
            }
            (
                WindowShape::Mini | WindowShape::Lyrics,
                WindowShape::Mini | WindowShape::Lyrics,
            ) => TransitionKind::Compact,
            _ => TransitionKind::CrossFadeMorph,
        };
        let (fade_out_ms, morph_ms, fade_in_ms) = match kind {
            TransitionKind::Compact => (0.0, COMPACT_MORPH_MS, 0.0),
            TransitionKind::CrossFadeMorph => {
                (CROSS_FADE_OUT_MS, CROSS_MORPH_MS, CROSS_FADE_IN_MS)
            }
            TransitionKind::FullscreenFade => {
                (FULLSCREEN_FADE_OUT_MS, CROSS_MORPH_MS, FULLSCREEN_FADE_IN_MS)
            }
        };
        Self {
            from_shape,
            to_shape,
            kind,
            from_bounds,
            to_bounds,
            current_bounds: from_bounds,
            start_ms: None,
            fade_out_ms,
            morph_ms,
            fade_in_ms,
            fullscreen_sent_ms: None,
            fullscreen_settled_ms: None,
            fullscreen_bounds_applied: false,
        }
    }

    pub fn is_compact(&self) -> bool {
        self.kind == TransitionKind::Compact
    }

    pub fn involves_fullscreen(&self) -> bool {
        self.kind == TransitionKind::FullscreenFade
    }

    pub fn render_fullscreen(&self, is_fullscreen: bool, now_ms: f64) -> bool {
        if self.to_shape == WindowShape::Fullscreen {
            is_fullscreen
        } else if self.from_shape == WindowShape::Fullscreen {
            let in_fade_out = self
                .start_ms
                .is_none_or(|start| ((now_ms - start).max(0.0) as f32) < self.fade_out_ms);
            is_fullscreen && in_fade_out
        } else {
            false
        }
    }

    pub fn step(&mut self, now_ms: f64, is_fullscreen: bool) -> TransitionStep {
        let start = *self.start_ms.get_or_insert(now_ms);
        let elapsed = ((now_ms - start).max(0.0)) as f32;

        match self.kind {
            TransitionKind::Compact => {
                let p = (elapsed / self.morph_ms.max(1.0)).clamp(0.0, 1.0);
                let eased = quintic(p);
                let bounds = interpolate_bounds(self.from_bounds, self.to_bounds, eased);
                self.current_bounds = bounds;
                const FADE_OUT_END: f32 = 0.18;
                const FADE_IN_START: f32 = 0.76;
                let (render_shape, chrome_alpha) = if p < FADE_OUT_END {
                    (self.from_shape, (1.0 - p / FADE_OUT_END).clamp(0.0, 1.0))
                } else if p < FADE_IN_START {
                    (
                        if p < 0.45 { self.from_shape } else { self.to_shape },
                        0.0,
                    )
                } else {
                    (
                        self.to_shape,
                        quintic(((p - FADE_IN_START) / (1.0 - FADE_IN_START)).clamp(0.0, 1.0)),
                    )
                };
                let finished = p >= 1.0;
                TransitionStep {
                    bounds: Some(bounds),
                    render_mode: if finished {
                        self.to_shape.to_mode()
                    } else {
                        render_shape.to_mode()
                    },
                    content_alpha: 1.0,
                    chrome_alpha: if finished { 1.0 } else { chrome_alpha },
                    trigger_fullscreen_toggle: false,
                    waiting_fullscreen: false,
                    finished,
                }
            }
            TransitionKind::CrossFadeMorph => {
                if elapsed < self.fade_out_ms {
                    let p = (elapsed / self.fade_out_ms.max(1.0)).clamp(0.0, 1.0);
                    let alpha = (1.0 - quintic(p)).clamp(0.0, 1.0);
                    TransitionStep {
                        bounds: None,
                        render_mode: self.from_shape.to_mode(),
                        content_alpha: alpha,
                        chrome_alpha: alpha,
                        trigger_fullscreen_toggle: false,
                        waiting_fullscreen: false,
                        finished: false,
                    }
                } else if elapsed < self.fade_out_ms + self.morph_ms {
                    let mp =
                        ((elapsed - self.fade_out_ms) / self.morph_ms.max(1.0)).clamp(0.0, 1.0);
                    let bounds =
                        interpolate_bounds(self.from_bounds, self.to_bounds, quintic(mp));
                    self.current_bounds = bounds;
                    TransitionStep {
                        bounds: Some(bounds),
                        render_mode: self.to_shape.to_mode(),
                        content_alpha: 0.0,
                        chrome_alpha: 0.0,
                        trigger_fullscreen_toggle: false,
                        waiting_fullscreen: false,
                        finished: false,
                    }
                } else {
                    let ip = ((elapsed - self.fade_out_ms - self.morph_ms)
                        / self.fade_in_ms.max(1.0))
                        .clamp(0.0, 1.0);
                    let bounds = if (self.current_bounds.w - self.to_bounds.w).abs() > 0.5
                        || (self.current_bounds.h - self.to_bounds.h).abs() > 0.5
                        || (self.current_bounds.x - self.to_bounds.x).abs() > 0.5
                        || (self.current_bounds.y - self.to_bounds.y).abs() > 0.5
                    {
                        self.current_bounds = self.to_bounds;
                        Some(self.to_bounds)
                    } else {
                        None
                    };
                    let alpha = quintic(ip);
                    let finished = ip >= 1.0;
                    TransitionStep {
                        bounds,
                        render_mode: self.to_shape.to_mode(),
                        content_alpha: if finished { 1.0 } else { alpha },
                        chrome_alpha: if finished { 1.0 } else { alpha },
                        trigger_fullscreen_toggle: false,
                        waiting_fullscreen: false,
                        finished,
                    }
                }
            }
            TransitionKind::FullscreenFade => {
                if elapsed < self.fade_out_ms {
                    let p = (elapsed / self.fade_out_ms.max(1.0)).clamp(0.0, 1.0);
                    let alpha = (1.0 - quintic(p)).clamp(0.0, 1.0);
                    return TransitionStep {
                        bounds: None,
                        render_mode: self.from_shape.to_mode(),
                        content_alpha: alpha,
                        chrome_alpha: alpha,
                        trigger_fullscreen_toggle: false,
                        waiting_fullscreen: false,
                        finished: false,
                    };
                }
                if elapsed < self.fade_out_ms + self.morph_ms {
                    let mp =
                        ((elapsed - self.fade_out_ms) / self.morph_ms.max(1.0)).clamp(0.0, 1.0);
                    let bounds =
                        interpolate_bounds(self.from_bounds, self.to_bounds, quintic(mp));
                    self.current_bounds = bounds;
                    return TransitionStep {
                        bounds: Some(bounds),
                        render_mode: self.to_shape.to_mode(),
                        content_alpha: 0.0,
                        chrome_alpha: 0.0,
                        trigger_fullscreen_toggle: false,
                        waiting_fullscreen: false,
                        finished: false,
                    };
                }
                let want_fullscreen = self.to_shape == WindowShape::Fullscreen;
                let mut bounds = if !self.fullscreen_bounds_applied {
                    self.fullscreen_bounds_applied = true;
                    if !want_fullscreen {
                        self.current_bounds = self.to_bounds;
                        Some(self.to_bounds)
                    } else {
                        None
                    }
                } else {
                    None
                };
                let mut trigger_toggle = false;
                if self.fullscreen_sent_ms.is_none() {
                    self.fullscreen_sent_ms = Some(now_ms);
                    if is_fullscreen != want_fullscreen {
                        trigger_toggle = true;
                    }
                }
                if is_fullscreen != want_fullscreen {
                    let sent_at = self.fullscreen_sent_ms.unwrap_or(now_ms);
                    if now_ms - sent_at < FULLSCREEN_SETTLE_TIMEOUT_MS {
                        return TransitionStep {
                            bounds,
                            render_mode: self.to_shape.to_mode(),
                            content_alpha: 0.0,
                            chrome_alpha: 0.0,
                            trigger_fullscreen_toggle: trigger_toggle,
                            waiting_fullscreen: true,
                            finished: false,
                        };
                    }
                }
                if !want_fullscreen
                    && bounds.is_none()
                    && ((self.current_bounds.w - self.to_bounds.w).abs() > 0.5
                        || (self.current_bounds.h - self.to_bounds.h).abs() > 0.5
                        || (self.current_bounds.x - self.to_bounds.x).abs() > 0.5
                        || (self.current_bounds.y - self.to_bounds.y).abs() > 0.5)
                {
                    self.current_bounds = self.to_bounds;
                    bounds = Some(self.to_bounds);
                }
                let settled = *self.fullscreen_settled_ms.get_or_insert(now_ms);
                if self.fade_in_ms <= 0.0 {
                    return TransitionStep {
                        bounds,
                        render_mode: self.to_shape.to_mode(),
                        content_alpha: 0.0,
                        chrome_alpha: 0.0,
                        trigger_fullscreen_toggle: trigger_toggle,
                        waiting_fullscreen: false,
                        finished: true,
                    };
                }
                let in_elapsed = ((now_ms - settled).max(0.0)) as f32;
                let ip = (in_elapsed / self.fade_in_ms.max(1.0)).clamp(0.0, 1.0);
                let alpha = quintic(ip);
                let finished = ip >= 1.0;
                TransitionStep {
                    bounds,
                    render_mode: self.to_shape.to_mode(),
                    content_alpha: if finished { 1.0 } else { alpha },
                    chrome_alpha: if finished { 1.0 } else { alpha },
                    trigger_fullscreen_toggle: trigger_toggle,
                    waiting_fullscreen: false,
                    finished,
                }
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WindowGeometryMemory {
    pub wide_bounds: Option<RectF>,
    pub compact_origin: Option<(f32, f32)>,
}

impl WindowGeometryMemory {
    pub fn record_leaving(&mut self, shape: WindowShape, bounds: RectF) {
        match shape {
            WindowShape::Window => self.wide_bounds = Some(bounds),
            WindowShape::Mini | WindowShape::Album | WindowShape::Lyrics => {
                self.compact_origin = Some((bounds.x, bounds.y));
            }
            WindowShape::Fullscreen => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quintic_is_smooth_and_monotonic() {
        assert_eq!(quintic(0.0), 0.0);
        assert!((quintic(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(quintic(1.0), 1.0);
        let mut prev = 0.0;
        for i in 1..=20 {
            let v = quintic(i as f32 / 20.0);
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn mini_to_lyrics_preserves_top_left_when_it_fits_and_clamps_bottom() {
        let work = Some(RectF::new(0.0, 0.0, 1920.0, 1040.0));
        let high = transition_target(
            RectF::new(120.0, 200.0, 380.0, 176.0),
            WindowShape::Mini,
            WindowShape::Lyrics,
            (1280.0, 840.0),
            work,
            None,
            None,
        );
        assert_eq!(high, RectF::new(120.0, 200.0, 380.0, 650.0));

        let low = transition_target(
            RectF::new(120.0, 840.0, 380.0, 176.0),
            WindowShape::Mini,
            WindowShape::Lyrics,
            (1280.0, 840.0),
            work,
            None,
            None,
        );
        assert_eq!(low, RectF::new(120.0, 390.0, 380.0, 650.0));
    }

    #[test]
    fn window_round_trip_remembers_wide_and_compact_origins() {
        let work = Some(RectF::new(0.0, 0.0, 1920.0, 1040.0));
        let mut memory = WindowGeometryMemory::default();
        let wide = RectF::new(180.0, 90.0, 1280.0, 840.0);
        memory.record_leaving(WindowShape::Window, wide);

        let to_mini = transition_target(
            wide,
            WindowShape::Window,
            WindowShape::Mini,
            (1280.0, 840.0),
            work,
            memory.wide_bounds,
            memory.compact_origin,
        );
        assert_eq!(to_mini.w, 380.0);
        assert_eq!(to_mini.h, 176.0);

        let moved_mini = RectF::new(1400.0, 120.0, 380.0, 176.0);
        memory.record_leaving(WindowShape::Mini, moved_mini);
        let back_to_wide = transition_target(
            moved_mini,
            WindowShape::Mini,
            WindowShape::Window,
            (1280.0, 840.0),
            work,
            memory.wide_bounds,
            memory.compact_origin,
        );
        assert_eq!(back_to_wide, wide);

        let back_to_lyrics = transition_target(
            wide,
            WindowShape::Window,
            WindowShape::Lyrics,
            (1280.0, 840.0),
            work,
            memory.wide_bounds,
            memory.compact_origin,
        );
        assert_eq!(back_to_lyrics, RectF::new(1400.0, 120.0, 380.0, 650.0));
    }

    #[test]
    fn compact_transition_fades_chrome_without_hiding_cover() {
        let from = RectF::new(100.0, 100.0, 380.0, 176.0);
        let to = RectF::new(100.0, 100.0, 380.0, 650.0);
        let mut tr = ModeTransition::new(WindowShape::Mini, WindowShape::Lyrics, from, to);

        let start = tr.step(1000.0, false);
        assert_eq!(start.content_alpha, 1.0);
        assert_eq!(start.render_mode, Mode::Mini);
        assert!(!start.finished);

        let fade_old = tr.step(1040.0, false);
        assert_eq!(fade_old.content_alpha, 1.0);
        assert_eq!(fade_old.render_mode, Mode::Mini);
        assert!(fade_old.chrome_alpha < 0.7);

        let mid = tr.step(1190.0, false);
        assert_eq!(mid.content_alpha, 1.0);
        assert_eq!(mid.render_mode, Mode::Lyrics);
        let b = mid.bounds.unwrap();
        assert!((b.h - 413.0).abs() < 1.0);
        assert_eq!(mid.chrome_alpha, 0.0);

        let fade_in = tr.step(1340.0, false);
        assert_eq!(fade_in.render_mode, Mode::Lyrics);
        assert!(fade_in.chrome_alpha > 0.0 && fade_in.chrome_alpha < 1.0);

        let end = tr.step(1380.0, false);
        assert!(end.finished);
        assert_eq!(end.bounds, Some(to));
        assert_eq!(end.chrome_alpha, 1.0);
        assert_eq!(end.render_mode, Mode::Lyrics);
    }

    #[test]
    fn window_compact_transition_hides_content_while_morphing() {
        let from = RectF::new(100.0, 80.0, 1280.0, 840.0);
        let to = RectF::new(550.0, 175.0, 380.0, 650.0);
        let mut tr = ModeTransition::new(WindowShape::Window, WindowShape::Lyrics, from, to);

        let s0 = tr.step(0.0, false);
        assert_eq!(s0.content_alpha, 1.0);
        assert!(s0.bounds.is_none());

        let morph = tr.step(
            CROSS_FADE_OUT_MS as f64 + CROSS_MORPH_MS as f64 * 0.5,
            false,
        );
        assert_eq!(morph.content_alpha, 0.0);
        let b = morph.bounds.unwrap();
        assert!(b.w < 1280.0 && b.w > 380.0);

        let fade_in = tr.step(
            (CROSS_FADE_OUT_MS + CROSS_MORPH_MS) as f64 + CROSS_FADE_IN_MS as f64 * 0.5,
            false,
        );
        assert_eq!(fade_in.render_mode, Mode::Lyrics);
        assert!(fade_in.content_alpha > 0.1 && fade_in.content_alpha < 0.9);

        let done = tr.step(
            (CROSS_FADE_OUT_MS + CROSS_MORPH_MS + CROSS_FADE_IN_MS) as f64,
            false,
        );
        assert!(done.finished);
        assert_eq!(done.content_alpha, 1.0);
    }

    #[test]
    fn fullscreen_transition_morphs_then_triggers_toggle_once() {
        let wide = RectF::new(100.0, 80.0, 1280.0, 840.0);
        let full = RectF::new(0.0, 0.0, 1920.0, 1080.0);
        let mut tr = ModeTransition::new(WindowShape::Window, WindowShape::Fullscreen, wide, full);

        let s0 = tr.step(0.0, false);
        assert!(!s0.trigger_fullscreen_toggle);
        assert_eq!(s0.content_alpha, 1.0);

        let morph = tr.step(FULLSCREEN_FADE_OUT_MS as f64 + CROSS_MORPH_MS as f64 * 0.5, false);
        assert!(!morph.trigger_fullscreen_toggle);
        assert_eq!(morph.content_alpha, 0.0);
        let mb = morph.bounds.unwrap();
        assert!(mb.w > 1280.0 && mb.w < 1920.0);

        let trigger_t = (FULLSCREEN_FADE_OUT_MS + CROSS_MORPH_MS) as f64;
        let trigger = tr.step(trigger_t, false);
        assert!(trigger.trigger_fullscreen_toggle);
        assert!(trigger.waiting_fullscreen);
        assert_eq!(trigger.content_alpha, 0.0);

        let waiting = tr.step(trigger_t + 16.0, false);
        assert!(!waiting.trigger_fullscreen_toggle);
        assert!(waiting.waiting_fullscreen);

        let settled = tr.step(trigger_t + 32.0, true);
        assert!(!settled.waiting_fullscreen);
        assert!(!settled.finished);

        let done = tr.step(trigger_t + 32.0 + FULLSCREEN_FADE_IN_MS as f64, true);
        assert!(done.finished);
        assert_eq!(done.content_alpha, 1.0);
    }

    #[test]
    fn midflight_retarget_stays_within_start_and_target_bounds() {
        let mini = RectF::new(100.0, 200.0, 380.0, 176.0);
        let lyrics = RectF::new(100.0, 200.0, 380.0, 650.0);
        let mut tr = ModeTransition::new(WindowShape::Mini, WindowShape::Lyrics, mini, lyrics);
        let _ = tr.step(0.0, false);
        let mid = tr.step(190.0, false).bounds.unwrap();
        assert!(mid.h > 300.0 && mid.h < 500.0);

        let mut reverse = ModeTransition::new(WindowShape::Lyrics, WindowShape::Mini, mid, mini);
        let s0 = reverse.step(0.0, false).bounds.unwrap();
        assert_eq!(s0, mid);
        let s1 = reverse.step(COMPACT_MORPH_MS as f64, false);
        assert!(s1.finished);
        assert_eq!(s1.bounds, Some(mini));
    }

    #[test]
    fn fullscreen_exit_to_all_shapes_shrinks_smoothly_before_toggle() {
        let full = RectF::new(0.0, 0.0, 1920.0, 1080.0);
        let targets = [
            (WindowShape::Window, RectF::new(320.0, 100.0, 1280.0, 840.0), Mode::Window),
            (WindowShape::Lyrics, RectF::new(770.0, 195.0, 380.0, 650.0), Mode::Lyrics),
            (WindowShape::Mini, RectF::new(770.0, 432.0, 380.0, 176.0), Mode::Mini),
        ];
        for (shape, target, expected_mode) in targets {
            let mut tr = ModeTransition::new(WindowShape::Fullscreen, shape, full, target);
            assert!(tr.involves_fullscreen());

            let s0 = tr.step(0.0, true);
            assert!(!s0.trigger_fullscreen_toggle);
            assert!(s0.bounds.is_none());
            assert!(tr.render_fullscreen(true, 0.0));

            let mid_t = FULLSCREEN_FADE_OUT_MS as f64 + CROSS_MORPH_MS as f64 * 0.5;
            let mid = tr.step(mid_t, true);
            assert!(!mid.trigger_fullscreen_toggle);
            assert_eq!(mid.render_mode, expected_mode);
            assert!(!tr.render_fullscreen(true, mid_t));
            let b = mid.bounds.unwrap();
            assert!(b.w < 1920.0 && b.w > target.w);
            assert!(b.h < 1080.0 && b.h > target.h);

            let trigger_t = (FULLSCREEN_FADE_OUT_MS + CROSS_MORPH_MS) as f64;
            let trigger = tr.step(trigger_t, true);
            assert!(trigger.trigger_fullscreen_toggle);
            assert!(trigger.waiting_fullscreen);
            assert_eq!(trigger.bounds, Some(target));

            let settled = tr.step(trigger_t + 16.0, false);
            assert!(!settled.waiting_fullscreen);
            assert!(!settled.finished);

            let done = tr.step(trigger_t + 16.0 + FULLSCREEN_FADE_IN_MS as f64, false);
            assert!(done.finished);
            assert_eq!(done.content_alpha, 1.0);
        }
    }
}
