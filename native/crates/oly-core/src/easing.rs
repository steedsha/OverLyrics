//! `bezier-easing@3.2.0`, matching the curve solver imported by AMLL.

/// Cubic Bezier easing curve evaluated by Newton-Raphson with binary-subdivision
/// fallback over an 11-sample lookup table, matching `bezier-easing@3.2.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BezierEasing {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

const NEWTON_ITERATIONS: u32 = 4;
const NEWTON_MIN_SLOPE: f64 = 0.001;
const SUBDIVISION_PRECISION: f64 = 0.000_000_1;
const SUBDIVISION_MAX_ITERATIONS: u32 = 10;
const SPLINE_TABLE_SIZE: usize = 11;
const SAMPLE_STEP: f64 = 1.0 / (SPLINE_TABLE_SIZE as f64 - 1.0);

impl BezierEasing {
    pub const fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn table(&self) -> [f64; SPLINE_TABLE_SIZE] {
        let mut table = [0.0; SPLINE_TABLE_SIZE];
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = calc_bezier(i as f64 * SAMPLE_STEP, self.x1, self.x2);
        }
        table
    }

    pub fn eval(&self, x: f64) -> f64 {
        if self.x1 == self.y1 && self.x2 == self.y2 {
            return x;
        }
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        let samples = self.table();
        calc_bezier(self.t_for_x(x, &samples), self.y1, self.y2)
    }

    fn t_for_x(&self, x: f64, samples: &[f64; SPLINE_TABLE_SIZE]) -> f64 {
        let mut interval_start = 0.0;
        let mut current = 1usize;
        let last = SPLINE_TABLE_SIZE - 1;
        while current != last && samples[current] <= x {
            interval_start += SAMPLE_STEP;
            current += 1;
        }
        current -= 1;
        let dist = (x - samples[current]) / (samples[current + 1] - samples[current]);
        let guess = interval_start + dist * SAMPLE_STEP;
        let slope = get_slope(guess, self.x1, self.x2);
        if slope >= NEWTON_MIN_SLOPE {
            newton_raphson(x, guess, self.x1, self.x2)
        } else if slope == 0.0 {
            guess
        } else {
            binary_subdivide(
                x,
                interval_start,
                interval_start + SAMPLE_STEP,
                self.x1,
                self.x2,
            )
        }
    }
}

fn a(a1: f64, a2: f64) -> f64 {
    1.0 - 3.0 * a2 + 3.0 * a1
}

fn b(a1: f64, a2: f64) -> f64 {
    3.0 * a2 - 6.0 * a1
}

fn c(a1: f64) -> f64 {
    3.0 * a1
}

fn calc_bezier(t: f64, a1: f64, a2: f64) -> f64 {
    ((a(a1, a2) * t + b(a1, a2)) * t + c(a1)) * t
}

fn get_slope(t: f64, a1: f64, a2: f64) -> f64 {
    3.0 * a(a1, a2) * t * t + 2.0 * b(a1, a2) * t + c(a1)
}

fn binary_subdivide(x: f64, mut a_: f64, mut b_: f64, x1: f64, x2: f64) -> f64 {
    let mut i = 0;
    loop {
        let current_t = a_ + (b_ - a_) / 2.0;
        let current_x = calc_bezier(current_t, x1, x2) - x;
        if current_x.abs() < SUBDIVISION_PRECISION {
            return current_t;
        }
        if current_x > 0.0 {
            b_ = current_t;
        } else {
            a_ = current_t;
        }
        i += 1;
        if i >= SUBDIVISION_MAX_ITERATIONS {
            return current_t;
        }
    }
}

fn newton_raphson(x: f64, mut t: f64, x1: f64, x2: f64) -> f64 {
    for _ in 0..NEWTON_ITERATIONS {
        let slope = get_slope(t, x1, x2);
        if slope == 0.0 {
            return t;
        }
        let current_x = calc_bezier(t, x1, x2) - x;
        t -= current_x / slope;
    }
    t
}
