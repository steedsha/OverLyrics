//! The background scene ring: warm-up, swap and the 550 ms cross-fade.
//!
//! This is the state machine half of `renderer.ts` (commit 708f6b1). It is kept
//! separate from the GPU passes so the ordering rules can be tested without a
//! device: they are where the visible regressions live (a cover that appears
//! half-settled, a fade that restarts, a stale scene that keeps drawing).
//!
//! The reference keeps at most two visible scenes plus one bounded pending
//! cover:
//!
//! ```text
//!   pending --(2700 warm steps, 90 per tick)--> current --(550 ms)--> previous
//! ```
//!
//! The warm-up is not cosmetic. Each simulation step redraws the source through
//! a rotating layer pass into `acc`, and pixels the layer leaves out keep
//! whatever `acc` held before. That smear is the feedback blur the effect is
//! built on, so a freshly decoded cover must be stepped 2700 times before it is
//! shown; otherwise the first visible frame would be a different picture than
//! every later frame.

/// Cross-fade duration between two scenes, in milliseconds.
pub const FADE_MS: f64 = 800.0;

/// Simulation steps a new scene needs before it may be shown.
pub const WARM_STEPS: u32 = 2700;

/// Upper bound on warm-up steps in one tick, so a stalled frame cannot block
/// the UI for a whole warm-up's worth of draws.
pub const MAX_WARM_PER_TICK: u32 = 90;

/// One cover's scene state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneSlot {
    /// The background clock time the scene was created at.
    pub time: f64,
    /// Warm-up steps still owed before the scene may be shown.
    pub warm: u32,
}

impl SceneSlot {
    pub fn new(time: f64) -> Self {
        Self {
            time,
            warm: WARM_STEPS,
        }
    }
}

/// The steps a tick must execute, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FramePlan {
    /// Warm-up steps for the pending scene: `(start, count)`.
    ///
    /// The k-th of those steps runs at `start + k / 60.0`, because the
    /// reference decrements `pending.warm` *inside* its loop:
    ///
    /// ```text
    ///   for (i = 0; i < count; i++) { step(pending, pending.time - (pending.warm - 1) / 60); pending.warm--; }
    /// ```
    ///
    /// so every iteration sees a time one 60 Hz tick later than the previous
    /// one. Stepping the whole burst at a single time would hold the rotation
    /// still for 90 frames and change the picture the warm-up converges on.
    pub warmup: Option<(f64, u32)>,
    /// Simulation steps for the current scene.
    pub steps: Vec<f64>,
    /// Whether this tick swapped the pending scene in.
    pub swapped: bool,
    /// What to draw this frame; `None` means the window has no scene yet.
    pub present: Option<PresentPlan>,
}

/// The final composite for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresentPlan {
    /// Opacity the outgoing scene is drawn with, when a cross-fade is running.
    pub previous: Option<f64>,
    /// Opacity the incoming (current) scene is drawn with.
    pub current: f64,
}

/// The scene ring.
#[derive(Debug, Default)]
pub struct BackgroundScene {
    current: Option<SceneSlot>,
    previous: Option<SceneSlot>,
    pending: Option<SceneSlot>,
    fade_start: Option<f64>,
    time: f64,
}

impl BackgroundScene {
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs a new cover. Any earlier pending scene is dropped before its
    /// warm-up finished, which is what keeps rapid track skips from stacking
    /// GPU work.
    pub fn set_artwork(&mut self, time: f64) {
        self.pending = Some(SceneSlot::new(time));
    }

    /// Drops every scene, as `renderer.clear()` does.
    pub fn clear(&mut self) {
        self.current = None;
        self.previous = None;
        self.pending = None;
        self.fade_start = None;
    }

    /// True while a cross-fade or a warm-up still needs frames.
    pub fn has_work(&self) -> bool {
        self.pending.is_some() || self.previous.is_some()
    }

    /// True once a scene is available to draw.
    pub fn is_ready(&self) -> bool {
        self.current.is_some()
    }

    /// The current scene, for the GPU pass.
    pub fn current(&self) -> Option<SceneSlot> {
        self.current
    }

    /// The pending scene, for the GPU pass's warm-up steps.
    pub fn pending(&self) -> Option<SceneSlot> {
        self.pending
    }

    /// The background clock time last observed.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Computes the composite opacities for the active scene(s) at `now`.
    pub fn present_plan(&self, now: f64) -> Option<PresentPlan> {
        self.current?;
        let fade_start = self.fade_start.unwrap_or(0.0);
        let alpha = ((now - fade_start) / FADE_MS).clamp(0.0, 1.0);
        if self.previous.is_some() && alpha < 1.0 {
            Some(PresentPlan {
                previous: Some(1.0),
                current: alpha,
            })
        } else {
            Some(PresentPlan {
                previous: None,
                current: 1.0,
            })
        }
    }

    /// Advances one display frame.
    ///
    /// `times` are the simulation steps the fixed 60 Hz clock earned this
    /// frame; `now` is the display timestamp used for the cross-fade.
    pub fn tick(&mut self, times: &[f64], now: f64) -> FramePlan {
        let mut plan = FramePlan::default();

        if let Some(pending) = self.pending.as_mut() {
            let count = pending.warm.min(MAX_WARM_PER_TICK);
            if count > 0 {
                // The burst starts at the oldest owed time; the caller advances
                // it by 1/60 per step (`FramePlan::warmup`).
                let at = pending.time - (pending.warm as f64 - 1.0) / 60.0;
                plan.warmup = Some((at, count));
                pending.warm -= count;
            }
            if pending.warm == 0 {
                self.previous = self.current.take();
                self.current = self.pending.take();
                self.fade_start = Some(now);
                plan.swapped = true;
            }
        }

        if let Some(last) = times.last() {
            self.time = *last;
        }

        if self.current.is_some() {
            if plan.swapped {
                plan.steps.push(self.time);
            }
            plan.steps.extend_from_slice(times);
        }

        let Some(current) = self.current else {
            return plan;
        };
        let _ = current;

        let fade_start = self.fade_start.unwrap_or(0.0);
        let t = ((now - fade_start) / FADE_MS).clamp(0.0, 1.0);
        let alpha = t * t * (3.0 - 2.0 * t);
        if self.previous.is_some() && t < 1.0 {
            plan.present = Some(PresentPlan {
                previous: Some(1.0),
                current: alpha,
            });
        } else {
            self.previous = None;
            plan.present = Some(PresentPlan {
                previous: None,
                current: 1.0,
            });
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ticks until the pending scene is swapped in, returning the tick count.
    fn settle(scene: &mut BackgroundScene, start_ms: f64) -> u32 {
        let mut ticks = 0;
        while !scene.is_ready() {
            scene.tick(&[], start_ms + f64::from(ticks) * 16.0);
            ticks += 1;
            assert!(ticks < 100, "warm-up never finished");
        }
        ticks
    }

    #[test]
    fn a_new_cover_takes_thirty_ticks_to_warm_and_never_more_than_ninety_steps_each() {
        let mut scene = BackgroundScene::new();
        scene.set_artwork(0.0);
        assert!(!scene.is_ready());
        let plan = scene.tick(&[], 0.0);
        let (_, count) = plan.warmup.expect("pending scene warms");
        assert_eq!(count, MAX_WARM_PER_TICK);
        assert!(!plan.swapped);
        assert!(plan.present.is_none());

        let mut scene = BackgroundScene::new();
        scene.set_artwork(0.0);
        let ticks = settle(&mut scene, 0.0);
        assert_eq!(ticks, WARM_STEPS.div_ceil(MAX_WARM_PER_TICK));
    }

    #[test]
    fn the_swap_starts_the_fade_from_zero_and_draws_the_old_scene_under_it() {
        let mut scene = BackgroundScene::new();
        scene.set_artwork(0.0);
        settle(&mut scene, 0.0);
        scene.tick(&[0.0], 1000.0);
        // A second cover arrives while the first is fully faded in.
        scene.set_artwork(1.0);
        let mut now = 1016.0;
        while !scene.has_work() == false {
            let plan = scene.tick(&[], now);
            if plan.swapped {
                let present = plan.present.expect("a swapped scene presents");
                assert_eq!(present.current, 0.0);
                assert_eq!(present.previous, Some(1.0));
                assert_eq!(plan.steps, vec![scene.time()]);
                break;
            }
            now += 16.0;
        }
    }

    #[test]
    fn the_cross_fade_is_monotonic_and_ends_at_full_opacity() {
        let mut scene = BackgroundScene::new();
        scene.set_artwork(0.0);
        settle(&mut scene, 0.0);
        scene.tick(&[0.0], 500.0);
        scene.set_artwork(1.0);
        let mut now = 516.0;
        loop {
            let plan = scene.tick(&[], now);
            if plan.swapped {
                break;
            }
            now += 16.0;
        }
        let swap_at = now;
        let mut last = -1.0;
        for step in 0..=55 {
            let plan = scene.tick(&[], swap_at + f64::from(step) * 16.0);
            let present = plan.present.expect("a ready scene always presents");
            assert!(present.current >= last, "opacity went backwards");
            last = present.current;
        }
        assert_eq!(last, 1.0);
    }

    #[test]
    fn a_second_cover_replaces_the_pending_one_without_touching_the_screen() {
        let mut scene = BackgroundScene::new();
        scene.set_artwork(0.0);
        settle(&mut scene, 0.0);
        scene.tick(&[0.0], 100.0);
        scene.set_artwork(1.0);
        scene.tick(&[], 116.0);
        let before = scene.pending().expect("pending scene").warm;
        scene.set_artwork(2.0);
        assert_eq!(scene.pending().expect("new pending").warm, WARM_STEPS);
        assert!(before < WARM_STEPS);
        // The current scene kept drawing throughout.
        let plan = scene.tick(&[], 132.0);
        assert_eq!(plan.present.expect("still presenting").current, 1.0);
    }

    #[test]
    fn clearing_drops_everything_and_stops_presenting() {
        let mut scene = BackgroundScene::new();
        scene.set_artwork(0.0);
        settle(&mut scene, 0.0);
        assert!(scene.is_ready());
        scene.clear();
        assert!(!scene.is_ready());
        assert!(!scene.has_work());
        assert!(scene.tick(&[1.0], 2000.0).present.is_none());
    }

    #[test]
    fn warm_up_steps_ramp_at_sixty_hertz_instead_of_repeating_one_time() {
        let mut scene = BackgroundScene::new();
        scene.set_artwork(0.0);
        let (start, count) = scene.tick(&[], 0.0).warmup.expect("pending scene warms");
        assert_eq!(count, MAX_WARM_PER_TICK);
        // The reference's first step uses `time - (2700 - 1) / 60`.
        assert!((start - -(2699.0 / 60.0)).abs() < 1e-9);
        let mut now = 16.0;
        let (next, _) = scene.tick(&[], now).warmup.expect("second burst");
        // After 90 steps the counter is 2610, so the next burst starts 90
        // sixtieths later — one step per sixtieth, not one per tick.
        assert!((next - (start + 90.0 / 60.0)).abs() < 1e-9);
        now += 16.0;
        let (third, _) = scene.tick(&[], now).warmup.expect("third burst");
        assert!((third - (start + 180.0 / 60.0)).abs() < 1e-9);
    }

    #[test]
    fn the_simulation_time_is_the_last_earned_step() {
        let mut scene = BackgroundScene::new();
        scene.set_artwork(0.0);
        settle(&mut scene, 0.0);
        let plan = scene.tick(&[1.0, 1.0166666666666666], 100.0);
        assert_eq!(scene.time(), 1.0166666666666666);
        assert_eq!(plan.steps.len(), 2);
    }
}
