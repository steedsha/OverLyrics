//! Frame-rate independent chrome feedback and device-pixel glass allocation.

#[derive(Clone, Copy, Debug, Default)]
pub struct Feedback {
    pub hover: f32,
    pub press: f32,
    pub flash: f32,
    pub hop: f32,
    pub hop_vel: f32,
    pub pull: [f32; 2],
    pub squeeze: [f32; 2],
    pub focus: [f32; 2],
    pub was_pressed: bool,
    pub was_hovered: bool,
}

impl Feedback {
    pub fn trigger_hop(&mut self, impulse: f32) {
        self.hop_vel = self.hop_vel.max(impulse);
    }

    pub fn step(&mut self, hovered: bool, pressed: bool, dt_ms: f64) -> bool {
        self.step_with_hop(hovered, pressed, dt_ms, 220.0, 28.0, 1.8)
    }

    pub fn step_with_hop(
        &mut self,
        hovered: bool,
        pressed: bool,
        dt_ms: f64,
        k: f64,
        c: f64,
        impulse: f32,
    ) -> bool {
        fn approach(value: &mut f32, target: f32, dt: f64, tau: f64) -> bool {
            *value += (target - *value) * (1.0 - (-dt.max(0.0) / tau).exp()) as f32;
            if (*value - target).abs() < 0.001 {
                *value = target;
            }
            *value != target
        }
        if hovered && !self.was_hovered && !pressed {
            self.hop_vel = self.hop_vel.max(impulse);
        }
        self.was_hovered = hovered;
        if pressed && !self.was_pressed {
            self.flash = 1.0;
        }
        self.was_pressed = pressed;
        let hover_pending = approach(&mut self.hover, if hovered { 1.0 } else { 0.0 }, dt_ms, 55.0);
        let press_pending = approach(&mut self.press, if pressed { 1.0 } else { 0.0 }, dt_ms, 35.0);
        let flash_target = if pressed { 0.65 } else { 0.0 };
        let flash_tau = if pressed { 70.0 } else { 95.0 };
        let flash_pending = approach(&mut self.flash, flash_target, dt_ms, flash_tau);
        let clamped_ms = dt_ms.clamp(0.0, 250.0);
        if clamped_ms > 0.0 && (self.hop != 0.0 || self.hop_vel != 0.0) {
            let substeps = ((clamped_ms / 0.5).ceil() as usize).max(1);
            let h = (clamped_ms / 1000.0) / substeps as f64;
            let c = if pressed { c * 1.15 } else { c };
            let mut x = self.hop as f64;
            let mut v = self.hop_vel as f64;
            for _ in 0..substeps {
                let a = -k * x - c * v;
                v += a * h;
                x += v * h;
            }
            if x.abs() < 0.0008 && v.abs() < 0.01 {
                self.hop = 0.0;
                self.hop_vel = 0.0;
            } else {
                self.hop = x as f32;
                self.hop_vel = v as f32;
            }
        }
        let hop_pending = self.hop != 0.0 || self.hop_vel != 0.0;
        hover_pending || press_pending || flash_pending || hop_pending
    }
}

pub fn panel_pixels(size: (f64, f64), scale: f32) -> (u32, u32) {
    (
        (size.0 * scale as f64).round().max(1.0) as u32,
        (size.1 * scale as f64).round().max(1.0) as u32,
    )
}

/// Quiet state-icon transition: near critical damping avoids a release rebound.
pub fn state_icon_spring(position: f64) -> oly_core::scroll::Spring {
    let mut spring = oly_core::scroll::Spring::new(position);
    spring.update_params(oly_core::scroll::SpringParams::new(180.0, 27.0));
    spring
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feedback_reverses_continuously_and_settles_when_paused() {
        let mut f = Feedback::default();
        assert!(f.step(true, false, 16.0));
        assert!(f.hover > 0.0 && f.hover < 1.0);
        let previous = f.hover;
        f.step(false, true, 4.0);
        assert!(f.hover > 0.0 && f.hover < previous);
        assert!(f.press > 0.0 && f.press < 1.0);
        for _ in 0..400 { f.step(false, false, 4.0); }
        assert_eq!(f.hover, 0.0);
        assert_eq!(f.press, 0.0);
        assert_eq!(f.flash, 0.0);
        assert!(!f.step(false, false, 4.0));
    }
    #[test]
    fn press_charges_flash_pulse_and_decays_smoothly() {
        let mut f = Feedback::default();
        assert!(f.step(true, true, 16.0));
        assert!(f.flash > 0.85);
        assert!(f.step(true, false, 32.0));
        assert!(f.flash > 0.1 && f.flash < 0.85);
        for _ in 0..200 {
            f.step(false, false, 8.0);
        }
        assert_eq!(f.flash, 0.0);
    }
    #[test]
    fn feedback_is_independent_of_frame_rate() {
        let mut a = Feedback::default(); let mut b = a;
        for _ in 0..60 { a.step(true, true, 1000.0/60.0); }
        for _ in 0..240 { b.step(true, true, 1000.0/240.0); }
        assert!((a.hover-b.hover).abs() < 0.001);
        assert!((a.press-b.press).abs() < 0.001);
    }
    #[test]
    fn hover_triggers_one_subtle_hop_and_settles() {
        let mut f = Feedback::default();
        let mut peak = 0.0_f32;
        for _ in 0..30 {
            f.step(true, false, 8.0);
            peak = peak.max(f.hop);
        }
        assert!(peak > 0.03 && peak < 0.2, "hover should gently hop once: peak={peak}");
        for _ in 0..200 {
            f.step(true, false, 8.0);
        }
        assert_eq!(f.hop, 0.0);
        assert_eq!(f.hop_vel, 0.0);
    }

    #[test]
    fn panel_allocation_tracks_each_size_and_dpi() {
        assert_eq!(panel_pixels((240.0,96.0),1.0),(240,96));
        assert_eq!(panel_pixels((240.0,96.0),1.5),(360,144));
        assert_ne!(panel_pixels((240.0,96.0),1.5),panel_pixels((340.0,192.0),1.5));
    }
}
