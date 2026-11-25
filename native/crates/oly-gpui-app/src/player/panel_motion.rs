//! Continuous presentation state independent of playback and frame rate.
use super::interaction::Feedback;
use oly_core::scroll::{Spring, SpringParams};

pub struct PanelMotion {
    spring: Spring,
    content: Feedback,
    surface: Feedback,
    open: bool,
    light: [Spring; 3],
}
impl Default for PanelMotion {
    fn default() -> Self {
        Self {
            spring: Spring::new(0.0),
            content: Feedback::default(),
            surface: Feedback::default(),
            open: false,
            light: std::array::from_fn(|i| {
                let mut s = Spring::new([0.25, 0.05, 0.35][i]);
                s.update_params(SpringParams::new(220.0, 29.0));
                s
            }),
        }
    }
}
impl PanelMotion {
    pub fn step(&mut self, open: bool, dt_ms: f64) {
        if open != self.open {
            // The first glass frame samples at full strength. Reversing an
            // in-flight exit preserves the current surface alpha and velocity.
            if open && !self.visible() {
                self.surface.hover = 1.0;
            }
            self.spring.update_params(if open {
                SpringParams::new(310.0, 15.5)
            } else {
                SpringParams::new(600.0, 42.0)
            });
            self.spring
                .set_target_position(if open { 1.0 } else { 0.0 });
            self.open = open;
        }
        self.spring.update(dt_ms.max(0.0) / 1000.0);
        for s in &mut self.light {
            s.update(dt_ms.max(0.0) / 1000.0);
        }
        let fade_dt = dt_ms.max(0.0) * if open { 1.0 } else { 2.0 };
        self.content.step(open, false, fade_dt);
        self.surface.step(open, false, fade_dt);
    }
    pub fn aim_light(&mut self, x: f64, y: f64, energy: f64) {
        for (s, v) in self.light.iter_mut().zip([
            x.clamp(-0.25, 1.25),
            y.clamp(-0.25, 1.25),
            energy.clamp(0.0, 1.0),
        ]) {
            s.set_target_position(v);
        }
    }
    pub fn light(&self) -> [f32; 3] {
        self.light.each_ref().map(|s| s.position() as f32)
    }
    pub fn progress(&self) -> f64 {
        self.spring.position()
    }
    pub fn visible(&self) -> bool {
        self.open || self.surface.hover > 0.002
    }
    pub fn pending(&self) -> bool {
        self.light.iter().any(|s| !s.arrived())
            || !self.spring.arrived()
            || self.content.hover != if self.open { 1.0 } else { 0.0 }
            || self.surface.hover != if self.open { 1.0 } else { 0.0 }
    }
    pub fn content_alpha(&self) -> f32 {
        self.content.hover
    }
    pub fn surface_alpha(&self) -> f32 {
        self.surface.hover
    }
    pub fn offset(&self) -> f32 {
        (14.0 * (1.0 - self.progress())) as f32
    }
    pub fn scale(&self) -> f32 {
        let p = self.progress() as f32;
        if self.open {
            0.88 + 0.12 * p
        } else {
            0.94 + 0.06 * p
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reversing_keeps_the_current_pose_and_closing_eventually_unmounts() {
        let mut p = PanelMotion::default();
        assert!(!p.visible());
        for _ in 0..15 {
            p.step(true, 8.0);
        }
        assert!(p.visible() && p.progress() > 0.0 && p.pending());
        let before = p.progress();
        p.step(false, 0.0);
        assert_eq!(p.progress(), before);
        p.step(false, 8.0);
        let before = p.progress();
        p.step(true, 0.0);
        assert_eq!(p.progress(), before);
        for _ in 0..600 {
            p.step(false, 8.0);
        }
        assert!(!p.pending());
        assert!(!p.visible());
    }
    #[test]
    fn presentation_is_the_same_at_60_and_240hz() {
        let mut a = PanelMotion::default();
        let mut b = PanelMotion::default();
        for _ in 0..12 {
            a.step(true, 1000.0 / 60.0);
        }
        for _ in 0..48 {
            b.step(true, 1000.0 / 240.0);
        }
        assert!(a.progress() > 0.5);
        assert!((a.progress() - b.progress()).abs() < 0.001);
    }
}
