//! The background's own clock and its latest-request-wins cover loader.
//!
//! Ported from `src/windows/lyrics/background/lifecycle.ts` at commit 708f6b1.
//!
//! The background runs at a fixed 60 Hz simulation rate regardless of the
//! display's refresh rate: the mesh animation, the layer rotation and the
//! feedback blur are defined in 1/60 s steps, so a 240 Hz window takes four
//! display frames per simulation step rather than running the animation four
//! times faster. Everything downstream (the shader's `uAng`, the mesh phase)
//! consumes this stepped time, never the display's frame delta.
//!
//! A long stall must not flood the GPU with catch-up steps: the accumulator is
//! clamped to 50 ms per call and each call emits at most three steps. A hidden
//! window is paused outright, so returning from `document.hidden` does not
//! replay the hidden interval.

/// One simulation step, in milliseconds.
pub const STEP_MS: f64 = 1000.0 / 60.0;

/// Maximum simulation steps a single [`BackgroundClock::advance`] may emit.
pub const MAX_STEPS: usize = 3;

/// Largest frame delta the accumulator accepts, in milliseconds.
pub const MAX_DELTA_MS: f64 = 50.0;

/// Slack that keeps a 60 Hz display from losing a step to floating-point error.
const EPSILON_MS: f64 = 1e-7;

/// Fixed-step 60 Hz clock driven by display timestamps.
#[derive(Clone, Copy, Debug, Default)]
pub struct BackgroundClock {
    previous: Option<f64>,
    accumulator: f64,
    time: f64,
}

impl BackgroundClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one display timestamp and returns the simulation times it earned.
    ///
    /// The first call only establishes the baseline and emits nothing, which is
    /// what stops a freshly opened window from jumping straight to a step.
    pub fn advance(&mut self, now_ms: f64) -> Vec<f64> {
        let Some(previous) = self.previous else {
            self.previous = Some(now_ms);
            return Vec::new();
        };
        let delta = (now_ms - previous).clamp(0.0, MAX_DELTA_MS);
        self.accumulator += delta;
        self.previous = Some(now_ms);

        let mut times = Vec::new();
        while self.accumulator + EPSILON_MS >= STEP_MS && times.len() < MAX_STEPS {
            self.accumulator -= STEP_MS;
            self.time += 1.0 / 60.0;
            times.push(self.time);
        }
        times
    }

    /// Drops the baseline and the pending accumulator. Simulation time is kept:
    /// a window that is hidden and shown again continues the same animation
    /// instead of restarting it.
    pub fn pause(&mut self) {
        self.previous = None;
        self.accumulator = 0.0;
    }

    /// Simulation seconds elapsed so far.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// True when the next [`Self::advance`] call will only set the baseline.
    pub fn is_paused(&self) -> bool {
        self.previous.is_none()
    }
}

/// Latest-request-wins bookkeeping for asynchronous cover loading.
///
/// A cover load is a decode on another thread; by the time it finishes the user
/// may have skipped three tracks. Each request gets a generation and only the
/// current one may publish. `cancel` invalidates the in-flight request without
/// dropping the cover already on screen, which is what the reference does when
/// the window hides.
#[derive(Clone, Copy, Debug, Default)]
pub struct LatestImageRequest {
    generation: u64,
}

impl LatestImageRequest {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a request and returns its token.
    pub fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Whether `token` may still publish.
    pub fn is_current(&self, token: u64) -> bool {
        self.generation == token
    }

    /// Invalidates every outstanding request.
    pub fn cancel(&mut self) {
        self.generation += 1;
    }

    /// The generation most recently handed out.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference test's `run(hz)` helper.
    fn steps_at(hz: u32) -> usize {
        let mut clock = BackgroundClock::new();
        clock.advance(0.0);
        let mut steps = 0;
        for i in 1..=hz * 2 {
            steps += clock.advance(i as f64 * 1000.0 / hz as f64).len();
        }
        steps
    }

    #[test]
    fn the_simulation_rate_is_the_same_on_60hz_and_240hz_displays() {
        assert_eq!(steps_at(60), 120);
        assert_eq!(steps_at(240), 120);
    }

    #[test]
    fn the_first_advance_only_establishes_the_baseline() {
        let mut clock = BackgroundClock::new();
        assert!(clock.advance(1234.0).is_empty());
        assert!(clock.is_paused() == false);
        assert_eq!(clock.time(), 0.0);
    }

    #[test]
    fn a_hidden_window_does_not_replay_the_hidden_interval() {
        let mut clock = BackgroundClock::new();
        clock.advance(0.0);
        clock.advance(100.0);
        clock.pause();
        assert!(clock.advance(10_000.0).is_empty());
        assert_eq!(clock.advance(10_017.0).len(), 1);
        // 50 ms cap: at most three steps even after a one-second stall.
        assert!(clock.advance(100_000.0).len() <= MAX_STEPS);
    }

    #[test]
    fn a_negative_delta_does_not_rewind_the_animation() {
        let mut clock = BackgroundClock::new();
        clock.advance(1000.0);
        assert!(clock.advance(900.0).is_empty());
        // The reference sets `previous = now` unconditionally, so the next
        // delta is measured from the rewound timestamp: 116 ms clamped to
        // 50 ms, exactly three steps.
        assert_eq!(clock.advance(1016.0).len(), MAX_STEPS);
    }

    #[test]
    fn a_stale_request_cannot_publish_and_cancel_keeps_the_screen() {
        let mut requests = LatestImageRequest::new();
        let old = requests.begin();
        let latest = requests.begin();
        assert!(!requests.is_current(old));
        assert!(requests.is_current(latest));
        requests.cancel();
        assert!(!requests.is_current(latest));
    }
}
