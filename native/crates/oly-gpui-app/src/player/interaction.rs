//! OverLyrics-specific lyric hover state; generic feedback primitives live in `oly-ui`.
pub use oly_ui::interaction::{Feedback, panel_pixels, state_icon_spring};

/// Fade only the hovered wrapper; its geometry follows layout without a spring.
#[derive(Default)]
pub struct LyricHover {
    pub index: Option<usize>,
    pub alpha: f32,
}
impl LyricHover {
    pub fn step(&mut self, target: Option<usize>, dt_ms: f64) -> bool {
        if target.is_some() && target != self.index {
            self.index = target;
            self.alpha = 0.0;
        }
        let goal = if target.is_some() { 1.0 } else { 0.0 };
        self.alpha = super::model::step_opacity(self.alpha, goal, dt_ms, 250.0, false);
        if self.alpha == 0.0 && target.is_none() { self.index = None; }
        self.alpha != goal
    }
}

#[cfg(test)]
mod quiet_motion_tests {
    use super::*;
    #[test]
    fn state_changes_are_slow_and_do_not_rebound_in_either_direction() {
        for start in [0.0, 1.0] {
            let mut spring = state_icon_spring(start);
            spring.set_target_position(1.0 - start);
            for _ in 0..6 { spring.update(1.0 / 60.0); }
            let travel = (spring.position() - start).abs();
            assert!(travel > 0.1 && travel < 0.5, "100ms travel={travel}");
            for _ in 0..120 {
                spring.update(1.0 / 60.0);
                assert!((-0.001..=1.001).contains(&spring.position()));
            }
            assert!(spring.arrived());
        }
    }
    #[test]
    fn hover_tracks_the_same_row_then_fades_on_exit_or_occlusion() {
        let mut h = LyricHover::default();
        assert!(h.step(Some(3), 100.0));
        assert_eq!(h.index, Some(3));
        assert!((h.alpha - 0.4).abs() < 0.001);
        assert!(!h.step(Some(3), 150.0));
        assert_eq!(h.alpha, 1.0);
        assert!(h.step(None, 100.0));
        assert_eq!(h.index, Some(3));
        assert!((h.alpha - 0.6).abs() < 0.001);
        assert!(!h.step(None, 150.0));
        assert_eq!(h.index, None);
        assert_eq!(h.alpha, 0.0);
    }
    #[test]
    fn entering_another_row_never_slides_or_stretches_the_old_highlight() {
        let mut h = LyricHover::default();
        h.step(Some(3), 250.0);
        h.step(Some(7), 50.0);
        assert_eq!(h.index, Some(7));
        assert!((h.alpha - 0.2).abs() < 0.001);
    }
}
