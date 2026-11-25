//! Interlude three-dot indicator animation state machine, ported 1:1 from AMLL `InterludeDotsBase`.

use std::f64::consts::PI;

use crate::easing::BezierEasing;
use crate::scroll::{Spring, SpringParams};

const DOT3_TRAILING_MS: f64 = 750.0;
const EXIT_PHASE1_MS: f64 = 750.0;
const EXIT_PHASE2_MS: f64 = 250.0;
const EXIT_TOTAL_MS: f64 = 1000.0;
const EXIT_FADE_MS: f64 = 250.0;
const DISMISS_FADE_MS: f64 = 150.0;
const ENTER_HOLD_MS: f64 = 500.0;
const ENTER_FADE_MS: f64 = 180.0;
const DOT_ENTER_FADE_MS: f64 = 750.0;
const DOT_ENTER_STAGGER_MS: f64 = 80.0;
const DOT_ENTER_TOTAL_MS: f64 = 910.0;
const BREATHE_BASE_PERIOD_MS: f64 = 4000.0;
const FALLBACK_HOLD_THRESHOLD_MS: f64 = 3000.0;
const BREATHE_MAX_SCALE: f64 = 1.25;
const DOT_INACTIVE_OPACITY: f64 = 0.2;
const DOT_COUNT: f64 = 3.0;

const LIGHTING_EASING: BezierEasing = BezierEasing::new(0.56, 0.01, 0.45, 1.0);
const ENTER_FADE_EASING: BezierEasing = BezierEasing::new(0.59, 0.02, 0.07, 1.0);
const EXIT_PHASE1_EASING: BezierEasing = BezierEasing::new(0.14, 0.06, 0.25, 1.0);
const EXIT_PHASE2_EASING: BezierEasing = BezierEasing::new(0.29, 0.03, 1.0, 0.38);
const EXIT_FADE_EASING: BezierEasing = BezierEasing::new(0.43, 0.08, 0.83, 0.31);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Performing,
    Fading,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Breathe,
    FallbackHold,
}

/// Visual state for the three interlude dots on one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InterludeSnapshot {
    pub is_active: bool,
    pub dot_opacities: [f64; 3],
    pub scale: f64,
    pub opacity: f64,
}

impl Default for InterludeSnapshot {
    fn default() -> Self {
        Self {
            is_active: false,
            dot_opacities: [0.0; 3],
            scale: 1.0,
            opacity: 0.0,
        }
    }
}

/// Port of AMLL `InterludeDotsBase` (`src/lyric-player/base/interlude-dots.ts`).
#[derive(Clone, Debug)]
pub struct InterludeDots {
    left: f64,
    top: f64,
    pos_y: Spring,
    should_snap_pos_y: bool,
    current_ms: f64,
    playing: bool,
    phase: Phase,
    mode: Mode,
    fade_elapsed_ms: f64,
    fade_initial_opacity: f64,
    interval: Option<(f64, f64)>,
    anchor_ms: f64,
    delay_end_ms: f64,
    body_end_ms: f64,
    total_end_ms: f64,
    breathe_period_ms: f64,
    segment_ms: f64,
    dot3_duration_ms: f64,
    dot3_target: f64,
    snapshot: InterludeSnapshot,
}

impl Default for InterludeDots {
    fn default() -> Self {
        Self::new()
    }
}

impl InterludeDots {
    pub fn new() -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            pos_y: Spring::new(0.0),
            should_snap_pos_y: true,
            current_ms: 0.0,
            playing: true,
            phase: Phase::Idle,
            mode: Mode::Breathe,
            fade_elapsed_ms: 0.0,
            fade_initial_opacity: 0.0,
            interval: None,
            anchor_ms: 0.0,
            delay_end_ms: 0.0,
            body_end_ms: 0.0,
            total_end_ms: 0.0,
            breathe_period_ms: BREATHE_BASE_PERIOD_MS,
            segment_ms: 0.0,
            dot3_duration_ms: 0.0,
            dot3_target: 0.0,
            snapshot: InterludeSnapshot::default(),
        }
    }

    pub fn set_playing(&mut self, playing: bool) {
        if playing {
            self.resume();
        } else {
            self.pause();
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
        if self.phase == Phase::Fading {
            self.hide_performance();
        }
    }

    pub fn resume(&mut self) {
        self.playing = true;
    }

    pub fn sync_clock(&mut self, time_ms: f64) {
        self.current_ms = time_ms;
    }

    pub fn update_spring_params(&mut self, params: SpringParams) {
        self.pos_y.update_params(params);
    }

    pub fn set_interlude(
        &mut self,
        start_ms: f64,
        end_ms: f64,
        current_ms: f64,
        force_reset: bool,
        anchor_line_index: isize,
    ) -> bool {
        let is_same = self.interval == Some((start_ms, end_ms));
        if !force_reset && is_same && self.phase != Phase::Fading {
            return self.phase == Phase::Performing;
        }
        self.cancel_fade_out();
        self.interval = Some((start_ms, end_ms));
        self.current_ms = current_ms;
        if force_reset {
            self.anchor_ms = current_ms;
            self.delay_end_ms = ENTER_HOLD_MS;
        } else {
            self.anchor_ms = start_ms;
            self.delay_end_ms = if anchor_line_index == -1 {
                0.0
            } else {
                ENTER_HOLD_MS
            };
        }
        let body_ms = (end_ms - self.anchor_ms).max(0.0) - self.delay_end_ms - EXIT_TOTAL_MS;
        if body_ms < DOT_ENTER_TOTAL_MS {
            self.hide_performance();
            return false;
        }
        self.body_end_ms = self.delay_end_ms + body_ms;
        self.total_end_ms = self.body_end_ms + EXIT_TOTAL_MS;
        if body_ms < FALLBACK_HOLD_THRESHOLD_MS {
            self.mode = Mode::FallbackHold;
            self.dot3_target = 1.0;
        } else {
            self.mode = Mode::Breathe;
            let breathe_cycles = (body_ms / BREATHE_BASE_PERIOD_MS).floor().max(1.0);
            self.breathe_period_ms = body_ms / breathe_cycles;
            self.segment_ms = ((body_ms + DOT3_TRAILING_MS) / DOT_COUNT).round();
            self.dot3_duration_ms = body_ms - self.segment_ms * 2.0;
            self.dot3_target = if self.segment_ms > 0.0 {
                self.dot3_duration_ms / self.segment_ms
            } else {
                1.0
            };
        }
        self.phase = Phase::Performing;
        self.should_snap_pos_y = true;
        true
    }

    pub fn clear_interlude(&mut self, immediate: bool) {
        self.interval = None;
        self.dismiss(immediate);
    }

    pub fn dismiss(&mut self, immediate: bool) {
        if self.phase == Phase::Idle {
            return;
        }
        if immediate || !self.playing {
            self.cancel_fade_out();
            self.hide_performance();
            return;
        }
        if self.phase == Phase::Fading {
            return;
        }
        if self.snapshot.opacity <= 0.0 {
            self.hide_performance();
            return;
        }
        self.fade_elapsed_ms = 0.0;
        self.fade_initial_opacity = self.snapshot.opacity;
        self.phase = Phase::Fading;
    }

    pub fn set_transform(&mut self, left: f64, top: f64, immediate: bool, params: SpringParams) {
        self.left = left;
        self.top = top;
        if self.pos_y.params() != params {
            self.pos_y.update_params(params);
        }
        if !immediate && self.phase != Phase::Idle && !self.should_snap_pos_y {
            self.pos_y.set_target_position(top);
        } else {
            self.should_snap_pos_y = false;
            self.pos_y.set_position(top);
        }
    }

    pub fn update(&mut self, delta_ms: f64) {
        if self.phase == Phase::Idle {
            return;
        }
        if self.phase == Phase::Fading {
            self.fade_elapsed_ms += delta_ms.max(0.0);
            let progress = (self.fade_elapsed_ms / DISMISS_FADE_MS).clamp(0.0, 1.0);
            if progress >= 1.0 {
                self.hide_performance();
                return;
            }
            self.snapshot.opacity =
                self.fade_initial_opacity * (1.0 - EXIT_FADE_EASING.eval(progress));
            self.snapshot.is_active = true;
            self.pos_y.update(delta_ms.clamp(0.0, 100.0) / 1000.0);
            return;
        }
        let elapsed_ms = (self.current_ms - self.anchor_ms).max(0.0);
        self.snapshot = self.resolve_snapshot(elapsed_ms);
        self.pos_y.update(delta_ms.clamp(0.0, 100.0) / 1000.0);
        if !self.snapshot.is_active {
            self.phase = Phase::Idle;
        }
    }

    /// Returns `(snapshot, left, current_y)` while the indicator is visible.
    pub fn visible_snapshot(&self) -> Option<(InterludeSnapshot, f64, f64)> {
        if self.phase == Phase::Idle || !self.snapshot.is_active || self.snapshot.opacity <= 0.0 {
            return None;
        }
        Some((self.snapshot, self.left, self.pos_y.position()))
    }

    pub fn pending(&self) -> bool {
        self.phase == Phase::Fading
            || (self.phase == Phase::Performing && (!self.pos_y.arrived() || self.playing))
    }

    pub fn pos_y(&self) -> f64 {
        self.pos_y.position()
    }

    fn cancel_fade_out(&mut self) {
        if self.phase == Phase::Fading {
            self.phase = Phase::Idle;
        }
    }

    fn hide_performance(&mut self) {
        self.phase = Phase::Idle;
        self.snapshot = InterludeSnapshot::default();
    }

    fn resolve_snapshot(&self, elapsed_ms: f64) -> InterludeSnapshot {
        if elapsed_ms >= self.total_end_ms {
            return InterludeSnapshot::default();
        }
        if elapsed_ms < self.delay_end_ms {
            return InterludeSnapshot {
                is_active: true,
                dot_opacities: [0.0; 3],
                scale: 1.0,
                opacity: 0.0,
            };
        }
        let internal_ms = elapsed_ms - self.delay_end_ms;
        if elapsed_ms >= self.body_end_ms {
            let exit_elapsed = elapsed_ms - self.body_end_ms;
            let fade_t = ((exit_elapsed - 750.0) / EXIT_FADE_MS).clamp(0.0, 1.0);
            let opacity = enter_opacity(internal_ms) * (1.0 - EXIT_FADE_EASING.eval(fade_t));
            let scale = if exit_elapsed < EXIT_PHASE1_MS {
                1.0 + EXIT_PHASE1_EASING.eval(exit_elapsed / EXIT_PHASE1_MS) * 0.25
            } else {
                let phase2_t = ((exit_elapsed - EXIT_PHASE1_MS) / EXIT_PHASE2_MS).clamp(0.0, 1.0);
                BREATHE_MAX_SCALE - EXIT_PHASE2_EASING.eval(phase2_t) * 0.85
            };
            let trailing = (exit_elapsed / DOT3_TRAILING_MS).clamp(0.0, 1.0);
            return InterludeSnapshot {
                is_active: true,
                dot_opacities: write_dot_opacities(
                    internal_ms,
                    [
                        1.0,
                        1.0,
                        self.dot3_target + (1.0 - self.dot3_target) * trailing,
                    ],
                ),
                scale,
                opacity,
            };
        }
        let opacity = enter_opacity(internal_ms);
        if self.mode == Mode::FallbackHold {
            return InterludeSnapshot {
                is_active: true,
                dot_opacities: write_dot_opacities(internal_ms, [1.0, 1.0, 1.0]),
                scale: 1.0,
                opacity,
            };
        }
        let phase_t = (internal_ms % self.breathe_period_ms) / self.breathe_period_ms;
        let progress = breathing_progress(phase_t);
        // Smooth cosine wave eliminates the sharp peak/trough derivative flip.
        let scale = 1.0 + 0.5 * (BREATHE_MAX_SCALE - 1.0) * (1.0 - (2.0 * PI * progress).cos());
        InterludeSnapshot {
            is_active: true,
            dot_opacities: write_dot_opacities(
                internal_ms,
                [
                    compute_dot_fraction(0.0, self.segment_ms, internal_ms, 1.0),
                    compute_dot_fraction(self.segment_ms, self.segment_ms, internal_ms, 1.0),
                    compute_dot_fraction(
                        self.segment_ms * 2.0,
                        self.dot3_duration_ms,
                        internal_ms,
                        self.dot3_target,
                    ),
                ],
            ),
            scale,
            opacity,
        }
    }
}

fn breathing_progress(t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let angle = 4.0 * PI * t;
    let s = angle.sin();
    let c = angle.cos();
    t - 0.084 * s + 0.008 * (1.0 - c) + 0.0046 * s * (c - s)
}

fn dot_opacity(fraction: f64) -> f64 {
    DOT_INACTIVE_OPACITY + 0.7 * fraction.clamp(0.0, 1.0)
}

fn enter_opacity(internal_ms: f64) -> f64 {
    ENTER_FADE_EASING.eval((internal_ms / ENTER_FADE_MS).clamp(0.0, 1.0))
}

fn dot_enter_alpha(index: usize, internal_ms: f64) -> f64 {
    let t = ((internal_ms - index as f64 * DOT_ENTER_STAGGER_MS) / DOT_ENTER_FADE_MS).clamp(0.0, 1.0);
    t * t
}

fn write_dot_opacities(internal_ms: f64, fractions: [f64; 3]) -> [f64; 3] {
    [
        dot_opacity(fractions[0]) * dot_enter_alpha(0, internal_ms),
        dot_opacity(fractions[1]) * dot_enter_alpha(1, internal_ms),
        dot_opacity(fractions[2]) * dot_enter_alpha(2, internal_ms),
    ]
}

fn compute_dot_fraction(start_delay: f64, duration: f64, internal_ms: f64, target: f64) -> f64 {
    if internal_ms <= start_delay || duration <= 0.0 {
        return 0.0;
    }
    let local_t = ((internal_ms - start_delay) / duration).clamp(0.0, 1.0);
    LIGHTING_EASING.eval(local_t) * target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_short_remaining_interlude_cannot_display() {
        let mut dots = InterludeDots::new();
        assert!(!dots.set_interlude(1000.0, 8000.0, 6000.0, true, 0));
        assert!(dots.visible_snapshot().is_none());
    }

    #[test]
    fn leading_intro_starts_without_hold_and_lights_dots_in_order() {
        let mut dots = InterludeDots::new();
        assert!(dots.set_interlude(0.0, 10_000.0, 0.0, false, -1));
        dots.sync_clock(1_000.0);
        dots.update(16.0);
        let (snap, _, _) = dots.visible_snapshot().expect("visible during intro");
        assert!(snap.opacity > 0.99);
        assert!(snap.dot_opacities[0] > snap.dot_opacities[1]);
        assert!(snap.dot_opacities[1] >= snap.dot_opacities[2]);
        assert!(snap.scale > 1.0 && snap.scale <= 1.25);
    }

    #[test]
    fn mid_song_interlude_waits_hold_and_exits_gracefully() {
        let mut dots = InterludeDots::new();
        assert!(dots.set_interlude(10_000.0, 20_000.0, 10_000.0, false, 3));
        dots.sync_clock(10_200.0);
        dots.update(16.0);
        assert!(
            dots.visible_snapshot().is_none(),
            "hidden during 500ms enter hold"
        );

        dots.sync_clock(19_950.0);
        dots.update(16.0);
        let (exit_snap, _, _) = dots.visible_snapshot().expect("visible during exit phase");
        assert!(exit_snap.scale < 1.0, "phase2 shrinks below 1.0: {}", exit_snap.scale);
        assert!(exit_snap.opacity < 1.0);

        dots.clear_interlude(false);
        dots.update(160.0);
        assert!(dots.visible_snapshot().is_none(), "faded out after 150ms");
    }

    #[test]
    fn short_body_uses_fallback_hold_without_breathing() {
        let mut dots = InterludeDots::new();
        // 500ms hold + 2000ms body + 1000ms exit = 3500ms span (>= 910ms body, < 3000ms body).
        assert!(dots.set_interlude(10_000.0, 13_500.0, 10_000.0, false, 0));
        dots.sync_clock(11_500.0);
        dots.update(16.0);
        let (snap, _, _) = dots.visible_snapshot().unwrap();
        assert_eq!(snap.scale, 1.0);
    }
}
