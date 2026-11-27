//! Fixed-size GPU backing with a spring-deformed capsule inside it.

use crate::geometry::Box4;

pub fn hit(track: Box4, thumb: Box4) -> Box4 {
    Box4::new(track.x - 10., thumb.y - 5., 24., thumb.h + 10.)
}

pub fn rest_line(track: Box4, thumb: Box4, hovered: bool) -> Box4 {
    let w = if hovered { 3.5 } else { 2.5 };
    Box4::new(track.x + (track.w - w) * 0.5, thumb.y, w, thumb.h)
}

pub fn held_shape(height: f32, pressure: f32, pull: [f32; 2]) -> Box4 {
    let p = pressure.clamp(0.0, 1.15);
    let w = (2.5 + 7.5 * p + pull[0].abs() * 0.15).clamp(2.5, 14.0);
    let h = height + (4. * p + pull[1].abs() * 0.3).clamp(-1., 7.);
    Box4::new((32. - w) * 0.5 + pull[0] * 0.25, 10. + pull[1] * 0.2 - (h - height) * 0.5, w, h)
}

pub fn shape(height: f32, pressure: f32, pull: [f32; 2]) -> Box4 {
    let p = pressure.clamp(-0.1, 1.15);
    let w = (6. + 12. * p + pull[0].abs() * 0.2).clamp(5., 22.);
    let h = height + (4. * p + pull[1].abs() * 0.3).clamp(-1., 7.);
    Box4::new((32. - w) * 0.5 + pull[0] * 0.25, 10. + pull[1] * 0.2 - (h - height) * 0.5, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pressed_capsule_widens_inside_a_stable_backing() {
        let a = shape(90., 0., [0., 0.]);
        let b = shape(90., 1., [6., 7.]);
        assert_eq!(a.w, 6.);
        assert!(b.w > a.w * 2.);
        assert!(b.x > 0. && b.right() < 32. && b.y >= 0. && b.bottom() < 110.);
    }
    #[test]
    fn rest_line_is_thin_and_held_shape_thickens() {
        let track = Box4::new(100., 20., 4., 300.);
        let thumb = Box4::new(98., 40., 8., 60.);
        let line = rest_line(track, thumb, false);
        assert_eq!(line.w, 2.5);
        let held = held_shape(60., 1.0, [0., 0.]);
        assert!(held.w > line.w * 3. && held.right() < 32.);
    }
}
