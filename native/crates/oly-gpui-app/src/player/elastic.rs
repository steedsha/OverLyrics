//! Bounded pointer deformation and release semantics for liquid controls.
use oly_core::scroll::{Spring, SpringParams};
pub struct Elastic {
    x: Spring,
    y: Spring,
    pressure: Spring,
}
impl Default for Elastic {
    fn default() -> Self {
        let make = || {
            let mut s = Spring::new(0.0);
            s.update_params(SpringParams::new(320.0, 18.0));
            s
        };
        Self {
            x: make(),
            y: make(),
            pressure: make(),
        }
    }
}
impl Elastic {
    pub fn step(&mut self, delta: [f32; 2], held: bool, dt: f64) {
        let rubber = |v: f32| v as f64 / (1.0 + (v as f64).abs() / 7.0);
        self.x
            .set_target_position(if held { rubber(delta[0]) } else { 0.0 });
        self.y
            .set_target_position(if held { rubber(delta[1]) } else { 0.0 });
        self.pressure
            .set_target_position(if held { 1.0 } else { 0.0 });
        for s in [&mut self.x, &mut self.y, &mut self.pressure] {
            s.update(dt.max(0.0) / 1000.0);
        }
    }
    pub fn pull(&self) -> [f32; 2] {
        [
            self.x.position().clamp(-9.0, 9.0) as f32,
            self.y.position().clamp(-9.0, 9.0) as f32,
        ]
    }
    pub fn squeeze(&self) -> [f32; 2] {
        let p = self.pull();
        let press = self.pressure();
        [
            -0.035 * press + 0.016 * p[0].abs() - 0.006 * p[1].abs(),
            -0.035 * press + 0.016 * p[1].abs() - 0.006 * p[0].abs(),
        ]
    }
    pub fn pressure(&self) -> f32 {
        self.pressure.position().clamp(-0.2, 1.2) as f32
    }
    pub fn pending(&self) -> bool {
        !self.x.arrived() || !self.y.arrived() || !self.pressure.arrived()
    }
}
#[derive(Default)]
pub struct PressGesture {
    origin: Option<[f32; 2]>,
    delta: [f32; 2],
    dragged: bool,
}
impl PressGesture {
    pub fn begin(&mut self, x: f32, y: f32) {
        self.origin = Some([x, y]);
        self.delta = [0.0; 2];
        self.dragged = false;
    }
    pub fn update(&mut self, x: f32, y: f32) {
        if let Some(o) = self.origin {
            self.delta = [x - o[0], y - o[1]];
            self.dragged |= self.delta[0].hypot(self.delta[1]) > 6.0;
        }
    }
    pub fn delta(&self) -> [f32; 2] {
        self.delta
    }
    pub fn dragged(&self) -> bool {
        self.dragged
    }
    pub fn cancel(&mut self) {
        self.origin = None;
        self.delta = [0.0; 2];
        self.dragged = false;
    }
    pub fn release(&mut self, inside: bool) -> bool {
        let click = self.origin.is_some() && inside && !self.dragged;
        self.cancel();
        click
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dragging_stretches_in_its_direction_then_springs_back() {
        let mut e = Elastic::default();
        for _ in 0..30 {
            e.step([60.0, 0.0], true, 8.0);
        }
        assert!(e.pull()[0] > 2.0 && e.pull()[0] < 9.0);
        assert!(e.squeeze()[0] > e.squeeze()[1]);
        let p = e.pull();
        e.step([0.0; 2], false, 0.0);
        assert_eq!(e.pull(), p);
        let mut overshot = false;
        for _ in 0..500 {
            e.step([0.0; 2], false, 8.0);
            overshot |= e.pull()[0] < -0.01;
        }
        assert!(overshot, "release must preserve velocity and recoil");
        assert!(!e.pending());
        assert_eq!(e.pull(), [0.0; 2]);
    }
    #[test]
    fn elastic_response_is_frame_rate_independent() {
        let mut a = Elastic::default();
        let mut b = Elastic::default();
        for _ in 0..12 {
            a.step([40.0, 20.0], true, 1000.0 / 60.0);
        }
        for _ in 0..48 {
            b.step([40.0, 20.0], true, 1000.0 / 240.0);
        }
        assert!(a.pressure() > 0.5);
        assert!((a.pull()[0] - b.pull()[0]).abs() < 0.001);
    }
    #[test]
    fn dragging_out_and_back_does_not_activate_a_button() {
        let mut g = PressGesture::default();
        g.begin(0.0, 0.0);
        g.update(30.0, 0.0);
        g.update(0.0, 0.0);
        assert!(!g.release(true));
        g.begin(0.0, 0.0);
        g.update(2.0, 1.0);
        assert!(g.release(true));
        assert!(!g.release(true));
        g.begin(0.0, 0.0);
        assert!(!g.release(false));
    }
}
