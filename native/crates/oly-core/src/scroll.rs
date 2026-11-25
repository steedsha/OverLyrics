//! Spring scroll, manual browsing and the return to the current line.
//!
//! Two mechanisms in the reference are easy to conflate and must not be:
//!
//!   * the **spring** (`posY`) moves each lyric row towards its layout target,
//!     with parameters chosen per playback situation;
//!   * the **scroll offset** is the user's displacement on top of that layout.
//!
//! The spring never sees user input. The offset never springs. A thumb drag is
//! one-to-one with the pixels the pointer travelled, which is why the reference
//! dispatches a synthetic `wheel` event for it instead of driving a second
//! animation. Wrapping the drag in a spring would make the thumb lag the
//! pointer, and the scrollbar position is computed from the same offset, so the
//! two would visibly disagree.
//!
//! Everything here is a pure function of `(inputs, now)`. The reference reads
//! `performance.now()` in five places; taking the timestamp as an argument is
//! what makes the replay tests deterministic, and it is the same substitution
//! the transport port already makes.

/// How long the offset must sit still before automatic alignment may resume.
/// A scroll that ended 400 ms ago is still "the user is looking at something";
/// one that ended a second ago is not.
pub const AUTO_ALIGN_RESUME_DELAY_MS: f64 = 500.0;

/// Debounce that ends a wheel gesture. Continuous wheel events keep pushing it
/// out; this is why a long flick does not return mid-gesture.
pub const WHEEL_IDLE_TIMEOUT_MS: f64 = 150.0;

/// The reference integrates inertia in 60 Hz steps, so the decay factor is
/// expressed per 1/60 s and raised to the number of steps a frame covered.
pub const BASE_FRAME_DURATION_MS: f64 = 1000.0 / 60.0;

/// A frame longer than this is a stall or a tab switch, not motion: drop the
/// step rather than teleporting the content by a quarter second of inertia.
pub const MAX_INERTIA_FRAME_DELTA_MS: f64 = 100.0;

/// Adapter-level browsing timeout. After four seconds without input the view
/// returns to the playing line by itself.
pub const SCROLL_IDLE_TIMEOUT_MS: f64 = 4000.0;

/// Duration of the eased return after browsing ends.
pub const RETURN_DURATION_MS: f64 = 600.0;

/// Touch gestures below this travel are taps, not scrolls.
pub const TOUCH_INTENT_PX: f64 = 10.0;

/// `WheelEvent.DOM_DELTA_LINE` is converted with this factor. The reference
/// hard-codes 50 px, which is close to one lyric row and deliberately coarse.
pub const WHEEL_LINE_PX: f64 = 50.0;

/// Inertia stops below this speed; a slower flick is treated as a stop.
pub const INERTIA_STOP_SPEED: f64 = 0.1;

/// ...and does not start below this one.
pub const INERTIA_START_SPEED: f64 = 0.05;

/// Per-1/60 s velocity decay.
pub const INERTIA_DECAY: f64 = 0.95;

// ---------------------------------------------------------------------------
// Spring
// ---------------------------------------------------------------------------

/// Spring constants. `mass` is present because the reference's solver uses it
/// even though the adapter never changes it from 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringParams {
    pub stiffness: f64,
    pub damping: f64,
    pub mass: f64,
    /// Forces the critically-damped closed form. The reference exposes it but
    /// the adapter never sets it.
    pub soft: bool,
}

impl Default for SpringParams {
    fn default() -> Self {
        // `solveSpring`'s own defaults.
        Self {
            stiffness: 100.0,
            damping: 10.0,
            mass: 1.0,
            soft: false,
        }
    }
}

impl SpringParams {
    pub fn new(stiffness: f64, damping: f64) -> Self {
        Self {
            stiffness,
            damping,
            ..Self::default()
        }
    }
}

/// Slower, steadier parameters used while seeking and during an interlude.
pub const SLOW_STIFFNESS: f64 = 90.0;
pub const SLOW_DAMPING: f64 = 15.0;

/// Medium parameters used once the song has ended.
pub const MEDIUM_STIFFNESS: f64 = 140.0;
pub const MEDIUM_DAMPING: f64 = 22.0;

const MIN_INTERVAL_MS: f64 = 100.0;
const MAX_INTERVAL_MS: f64 = 800.0;
const MIN_STIFFNESS: f64 = 170.0;
const DAMPING_MULTIPLIER: f64 = 2.2;
const INTERVAL_EXPONENT: f64 = 0.2;

/// `getPosYSpringPolicy`.
///
/// Faster songs get a stiffer spring so a row still lands before the next one
/// starts. The exponent flattens the curve: without it, 100 ms and 800 ms rows
/// would need wildly different stiffness and the motion would read as two
/// different animations.
pub fn pos_y_spring_policy(
    is_seeking: bool,
    is_interlude_active: bool,
    interval_ms: Option<f64>,
    is_end_of_song: bool,
) -> SpringParams {
    if is_seeking || is_interlude_active {
        return SpringParams::new(SLOW_STIFFNESS, SLOW_DAMPING);
    }
    if is_end_of_song {
        return SpringParams::new(MEDIUM_STIFFNESS, MEDIUM_DAMPING);
    }
    let Some(interval_ms) = interval_ms else {
        return SpringParams::new(SLOW_STIFFNESS, SLOW_DAMPING);
    };
    let clamped = interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
    let ratio = (1.0 - (clamped - MIN_INTERVAL_MS) / 700.0).powf(INTERVAL_EXPONENT);
    let stiffness = MIN_STIFFNESS + ratio * 50.0;
    SpringParams::new(stiffness, stiffness.sqrt() * DAMPING_MULTIPLIER)
}

/// The closed form the reference solves for. Keeping the three cases separate
/// (rather than integrating numerically) is what reproduces its exact
/// trajectories: a frame-rate-independent integrator would not.
#[derive(Clone, Copy, Debug)]
enum Solver {
    Constant(f64),
    /// Critically damped or softer: `to - (delta + t*leftover) * e^(t*w)`.
    Soft {
        from: f64,
        to: f64,
        delta: f64,
        leftover: f64,
        angular_frequency: f64,
    },
    /// Under-damped: a decaying cosine with a sine correction for velocity.
    Underdamped {
        from: f64,
        to: f64,
        delta: f64,
        leftover: f64,
        damping_frequency: f64,
        decay: f64,
    },
}

impl Solver {
    fn sample(&self, t: f64) -> f64 {
        match *self {
            Solver::Constant(value) => value,
            Solver::Soft {
                from,
                to,
                delta,
                leftover,
                angular_frequency,
            } => {
                if t < 0.0 {
                    return from;
                }
                to - (delta + t * leftover) * (t * angular_frequency).exp()
            }
            Solver::Underdamped {
                from,
                to,
                delta,
                leftover,
                damping_frequency,
                decay,
            } => {
                if t < 0.0 {
                    return from;
                }
                let angle = t * damping_frequency;
                to - (angle.cos() * delta + angle.sin() * leftover) * (t * decay).exp()
            }
        }
    }
}

fn solve_spring(from: f64, velocity: f64, to: f64, params: SpringParams) -> Solver {
    let SpringParams {
        stiffness,
        damping,
        mass,
        soft,
    } = params;
    let delta = to - from;
    // `1 <= damping / (2 * sqrt(stiffness * mass))` is the critical-damping
    // threshold; above it the oscillating form would produce a negative
    // discriminant.
    if soft || 1.0 <= damping / (2.0 * (stiffness * mass).sqrt()) {
        let angular_frequency = -(stiffness / mass).sqrt();
        let leftover = -angular_frequency * delta - velocity;
        Solver::Soft {
            from,
            to,
            delta,
            leftover,
            angular_frequency,
        }
    } else {
        let damping_frequency = (4.0 * mass * stiffness - damping * damping).sqrt();
        let leftover = (damping * delta - 2.0 * mass * velocity) / damping_frequency;
        Solver::Underdamped {
            from,
            to,
            delta,
            leftover,
            damping_frequency: 0.5 * damping_frequency / mass,
            decay: -(0.5 * damping) / mass,
        }
    }
}

fn derivative<F: Fn(f64) -> f64>(f: F, x: f64) -> f64 {
    // The reference uses a central difference with h = 0.001 and feeds the
    // result back into itself for the second derivative. Reproducing the step
    // size matters for `arrived`, which compares those estimates to 0.01.
    const H: f64 = 0.001;
    (f(x + H) - f(x - H)) / (2.0 * H)
}

/// A scalar spring with the reference's queueing semantics.
///
/// Two delayed queues exist because the adapter uses both: a row can be given a
/// new target with a stagger delay, and the parameters can be swapped with a
/// delay as well. Dropping either would change which frame a row starts moving
/// on, which is visible as rows moving in lockstep during a seek.
#[derive(Clone, Debug)]
pub struct Spring {
    current_position: f64,
    target_position: f64,
    current_time: f64,
    params: SpringParams,
    solver: Solver,
    queued_params: Option<(SpringParams, f64)>,
    queued_position: Option<(f64, f64)>,
}

impl Spring {
    pub fn new(initial: f64) -> Self {
        Self {
            current_position: initial,
            target_position: initial,
            current_time: 0.0,
            params: SpringParams::default(),
            solver: Solver::Constant(initial),
            queued_params: None,
            queued_position: None,
        }
    }

    pub fn position(&self) -> f64 {
        self.current_position
    }

    pub fn target(&self) -> f64 {
        self.target_position
    }

    pub fn params(&self) -> SpringParams {
        self.params
    }

    fn reset_solver(&mut self) {
        let velocity = self.velocity(self.current_time);
        self.current_time = 0.0;
        self.solver = solve_spring(
            self.current_position,
            velocity,
            self.target_position,
            self.params,
        );
    }

    fn velocity(&self, t: f64) -> f64 {
        derivative(|x| self.solver.sample(x), t)
    }

    fn acceleration(&self, t: f64) -> f64 {
        let solver = self.solver;
        derivative(move |x| derivative(|y| solver.sample(y), x), t)
    }

    /// The reference's rest test: position, velocity and acceleration all
    /// within 0.01, and nothing queued. The acceleration term is what stops the
    /// spring from being declared arrived at the top of its arc.
    pub fn arrived(&self) -> bool {
        (self.target_position - self.current_position).abs() < 0.01
            && self.velocity(self.current_time).abs() < 0.01
            && self.acceleration(self.current_time).abs() < 0.01
            && self.queued_params.is_none()
            && self.queued_position.is_none()
    }

    pub fn set_position(&mut self, target: f64) {
        self.target_position = target;
        self.current_position = target;
        self.solver = Solver::Constant(target);
    }

    pub fn update(&mut self, delta_secs: f64) {
        self.current_time += delta_secs;
        self.current_position = self.solver.sample(self.current_time);

        if let Some((params, remaining)) = self.queued_params.as_mut() {
            *remaining -= delta_secs;
            if *remaining <= 0.0 {
                let params = *params;
                self.queued_params = None;
                self.update_params(params);
            }
        }
        if let Some((position, remaining)) = self.queued_position.as_mut() {
            *remaining -= delta_secs;
            if *remaining <= 0.0 {
                let position = *position;
                self.queued_position = None;
                self.set_target_position(position);
            }
        }
        if self.arrived() {
            self.set_position(self.target_position);
        }
    }

    pub fn update_params(&mut self, params: SpringParams) {
        self.queued_position = None;
        self.params = params;
        self.reset_solver();
    }

    pub fn update_params_after(&mut self, params: SpringParams, delay_secs: f64) {
        if delay_secs > 0.0 {
            self.queued_params = Some((params, delay_secs));
        } else {
            self.update_params(params);
        }
    }

    pub fn set_target_position(&mut self, target: f64) {
        if (self.target_position - target).abs() < 0.001 {
            self.queued_position = None;
            return;
        }
        self.queued_position = None;
        self.target_position = target;
        self.reset_solver();
    }

    pub fn set_target_after(&mut self, target: f64, delay_secs: f64) {
        if delay_secs > 0.0 {
            self.queued_position = Some((target, delay_secs));
        } else {
            self.set_target_position(target);
        }
    }
}

// ---------------------------------------------------------------------------
// Scroll offset
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractionKind {
    Wheel,
    Touch,
}

/// Touch bookkeeping, kept because inertia depends on the last two samples
/// rather than on the gesture as a whole.
#[derive(Clone, Copy, Debug, Default)]
struct TouchState {
    start_y: f64,
    start_x: f64,
    last_y: f64,
    start_offset: f64,
    speed: f64,
    last_timestamp: f64,
    intent_confirmed: bool,
}

/// Counters that make "this scroll did not rebuild the world" checkable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollStats {
    pub wheel_events: u64,
    pub touch_starts: u64,
    pub touch_moves: u64,
    /// Times the reference's `onInteractionStart` hook fired. Only a gesture
    /// that begins while none is running announces itself; a second wheel notch
    /// inside the debounce is the same gesture and stays silent.
    pub start_events: u64,
    /// Times `onScrollUpdate` fired.
    pub update_events: u64,
    /// The subset of updates flagged continuous: touch drags and inertia
    /// frames, which keep the browsing state alive until they stop.
    pub continuous_updates: u64,
    pub inertia_frames: u64,
    pub inertia_started: u64,
    pub boundary_updates: u64,
    pub returns_started: u64,
    pub returns_finished: u64,
    pub resets: u64,
}

/// The single offset model shared by the content and the scrollbar.
///
/// `offset` is added to every row's layout position. Negative means the content
/// is dragged down (the user is looking at earlier lines); positive means it is
/// dragged up.
#[derive(Debug)]
pub struct ScrollEngine {
    offset: f64,
    min_offset: f64,
    max_offset: f64,
    interacting: bool,
    scroll_end_time: Option<f64>,
    wheel_deadline: Option<f64>,
    touch: TouchState,
    inertia_speed: f64,
    inertia_last_frame: Option<f64>,
    stats: ScrollStats,
}

impl Default for ScrollEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollEngine {
    pub fn new() -> Self {
        Self {
            offset: 0.0,
            min_offset: 0.0,
            max_offset: 0.0,
            interacting: false,
            scroll_end_time: None,
            wheel_deadline: None,
            touch: TouchState::default(),
            inertia_speed: 0.0,
            inertia_last_frame: None,
            stats: ScrollStats::default(),
        }
    }

    pub fn offset(&self) -> f64 {
        self.offset
    }

    pub fn min_offset(&self) -> f64 {
        self.min_offset
    }

    pub fn max_offset(&self) -> f64 {
        self.max_offset
    }

    pub fn span(&self) -> f64 {
        self.max_offset - self.min_offset
    }

    pub fn stats(&self) -> ScrollStats {
        self.stats
    }

    pub fn is_interacting(&self) -> bool {
        self.interacting
    }

    pub fn has_inertia(&self) -> bool {
        self.inertia_last_frame.is_some()
    }

    fn clamp(&self, value: f64) -> f64 {
        let lo = self.min_offset.min(self.max_offset);
        let hi = self.min_offset.max(self.max_offset);
        value.clamp(lo, hi)
    }

    /// Set the reachable range and clamp into it.
    ///
    /// `min` is clamped to at most zero and `max` to at least zero, so the
    /// playing position is always reachable even when the content is shorter
    /// than the viewport.
    pub fn update_boundary(&mut self, min: f64, max: f64) -> f64 {
        self.min_offset = min.min(0.0);
        self.max_offset = max.max(0.0);
        self.offset = self.clamp(self.offset);
        self.stats.boundary_updates += 1;
        self.offset
    }

    /// 1:1 displacement, used by the scrollbar thumb. Positive `px` scrolls
    /// towards earlier lines, matching the reference `scrollBy`, whose adapter
    /// turns `px` into `deltaY: -px` before AMLL adds it to the offset.
    /// `Scrollbar.tsx` confirms it: a thumb dragged downwards sends
    /// `-(pointer travel)`, i.e. a negative `px`.
    pub fn scroll_by(&mut self, px: f64) -> f64 {
        if !px.is_finite() || px == 0.0 {
            return self.offset;
        }
        self.offset = self.clamp(self.offset - px);
        self.offset
    }

    fn begin_interaction(&mut self, _kind: InteractionKind) {
        self.scroll_end_time = None;
        if !self.interacting {
            self.interacting = true;
            self.stats.start_events += 1;
        }
    }

    fn end_interaction(&mut self, now: f64) {
        self.interacting = false;
        self.wheel_deadline = None;
        self.scroll_end_time = Some(now);
    }

    /// True once the offset has been still for long enough to resume alignment.
    pub fn can_resume_auto_align(&self, now: f64) -> bool {
        self.scroll_end_time
            .is_some_and(|ended| now - ended >= AUTO_ALIGN_RESUME_DELAY_MS)
    }

    /// A wheel step. `pixel_mode` mirrors `WheelEvent.DOM_DELTA_PIXEL`; other
    /// modes are multiplied by [`WHEEL_LINE_PX`].
    ///
    /// Returns true when this event started a new gesture, which is what the
    /// browsing layer needs in order to clear a running return.
    pub fn wheel(&mut self, delta_y: f64, pixel_mode: bool, now: f64) -> bool {
        let was_interacting = self.interacting;
        self.begin_interaction(InteractionKind::Wheel);
        self.inertia_last_frame = None;
        self.inertia_speed = 0.0;
        let step = if pixel_mode {
            delta_y
        } else {
            delta_y * WHEEL_LINE_PX
        };
        self.offset = self.clamp(self.offset + step);
        self.wheel_deadline = Some(now + WHEEL_IDLE_TIMEOUT_MS);
        self.stats.wheel_events += 1;
        // The reference calls onScrollUpdate(false) for a discrete wheel step.
        self.stats.update_events += 1;
        !was_interacting
    }

    /// First touch sample. Returns whether an inertia run or wheel timeout was
    /// interrupted, which the caller only needs for telemetry.
    pub fn touch_start(&mut self, y: f64, x: f64, now: f64) -> bool {
        let had_inertia = self.inertia_last_frame.is_some();
        let had_timer = self.wheel_deadline.is_some();
        self.inertia_last_frame = None;
        self.inertia_speed = 0.0;
        self.wheel_deadline = None;
        self.touch = TouchState {
            start_y: y,
            start_x: x,
            last_y: y,
            start_offset: self.offset,
            speed: 0.0,
            last_timestamp: now,
            intent_confirmed: false,
        };
        self.stats.touch_starts += 1;
        had_inertia || had_timer
    }

    /// A touch move. Below [`TOUCH_INTENT_PX`] of travel it is not a scroll yet
    /// and nothing moves; that is what lets a tap on a line seek without
    /// nudging the view.
    pub fn touch_move(&mut self, y: f64, x: f64, now: f64) -> bool {
        // Read the gesture state out before calling anything on `self`: the
        // interaction hook borrows the whole engine, so holding a `&mut` into
        // one field would alias it.
        let (start_y, start_x, start_offset, last_y, last_timestamp) = {
            let state = &self.touch;
            (
                state.start_y,
                state.start_x,
                state.start_offset,
                state.last_y,
                state.last_timestamp,
            )
        };
        let delta_y = y - start_y;
        let delta_x = x - start_x;
        if !self.touch.intent_confirmed {
            if delta_y.abs() > TOUCH_INTENT_PX || delta_x.abs() > TOUCH_INTENT_PX {
                self.touch.intent_confirmed = true;
                self.begin_interaction(InteractionKind::Touch);
            } else {
                return false;
            }
        }
        self.offset = self.clamp(start_offset - (y - start_y));
        let dt = now - last_timestamp;
        if dt > 0.0 {
            self.touch.speed = (y - last_y) / dt;
        }
        self.touch.last_y = y;
        self.touch.last_timestamp = now;
        self.stats.touch_moves += 1;
        // A drag is a continuous update: the adapter keeps browsing alive.
        self.stats.update_events += 1;
        self.stats.continuous_updates += 1;
        true
    }

    /// Touch release. Starts inertia when the last sampled speed is above the
    /// start threshold; otherwise the gesture simply ends.
    pub fn touch_end(&mut self, now: f64) {
        if !self.touch.intent_confirmed {
            return;
        }
        self.touch.intent_confirmed = false;
        if self.touch.speed.abs() < INERTIA_STOP_SPEED {
            self.touch.speed = 0.0;
        }
        if self.touch.speed.abs() > INERTIA_START_SPEED {
            self.inertia_speed = self.touch.speed;
            self.inertia_last_frame = Some(now);
            self.stats.inertia_started += 1;
        } else {
            self.end_interaction(now);
        }
    }

    /// Advance inertia and expire the wheel debounce.
    ///
    /// Returns true when the offset changed, so the caller knows whether a
    /// repaint is needed.
    pub fn tick(&mut self, now: f64) -> bool {
        self.poll_timers(now);
        let mut moved = false;

        if let Some(last) = self.inertia_last_frame {
            let dt = now - last;
            if dt <= 0.0 || dt > MAX_INERTIA_FRAME_DELTA_MS {
                // Keep the frame clock current but drop the step: a hitch must
                // not translate into a jump of a quarter second's inertia.
                self.inertia_last_frame = Some(now);
                return moved;
            }
            if self.inertia_speed.abs() > INERTIA_START_SPEED {
                self.offset = self.clamp(self.offset - self.inertia_speed * dt);
                let steps = dt / BASE_FRAME_DURATION_MS;
                self.inertia_speed *= INERTIA_DECAY.powf(steps);
                self.inertia_last_frame = Some(now);
                self.stats.inertia_frames += 1;
                self.stats.update_events += 1;
                self.stats.continuous_updates += 1;
                moved = true;
            } else {
                self.inertia_last_frame = None;
                self.end_interaction(now);
            }
        }
        moved
    }

    /// Expire the wheel debounce if it is due.
    ///
    /// Split out of [`Self::tick`] so the replay can advance the clock without
    /// also stepping inertia: the reference fires the debounce from a
    /// `setTimeout` macrotask and inertia from `requestAnimationFrame`, which
    /// are two different queues.
    ///
    /// The end is stamped with `now`, not with the scheduled deadline, because
    /// the reference calls `performance.now()` inside the callback — that is,
    /// when it actually runs.
    pub fn poll_timers(&mut self, now: f64) {
        if let Some(deadline) = self.wheel_deadline {
            if now >= deadline {
                self.end_interaction(now);
            }
        }
    }

    /// Drop every transient: inertia, debounce, touch state and the offset
    /// itself. Used when the song changes or the view rebuilds.
    pub fn reset(&mut self, target: f64) {
        self.wheel_deadline = None;
        self.scroll_end_time = None;
        self.interacting = false;
        self.inertia_last_frame = None;
        self.inertia_speed = 0.0;
        self.touch = TouchState::default();
        self.offset = self.clamp(target);
        self.stats.resets += 1;
    }
}

// ---------------------------------------------------------------------------
// Layout anchor and bounds
// ---------------------------------------------------------------------------

/// Where the anchored row's top edge sits, as a fraction of the container.
///
/// The adapter installs `alignAnchor = Top` and expresses the old row-count
/// anchor as a position fraction, so `anchorOffset` is zero in the shipped
/// configuration. The enum stays because a future mode may anchor by centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignAnchor {
    Top,
    Center,
    Bottom,
}

impl AlignAnchor {
    pub fn offset(self, target_height: f64) -> f64 {
        if target_height <= 0.0 {
            return 0.0;
        }
        match self {
            AlignAnchor::Top => 0.0,
            AlignAnchor::Center => target_height / 2.0,
            AlignAnchor::Bottom => target_height,
        }
    }
}

/// `computeAlignFrac`: the anchored row's top edge as a fraction of the height.
///
/// An explicit `playingLineTopOffset` wins. Otherwise the mode's anchor row
/// count is converted from "row centres" to "top edge", half a row up. Both are
/// clamped: 0 would park the row against the very top with no context above it,
/// and 1 would push it off the bottom.
pub fn align_fraction(
    playing_line_top_offset: Option<f64>,
    anchor_rows: Option<f64>,
    visible_rows: Option<f64>,
) -> Option<f64> {
    let explicit = playing_line_top_offset.filter(|v| v.is_finite());
    if explicit.is_none() && anchor_rows.is_some() {
        // A row count without a measurable container is the one case that must
        // return nothing: guessing 42 % here would silently override the mode's
        // anchor on the first frame, before the window has its final height.
        let rows = visible_rows?;
        let anchor = anchor_rows?;
        return Some(((anchor - 0.5) / rows.max(1.0)).clamp(0.02, 0.95));
    }
    // Unset means the in-mode default of 42 % — never 0, which parks the row at
    // the very top with no context above it.
    let top = explicit.unwrap_or(42.0);
    Some((top / 100.0).clamp(0.05, 0.95))
}

/// One frame's layout metrics, in the reference's own terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutFrame {
    /// Top of the focal row in content coordinates.
    pub focal_top: f64,
    /// Height of the focal row.
    pub target_height: f64,
    /// Total height of all rows, including harmony rows.
    pub total_height: f64,
    /// Height of the interlude dots block, zero when none is displayed.
    pub interlude_height: f64,
    /// Container height.
    pub container_height: f64,
    /// Anchored position as a fraction of the container height.
    pub align_position: f64,
    pub align_anchor: AlignAnchor,
}

/// The closed scroll range and the offset that puts the focal row on its anchor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollBounds {
    pub min: f64,
    pub max: f64,
    /// Offset the playing line would have with no user displacement.
    pub resting: f64,
}

impl ScrollBounds {
    pub fn compute(frame: &LayoutFrame) -> Self {
        if frame.container_height <= 0.0 || frame.total_height <= 0.0 {
            return Self {
                min: 0.0,
                max: 0.0,
                resting: 0.0,
            };
        }
        let anchor_offset = frame.align_anchor.offset(frame.target_height);
        let min = (-frame.focal_top).min(0.0);
        let base = -frame.focal_top + frame.container_height * frame.align_position - anchor_offset;
        let total = frame.total_height + frame.interlude_height;
        // The reference stops at `base + total - containerHeight / 2`, i.e. the
        // last row may rise to the middle of the view, not to its top. That is
        // what leaves the "next line" preview visible at the end of a song.
        let raw_max = base + total - frame.container_height / 2.0;
        Self {
            min,
            max: raw_max.max(0.0),
            resting: 0.0,
        }
    }

    /// Resting offset is the zero-offset baseline: the layout already places
    /// the focal row on its anchor, so "no user displacement" is zero, not
    /// `base`. Exposed separately for tests and for the scrollbar percentage.
    pub fn span(&self) -> f64 {
        (self.max - self.min).max(0.0)
    }
}

/// Scrollbar state, computed from the same offset the content uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMetrics {
    pub has_overflow: bool,
    /// Fraction 0..=1.
    pub p: f64,
    pub span: f64,
}

impl ScrollEngine {
    pub fn metrics(&self) -> ScrollMetrics {
        let span = self.span();
        ScrollMetrics {
            has_overflow: span > 1.0,
            p: if span > 0.0 {
                ((self.offset - self.min_offset) / span).clamp(0.0, 1.0)
            } else {
                0.0
            },
            span,
        }
    }
}

/// Device-pixel snap for a row that has stopped.
///
/// Only call this when the spring reports `arrived()`. Snapping a moving row
/// quantises its trajectory into whole-pixel steps, which on a 239 Hz display
/// reads as a low-frame-rate judder — far worse than a locked subpixel phase.
pub fn snap_settled(y: f64, scale_factor: f64) -> f64 {
    if !(scale_factor > 0.0) {
        return y;
    }
    (y * scale_factor).round() / scale_factor
}

/// Scale applied to a row, or `None` when it is the identity.
///
/// Returning `None` for 1.0 lets the renderer skip the transform entirely
/// instead of composing a no-op matrix, which matters for glyph rasterisation:
/// any non-identity matrix pushes the glyph through a resample.
pub fn line_scale(is_active: bool, is_playing: bool, enable_scale: bool) -> Option<f64> {
    if !enable_scale || is_active || !is_playing {
        return None;
    }
    Some(0.97)
}

/// Harmony rows shrink further than the main row when they are not active.
pub fn harmony_scale(is_active: bool, is_playing: bool) -> Option<f64> {
    if is_active || !is_playing {
        return None;
    }
    Some(0.75)
}

// ---------------------------------------------------------------------------
// Browsing and return
// ---------------------------------------------------------------------------

/// The eased return that runs after browsing ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollReturn {
    pub from: f64,
    pub started: f64,
    pub duration: f64,
    pub offset: f64,
    pub baseline: f64,
}

impl ScrollReturn {
    pub fn new(from: f64, now: f64, baseline: f64) -> Self {
        Self {
            from,
            started: now,
            duration: RETURN_DURATION_MS,
            offset: from,
            // The reference stores the focal row's spring position *plus* the
            // offset it just wrote, so the first frame's shift is zero when the
            // row has not moved since the return was armed.
            baseline: baseline + from,
        }
    }

    /// One frame of the return.
    ///
    /// `baseline_now` is the focal row's current spring position. If the
    /// playing row advanced while the return was running, the eased distance
    /// must be measured from the new baseline, otherwise the row would land
    /// where it used to be. The reference compensates and restarts the clock
    /// with the remaining duration.
    pub fn update(&mut self, now: f64, baseline_now: f64) -> bool {
        // The reference restores the previous offset before measuring, so the
        // comparable baseline is "row position + offset", not the row alone.
        // Dropping this term makes a still row look like it drifted by exactly
        // its own offset and the return restarts forever.
        let shift = (baseline_now + self.offset) - self.baseline;
        if shift.abs() > 0.5 {
            self.from += shift;
            self.duration = (self.duration - (now - self.started)).max(120.0);
            self.started = now;
        }
        let t = ((now - self.started) / self.duration).clamp(0.0, 1.0);
        self.offset = self.from * (1.0 - t).powi(3);
        self.baseline = baseline_now + self.offset;
        t >= 1.0
    }
}

/// The adapter's browsing state, separate from the offset itself.
#[derive(Debug, Default)]
pub struct Browsing {
    /// The user is browsing: auto-align is suspended and passed lines stay lit.
    active: bool,
    /// A scrollbar thumb is held down.
    interaction_active: bool,
    idle_deadline: Option<f64>,
    return_: Option<ScrollReturn>,
}

impl Browsing {
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_returning(&self) -> bool {
        self.return_.is_some()
    }

    pub fn return_state(&self) -> Option<&ScrollReturn> {
        self.return_.as_ref()
    }

    pub fn idle_deadline(&self) -> Option<f64> {
        self.idle_deadline
    }

    /// Enter browsing. While a thumb is held the deadline is disabled: the
    /// user may hold still for minutes without the view snapping away.
    pub fn begin(&mut self, now: f64) {
        self.return_ = None;
        self.active = true;
        self.idle_deadline = if self.interaction_active {
            None
        } else {
            Some(now + SCROLL_IDLE_TIMEOUT_MS)
        };
    }

    pub fn set_interaction_active(&mut self, active: bool, now: f64) {
        self.interaction_active = active;
        if active || self.active {
            self.begin(now);
        }
    }

    /// Clear browsing. Returns true when a return animation should start.
    pub fn end(&mut self, animate: bool) -> bool {
        self.idle_deadline = None;
        self.active = false;
        self.interaction_active = false;
        self.return_ = None;
        animate
    }

    pub fn expire(&mut self, now: f64) -> bool {
        match self.idle_deadline {
            Some(deadline) if now >= deadline => {
                self.idle_deadline = None;
                true
            }
            _ => false,
        }
    }

    pub fn start_return(&mut self, from: f64, now: f64, baseline: f64) -> &ScrollReturn {
        self.return_ = Some(ScrollReturn::new(from, now, baseline));
        self.return_.as_ref().expect("just set")
    }

    /// Advance the return. Returns true when it finished this frame.
    pub fn update_return(&mut self, now: f64, baseline_now: f64) -> bool {
        let Some(ret) = self.return_.as_mut() else {
            return false;
        };
        let finished = ret.update(now, baseline_now);
        if finished {
            self.return_ = None;
        }
        finished
    }

    pub fn clear(&mut self) {
        self.active = false;
        self.interaction_active = false;
        self.idle_deadline = None;
        self.return_ = None;
    }
}

// ---------------------------------------------------------------------------
// Cache generation
// ---------------------------------------------------------------------------

/// A generation counter for layout geometry.
///
/// A font or width change invalidates every measured row at once. Frames must
/// only ever apply the newest generation: a slow row that comes back after a
/// newer generation started would otherwise paint at an old position, which
/// shows up as a row that jumps twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct GeometryGeneration(pub u64);

impl GeometryGeneration {
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }

    pub fn accepts(self, measured: Self) -> bool {
        measured == self
    }
}

/// Rows outside the drawn window, with the overscan the reference uses.
///
/// The buffer is deliberately larger than the viewport: a row that enters the
/// screen already has its glyphs, so a fast scroll does not show a blank line
/// for a frame. Rows outside are neither measured nor painted, but a long note
/// that spans the boundary is still *timed* — virtualisation trims drawing, not
/// the timeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibleRange {
    pub first: usize,
    pub last: usize,
}

impl VisibleRange {
    pub fn compute(
        row_tops: &[f64],
        row_heights: &[f64],
        offset: f64,
        container_height: f64,
        overscan_px: f64,
    ) -> Self {
        if row_tops.is_empty() {
            return Self { first: 0, last: 0 };
        }
        // The motion buffer is 40% of the container on each side: the reference
        // keeps rows alive while they are still visibly sliding in.
        let motion = container_height * 0.4;
        // A positive offset moves the content up, so a row's viewport
        // position is row_top - offset and the visible band in row coordinates
        // is [offset - motion - overscan, offset + container + motion + overscan].
        let top = -overscan_px - motion + offset;
        let bottom = container_height + overscan_px + motion + offset;
        let mut first = row_tops.len();
        let mut last = 0;
        for (index, (&row_top, &height)) in row_tops.iter().zip(row_heights).enumerate() {
            let row_bottom = row_top + height;
            if row_top <= bottom && row_bottom >= top {
                first = first.min(index);
                last = index.max(last);
            }
        }
        if first == row_tops.len() {
            // Nothing intersects: collapse to the row nearest the viewport so
            // the caller still has something to anchor to.
            // Content moves up as the offset grows, so the row under the
            // middle of the viewport is at row_top = offset + container/2.
            // Scrolling past the end must land on the last row, not the first.
            let centre = offset + container_height / 2.0;
            let nearest = row_tops
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    let da = (a.1 + row_heights[a.0] / 2.0 - centre).abs();
                    let db = (b.1 + row_heights[b.0] / 2.0 - centre).abs();
                    da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(index, _)| index)
                .unwrap_or(0);
            return Self {
                first: nearest,
                last: nearest,
            };
        }
        Self { first, last }
    }

    pub fn len(&self) -> usize {
        self.last.saturating_sub(self.first) + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spring_policy_is_slow_when_seeking_and_stiff_for_fast_songs() {
        let slow = pos_y_spring_policy(true, false, Some(100.0), false);
        assert_eq!(slow, SpringParams::new(SLOW_STIFFNESS, SLOW_DAMPING));

        let interlude = pos_y_spring_policy(false, true, Some(100.0), false);
        assert_eq!(interlude, slow);

        let ended = pos_y_spring_policy(false, false, Some(100.0), true);
        assert_eq!(ended, SpringParams::new(MEDIUM_STIFFNESS, MEDIUM_DAMPING));

        let fast = pos_y_spring_policy(false, false, Some(MIN_INTERVAL_MS), false);
        assert!((fast.stiffness - 220.0).abs() < 1e-9, "{}", fast.stiffness);
        assert!((fast.damping - 220.0_f64.sqrt() * DAMPING_MULTIPLIER).abs() < 1e-9);

        let slow_song = pos_y_spring_policy(false, false, Some(MAX_INTERVAL_MS), false);
        assert!((slow_song.stiffness - 170.0).abs() < 1e-9);

        // Interval unknown: the reference falls back to the slow pair rather
        // than guessing.
        assert_eq!(
            pos_y_spring_policy(false, false, None, false),
            SpringParams::new(SLOW_STIFFNESS, SLOW_DAMPING)
        );
    }

    #[test]
    fn the_spring_lands_on_target_and_reports_arrival() {
        let mut spring = Spring::new(0.0);
        spring.update_params(SpringParams::new(200.0, 26.0));
        spring.set_target_position(100.0);
        assert!(!spring.arrived());
        for _ in 0..600 {
            spring.update(1.0 / 240.0);
            if spring.arrived() {
                break;
            }
        }
        assert!(spring.arrived(), "spring never settled: {}", spring.position());
        assert_eq!(spring.position(), 100.0);
    }

    #[test]
    fn arrival_requires_low_acceleration_not_just_position() {
        // A critically damped spring leaving the origin has zero velocity but
        // maximum acceleration; the reference's `arrived` must not fire there.
        let mut spring = Spring::new(0.0);
        spring.update_params(SpringParams::new(170.0, 170.0_f64.sqrt() * 2.2));
        spring.set_target_position(50.0);
        spring.update(0.001);
        assert!(!spring.arrived());
    }

    #[test]
    fn a_delayed_target_starts_moving_after_the_delay() {
        let mut spring = Spring::new(0.0);
        spring.update_params(SpringParams::new(170.0, 40.0));
        spring.set_target_after(30.0, 0.5);
        spring.update(0.25);
        assert_eq!(spring.position(), 0.0);
        spring.update(0.3);
        // The reference samples the solver before it applies a due queue entry,
        // so the frame that releases the delay still paints the old position.
        assert_eq!(spring.position(), 0.0);
        spring.update(1.0 / 240.0);
        assert!(spring.position() > 0.0);
    }

    #[test]
    fn a_target_inside_the_deadband_does_not_restart_the_solver() {
        let mut spring = Spring::new(10.0);
        spring.update_params(SpringParams::new(200.0, 20.0));
        spring.set_target_position(10.000_5);
        spring.update(1.0 / 60.0);
        // Still at rest: the reference ignores sub-0.001 target changes.
        assert_eq!(spring.position(), 10.0);
    }

    #[test]
    fn the_engine_clamps_to_the_boundary_and_keeps_zero_reachable() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-40.0, 900.0);
        assert_eq!(engine.offset(), 0.0);
        engine.scroll_by(-10_000.0);
        assert_eq!(engine.offset(), 900.0);
        engine.scroll_by(10_000.0);
        assert_eq!(engine.offset(), -40.0);

        // A short song reports max below zero; zero must remain reachable.
        engine.update_boundary(-40.0, -10.0);
        assert_eq!(engine.max_offset(), 0.0);
    }

    #[test]
    fn a_wheel_burst_is_one_gesture_and_ends_after_the_debounce() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-100.0, 100.0);
        assert!(engine.wheel(10.0, true, 0.0), "first event starts the gesture");
        assert!(!engine.wheel(10.0, true, 50.0), "second event is the same gesture");
        assert!(engine.is_interacting());
        assert_eq!(engine.offset(), 20.0);
        assert!(!engine.can_resume_auto_align(150.0));
        engine.tick(200.0);
        assert!(!engine.is_interacting(), "debounce expired");
        assert!(!engine.can_resume_auto_align(200.0));
        assert!(engine.can_resume_auto_align(700.0));
        assert_eq!(engine.stats().wheel_events, 2);
    }

    #[test]
    fn line_mode_wheels_are_scaled() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-1_000.0, 1_000.0);
        engine.wheel(3.0, false, 0.0);
        assert_eq!(engine.offset(), 3.0 * WHEEL_LINE_PX);
    }

    #[test]
    fn touch_ignores_taps_and_follows_a_drag_one_to_one() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-500.0, 500.0);
        engine.touch_start(100.0, 100.0, 0.0);
        assert!(!engine.touch_move(104.0, 100.0, 16.0), "below intent threshold");
        assert_eq!(engine.offset(), 0.0);
        assert!(engine.touch_move(100.0, 130.0, 32.0), "horizontal intent still scrolls");
        assert!(engine.touch_move(80.0, 130.0, 48.0));
        assert_eq!(engine.offset(), 20.0);
    }

    #[test]
    fn a_flick_starts_inertia_that_decays_and_stops() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-10_000.0, 10_000.0);
        engine.touch_start(500.0, 0.0, 0.0);
        engine.touch_move(400.0, 0.0, 16.0);
        engine.touch_move(300.0, 0.0, 32.0);
        // Two samples 100 px apart in 16 ms each: 6.25 px/ms.
        engine.touch_end(32.0);
        assert!(engine.has_inertia());
        let mut now = 32.0;
        let mut frames = 0;
        while engine.has_inertia() && frames < 1_000 {
            now += BASE_FRAME_DURATION_MS;
            engine.tick(now);
            frames += 1;
        }
        assert!(!engine.has_inertia());
        assert!(frames > 10, "inertia ended after {frames} frames");
        assert!(engine.offset() > 100.0);
        assert!(engine.stats().inertia_frames >= 10);
    }

    #[test]
    fn a_stalled_frame_does_not_teleport_the_content() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-10_000.0, 10_000.0);
        engine.touch_start(500.0, 0.0, 0.0);
        engine.touch_move(400.0, 0.0, 16.0);
        engine.touch_move(300.0, 0.0, 32.0);
        engine.touch_end(32.0);
        let before = engine.offset();
        engine.tick(1_000.0); // 968 ms frame
        assert_eq!(engine.offset(), before, "a stalled frame must be dropped");
        assert!(engine.has_inertia(), "and inertia must continue");
    }

    #[test]
    fn the_scrollbar_shares_the_content_offset() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-200.0, 800.0);
        assert_eq!(
            engine.metrics(),
            ScrollMetrics {
                has_overflow: true,
                p: 0.2,
                span: 1_000.0
            }
        );
        engine.scroll_by(-500.0);
        assert_eq!(engine.offset(), 500.0);
        assert!((engine.metrics().p - 0.7).abs() < 1e-12);
    }

    #[test]
    fn a_thumb_drag_moves_exactly_the_pixels_it_was_given() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-100.0, 100.0);
        engine.scroll_by(37.5);
        assert_eq!(engine.offset(), -37.5);
        engine.scroll_by(-37.5);
        assert_eq!(engine.offset(), 0.0);
    }

    #[test]
    fn the_anchor_fraction_prefers_an_explicit_offset() {
        assert_eq!(align_fraction(Some(28.0), Some(1.5), Some(10.0)), Some(0.28));
        assert_eq!(align_fraction(Some(0.0), None, None), Some(0.05));
        assert_eq!(align_fraction(Some(200.0), None, None), Some(0.95));
        // No explicit offset: anchor rows are centres, so subtract half a row.
        assert_eq!(align_fraction(None, Some(1.5), Some(10.0)), Some(0.1));
        assert_eq!(align_fraction(None, None, Some(10.0)), Some(0.42));
        assert_eq!(align_fraction(None, Some(2.0), None), None);
    }

    #[test]
    fn the_bounds_match_the_reference_formula() {
        let frame = LayoutFrame {
            focal_top: 1_000.0,
            target_height: 100.0,
            total_height: 4_000.0,
            interlude_height: 0.0,
            container_height: 800.0,
            align_position: 0.28,
            align_anchor: AlignAnchor::Top,
        };
        let bounds = ScrollBounds::compute(&frame);
        assert_eq!(bounds.min, -1_000.0);
        // base = -1000 + 800*0.28 - 0 = -776; max = -776 + 4000 - 400 = 2824
        assert!((bounds.max - 2824.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_layout_has_no_bounds() {
        let frame = LayoutFrame {
            focal_top: 0.0,
            target_height: 0.0,
            total_height: 0.0,
            interlude_height: 0.0,
            container_height: 800.0,
            align_position: 0.28,
            align_anchor: AlignAnchor::Top,
        };
        assert_eq!(
            ScrollBounds::compute(&frame),
            ScrollBounds {
                min: 0.0,
                max: 0.0,
                resting: 0.0
            }
        );
    }

    #[test]
    fn the_return_eases_to_zero_and_compensates_a_advanced_row() {
        let mut ret = ScrollReturn::new(200.0, 0.0, 0.0);
        assert!(!ret.update(300.0, 0.0));
        assert!(ret.offset < 200.0 && ret.offset > 0.0);
        assert!(ret.update(600.0, 0.0), "600 ms completes the return");
        assert_eq!(ret.offset, 0.0);

        // The playing row moved up by 50 px mid-return: the distance is
        // re-measured from the new baseline, so the row does not land at the
        // position it used to occupy.
        let mut ret = ScrollReturn::new(200.0, 0.0, 0.0);
        ret.update(300.0, 50.0);
        assert!((ret.from - 250.0).abs() < 1e-9, "from = {}", ret.from);
        assert!(ret.baseline >= 50.0);
    }

    #[test]
    fn browsing_suspends_the_idle_timeout_while_a_thumb_is_held() {
        let mut browsing = Browsing::default();
        browsing.set_interaction_active(true, 0.0);
        assert!(browsing.is_active());
        assert_eq!(browsing.idle_deadline(), None);
        assert!(!browsing.expire(10_000.0), "a held thumb never expires");

        browsing.set_interaction_active(false, 10_000.0);
        assert_eq!(browsing.idle_deadline(), Some(14_000.0));
        assert!(!browsing.expire(13_999.0));
        assert!(browsing.expire(14_000.0));
    }

    #[test]
    fn settled_rows_snap_to_the_device_grid_but_moving_ones_do_not() {
        assert_eq!(snap_settled(10.4, 1.0), 10.0);
        assert_eq!(snap_settled(10.6, 1.0), 11.0);
        // At 1.5x a device pixel is 2/3 of a logical pixel.
        // 10.4 logical px at 1.5x is 15.6 device px, which rounds to 16.
        assert!((snap_settled(10.4, 1.5) - 10.666_666_667).abs() < 1e-6);
        assert_eq!(snap_settled(10.4, 0.0), 10.4, "bad scale must not divide");
    }

    #[test]
    fn identity_scale_is_omitted_and_ninety_seven_percent_is_kept() {
        assert_eq!(line_scale(true, true, true), None);
        assert_eq!(line_scale(false, false, true), None);
        assert_eq!(line_scale(false, true, false), None);
        assert_eq!(line_scale(false, true, true), Some(0.97));
        assert_eq!(harmony_scale(false, true), Some(0.75));
        assert_eq!(harmony_scale(true, true), None);
    }

    #[test]
    fn virtualization_keeps_the_buffer_and_never_trims_a_long_row() {
        // 100 rows of 50 px. At offset 0 the viewport shows 0..800 plus 40% of
        // the container of motion buffer on each side.
        let tops: Vec<f64> = (0..100).map(|i| i as f64 * 50.0).collect();
        let heights = vec![50.0; 100];
        let range = VisibleRange::compute(&tops, &heights, 0.0, 800.0, 240.0);
        assert_eq!(range.first, 0);
        // 800 + 240 + 320 = 1360 px of bottom buffer -> row 27 (1350..1400)
        assert_eq!(range.last, 27);
        assert_eq!(range.len(), 28);

        // A 2 000 px row that starts far above the viewport is still visible.
        let mut tall = vec![50.0; 100];
        tall[0] = 2_000.0;
        let tops: Vec<f64> = std::iter::once(0.0)
            .chain((1..100).map(|i| 2_000.0 + (i as f64 - 1.0) * 50.0))
            .collect();
        let range = VisibleRange::compute(&tops, &tall, 0.0, 800.0, 240.0);
        assert_eq!(range.first, 0, "the tall row must stay drawn");
    }

    #[test]
    fn virtualization_centres_on_the_nearest_row_when_nothing_intersects() {
        let tops = vec![0.0, 100.0, 200.0];
        let heights = vec![50.0, 50.0, 50.0];
        // Scrolled far above the first row: anchor to the first one.
        let range = VisibleRange::compute(&tops, &heights, -5_000.0, 800.0, 0.0);
        assert_eq!(range.first, 0);
        assert_eq!(range.last, 0);
        // ...and far past the last row: anchor to the last one. A sign error
        // here is invisible in the middle of a song and wrong at both ends.
        let range = VisibleRange::compute(&tops, &heights, 5_000.0, 800.0, 0.0);
        assert_eq!(range.first, 2);
        assert_eq!(range.last, 2);
    }

    #[test]
    fn an_old_geometry_generation_is_rejected() {
        let first = GeometryGeneration::default();
        let second = first.next();
        assert!(second.accepts(second));
        assert!(!second.accepts(first), "a stale measurement must not paint");
        assert_eq!(first.next(), second);
    }

    #[test]
    fn a_reset_clears_inertia_and_returns_to_the_target() {
        let mut engine = ScrollEngine::new();
        engine.update_boundary(-100.0, 100.0);
        engine.touch_start(100.0, 0.0, 0.0);
        engine.touch_move(0.0, 0.0, 16.0);
        engine.touch_end(16.0);
        assert!(engine.has_inertia());
        engine.reset(0.0);
        assert!(!engine.has_inertia());
        assert_eq!(engine.offset(), 0.0);
        assert!(!engine.is_interacting());
    }
}
