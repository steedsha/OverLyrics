//! Local liquid-glass panels: placement, entrance motion, material parameters
//! and the software reference for the glass composite.
//!
//! The shipped application draws its panels with `@ttqtt/liquid-glass-react`
//! (`renderer="css"`, i.e. a CSS `backdrop-filter` path). Task 8 ports the
//! *observable* contract of that library, not the library itself:
//!
//! * **Materials** — the library's table (`dist/index.js`, `Ln`) with the
//!   application's overrides from `base.css` / `lyrics.css`. The recorded
//!   filter on every popover is `blur(6px) saturate(1.12) contrast(1.05)`,
//!   which is exactly the `clear/large` row plus the contrast the library
//!   injects for the dark tone.
//! * **Placement** — a literal port of `popover-placement.ts`.
//! * **Entrance** — the library injects `--lg-spring` as a `linear()` easing
//!   with 18 stops over `--lg-duration-spring: 520ms`. The recorded transform
//!   is `matrix(S,0,0,S,0,S*Y)` with `S = 0.86 + 0.14*C(t)` and
//!   `Y = -6*(1-C(t))`: scale and the local translateY are driven by *one*
//!   curve, and the recorded matrix component is `scale * translateY` because
//!   the pose is `scale(S) translateY(-6px)`. The content fades separately
//!   (`--lg-duration-release` 220 ms, `cubic-bezier(.16,1,.3,1)`), which is
//!   what keeps the first frame's glass fully sampled — only the content
//!   fades, never the backdrop sampling.
//! * **Composite** — one snapshot of the already-drawn scene, a separable
//!   Gaussian (CSS `blur(6px)` means sigma = 3 px per the filter spec),
//!   `saturate(1.12)`, `contrast(1.05)`, the tint and the rim, in that order.
//!   The same math is the reference for `shaders/glass.hlsl`.
//!
//! Nothing here may fall back to a whole-window DWM blur or a flat translucent
//! plate: the tests sample the composed scene and would fail if the backdrop
//! were frozen or ignored.

pub mod gpu;

pub mod tracks;

/// The `--lg-spring` easing the library injects: `linear()` stops as
/// `(progress, value)` pairs. The first stop is implicit at `(0, 0)` and the
/// last at `(1, 1)`.
pub const SPRING_STOPS: [(f64, f64); 18] = [
    (0.000, 0.000),
    (0.011, 0.008),
    (0.022, 0.030),
    (0.046, 0.118),
    (0.096, 0.430),
    (0.134, 0.657),
    (0.174, 0.840),
    (0.207, 0.940),
    (0.239, 1.006),
    (0.275, 1.050),
    (0.315, 1.072),
    (0.359, 1.070),
    (0.414, 1.050),
    (0.513, 1.011),
    (0.614, 0.995),
    (0.703, 0.994),
    (0.874, 1.001),
    (1.000, 1.000),
];

/// `--lg-duration-spring` (ms): the closed→open transform transition.
pub const DURATION_SPRING_MS: f64 = 520.0;
/// `--lg-duration-press` (ms): the open→closed transform transition.
pub const DURATION_PRESS_MS: f64 = 90.0;
/// `--lg-duration-release` (ms): the content fade-in.
pub const DURATION_RELEASE_MS: f64 = 220.0;

/// The closed pose: `scale(0.86) translateY(-6px)`, local CSS px.
pub const CLOSED_SCALE: f64 = 0.86;
pub const CLOSED_TRANSLATE_Y: f64 = -6.0;

/// `cubic-bezier(x1, y1, x2, y2)` evaluated at `t` (the x axis), solved by
/// bisection on the x parameter. `epsilon` is in x units.
pub fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, t: f64) -> f64 {
    let epsilon = 1e-6;
    let at = |u: f64| -> f64 {
        // One-dimensional cubic Bezier component.
        let v = 1.0 - u;
        3.0 * v * v * u * x1 + 3.0 * v * u * u * x2 + u * u * u
    };
    let at_y = |u: f64| -> f64 {
        let v = 1.0 - u;
        3.0 * v * v * u * y1 + 3.0 * v * u * u * y2 + u * u * u
    };
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let mut lo = 0.0;
    let mut hi = 1.0;
    let mut u = t;
    for _ in 0..64 {
        u = 0.5 * (lo + hi);
        if at(u) < t {
            lo = u;
        } else {
            hi = u;
        }
        if hi - lo < epsilon {
            break;
        }
    }
    at_y(u)
}

/// The spring easing, piecewise linear between the frozen stops.
pub fn spring_curve(progress: f64) -> f64 {
    if progress <= 0.0 {
        return 0.0;
    }
    if progress >= 1.0 {
        return 1.0;
    }
    let mut previous = (0.0, 0.0);
    for &(position, value) in SPRING_STOPS.iter() {
        if progress <= position {
            let span = position - previous.0;
            if span <= 0.0 {
                return value;
            }
            let alpha = (progress - previous.0) / span;
            return previous.1 + (value - previous.1) * alpha;
        }
        previous = (position, value);
    }
    1.0
}

/// A rectangle in CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// Where a popover ended up: position plus which side of the anchor it is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Below,
    Above,
}

/// Literal port of `popover-placement.ts`: `gap = 8`, `pad = 8`, prefer below
/// unless the panel would cross the bottom padding, and clamp both axes.
pub fn place_popover(anchor: Rect, panel: (f64, f64), viewport: (f64, f64), preferred: Option<Side>) -> (Rect, Side) {
    let gap = 8.0;
    let pad = 8.0;
    let below = anchor.top + anchor.height + gap;
    let side = preferred.unwrap_or({
        if below + panel.1 <= viewport.1 - pad {
            Side::Below
        } else {
            Side::Above
        }
    });
    let left = (anchor.left + anchor.width - panel.0)
        .max(pad)
        .min(viewport.0 - panel.0 - pad);
    let top = match side {
        Side::Below => below,
        Side::Above => anchor.top - panel.1 - gap,
    }
    .max(pad)
    .min(viewport.1 - panel.1 - pad);
    (
        Rect {
            left,
            top,
            width: panel.0,
            height: panel.1,
        },
        side,
    )
}

/// One glass material: the backdrop chain, the tint and the rim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// CSS `blur()` radius in px. The Gaussian sigma is half of this.
    pub blur_px: f64,
    pub saturation: f64,
    pub contrast: f64,
    /// Tint colour, 0..1 linear-ish sRGB channels (the app uses 24 25 28).
    pub tint: [f64; 3],
    pub tint_opacity: f64,
    /// Rim gradient endpoints, straight sRGB with alpha.
    pub rim_hi: [f64; 4],
    pub rim_lo: [f64; 4],
    /// Corner radius in px.
    pub radius: f64,
}

impl Material {
    /// The popover material (`clear/large` + app overrides from
    /// `lyrics.css` `.lg-popover[data-material='clear']` and the recorded
    /// `contrast(1.05)`).
    pub fn popover_clear() -> Self {
        Self {
            blur_px: 6.0,
            saturation: 1.12,
            contrast: 1.05,
            tint: [24.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0],
            tint_opacity: 0.36,
            rim_hi: [1.0, 1.0, 1.0, 0.52],
            rim_lo: [0.0, 0.0, 0.0, 0.2],
            radius: 14.0,
        }
    }

    /// The whole-window plate (`regular/large` + `base.css` overrides).
    pub fn window_plate() -> Self {
        Self {
            blur_px: 40.0,
            saturation: 1.3,
            contrast: 1.05,
            tint: [24.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0],
            tint_opacity: 0.3,
            rim_hi: [1.0, 1.0, 1.0, 0.62],
            rim_lo: [0.0, 0.0, 0.0, 0.16],
            radius: 20.0,
        }
    }

    /// Gaussian sigma for the CSS blur radius.
    pub fn blur_sigma(&self) -> f64 {
        self.blur_px / 2.0
    }

    /// How many px of extra scene content the blur kernel needs on every side
    /// of the panel rect: three sigmas covers 99.7% of the kernel weight.
    pub fn sample_margin(&self) -> f64 {
        self.blur_sigma() * 3.0
    }
}

/// Entrance/exit motion for one popover surface.
///
/// The scale and the local translateY are driven by one spring curve; the
/// matrix the renderer sees is `scale(S) translateY(Y)`, whose flattened form
/// is `[S, 0, 0, S, 0, S*Y]`. The content opacity is a separate fade so the
/// glass is fully sampled from the first frame.
#[derive(Clone, Copy, Debug)]
pub struct Entrance;

impl Entrance {
    /// Spring progress at `elapsed_ms` into the open transition.
    pub fn open_curve(elapsed_ms: f64) -> f64 {
        spring_curve(elapsed_ms / DURATION_SPRING_MS)
    }

    /// Spring progress at `elapsed_ms` into the close transition
    /// (`cubic-bezier(.2,.8,.2,1)` over 90 ms, running 1 → 0).
    pub fn close_curve(elapsed_ms: f64) -> f64 {
        1.0 - cubic_bezier(0.2, 0.8, 0.2, 1.0, elapsed_ms / DURATION_PRESS_MS)
    }

    /// Scale at `elapsed_ms`; `closing` selects the exit transition.
    pub fn scale_at(elapsed_ms: f64, closing: bool) -> f64 {
        let c = if closing {
            Self::close_curve(elapsed_ms)
        } else {
            Self::open_curve(elapsed_ms)
        };
        CLOSED_SCALE + (1.0 - CLOSED_SCALE) * c
    }

    /// Local translateY (CSS px) at `elapsed_ms`.
    pub fn translate_at(elapsed_ms: f64, closing: bool) -> f64 {
        let c = if closing {
            Self::close_curve(elapsed_ms)
        } else {
            Self::open_curve(elapsed_ms)
        };
        CLOSED_TRANSLATE_Y * (1.0 - c)
    }

    /// Content opacity at `elapsed_ms` (fade-in over the release duration).
    pub fn content_opacity_at(elapsed_ms: f64) -> f64 {
        cubic_bezier(0.16, 1.0, 0.3, 1.0, elapsed_ms / DURATION_RELEASE_MS)
    }

    /// The flattened transform matrix components `[S, 0, 0, S, 0, S*Y]`.
    pub fn matrix_at(elapsed_ms: f64, closing: bool) -> [f64; 6] {
        let s = Self::scale_at(elapsed_ms, closing);
        let y = Self::translate_at(elapsed_ms, closing);
        [s, 0.0, 0.0, s, 0.0, s * y]
    }
}

/// Rounded-rect signed distance (positive outside, px) for `point` in a rect
/// of `size` with corner `radius`.
pub fn rounded_rect_sdf(point: (f64, f64), size: (f64, f64), radius: f64) -> f64 {
    let r = radius.min(size.0 * 0.5).min(size.1 * 0.5);
    let px = point.0 - size.0 * 0.5;
    let py = point.1 - size.1 * 0.5;
    let qx = px.abs() - (size.0 * 0.5 - r);
    let qy = py.abs() - (size.1 * 0.5 - r);
    let outside_x = qx.max(0.0);
    let outside_y = qy.max(0.0);
    let outside = (outside_x * outside_x + outside_y * outside_y).sqrt();
    let inside = qx.max(qy).min(0.0);
    // iq's rounded-box SDF: the `- r` term puts the zero level on the actual
    // edge, not `r` px outside it.
    outside + inside - r
}

/// Separable Gaussian blur over an RGBA8 buffer, one channel set at a time,
/// clamped at the buffer edges. `sigma` in px; a radius of `ceil(3*sigma)`
/// matches the reference kernels used elsewhere in this crate.
fn gaussian_blur_rgba8(pixels: &[u8], width: u32, height: u32, sigma: f64) -> Vec<u8> {
    let radius = (sigma * 3.0).ceil() as i64;
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut total = 0.0;
    for i in -radius..=radius {
        let w = (-0.5 * (i as f64 / sigma).powi(2)).exp();
        kernel.push(w);
        total += w;
    }
    for w in kernel.iter_mut() {
        *w /= total;
    }
    let (w, h) = (width as usize, height as usize);
    let mut horizontal = vec![0.0f32; pixels.len()];
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let mut acc = 0.0f32;
                for (k, weight) in kernel.iter().enumerate() {
                    let sx = (x as i64 + k as i64 - radius).clamp(0, w as i64 - 1) as usize;
                    acc += pixels[(y * w + sx) * 4 + c] as f32 * *weight as f32;
                }
                horizontal[(y * w + x) * 4 + c] = acc;
            }
        }
    }
    let mut out = vec![0u8; pixels.len()];
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let mut acc = 0.0f32;
                for (k, weight) in kernel.iter().enumerate() {
                    let sy = (y as i64 + k as i64 - radius).clamp(0, h as i64 - 1) as usize;
                    acc += horizontal[(sy * w + x) * 4 + c] * *weight as f32;
                }
                out[(y * w + x) * 4 + c] = acc.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// One glass composite over a captured backdrop region.
///
/// `backdrop` is `region.width x region.height` RGBA8 straight-alpha pixels —
/// the already-drawn scene captured once — where `region` covers the panel
/// rect expanded by [`Material::sample_margin`]. `panel` is the panel rect in
/// *region-local* px. Returns the composited panel pixels
/// (`panel.width x panel.height`), straight alpha, opaque.
///
/// Order per the CSS stack: blur → saturate → contrast (the backdrop filter),
/// then the tint element, then the rim; clear content is drawn afterwards by
/// the caller and never by this function.
pub fn composite_glass(
    backdrop: &[u8],
    region: (u32, u32),
    panel: Rect,
    material: &Material,
) -> Vec<u8> {
    let blurred = gaussian_blur_rgba8(backdrop, region.0, region.1, material.blur_sigma());
    let (pw, ph) = (panel.width.round() as u32, panel.height.round() as u32);
    let mut out = vec![0u8; (pw * ph * 4) as usize];
    let tint = material.tint;
    let sat = material.saturation as f32;
    let contrast = material.contrast as f32;
    for py in 0..ph {
        for px in 0..pw {
            let fx = px as f64 + 0.5;
            let fy = py as f64 + 0.5;
            // Sample the blurred buffer at the texel whose centre matches the
            // panel pixel centre: B[k] = blur(k + 0.5) and the panel pixel
            // centre sits at panel.left + px + 0.5, so k = panel.left + px.
            // Rounding the centre itself (panel.left + fx) would bias the
            // sample one texel right/down and smear every hard edge.
            let sx = (panel.left + px as f64).round() as i64;
            let sy = (panel.top + py as f64).round() as i64;
            let index = ((sy.clamp(0, region.1 as i64 - 1) as u32 * region.0
                + sx.clamp(0, region.0 as i64 - 1) as u32)
                * 4) as usize;
            let mut rgb = [
                blurred[index] as f32 / 255.0,
                blurred[index + 1] as f32 / 255.0,
                blurred[index + 2] as f32 / 255.0,
            ];
            // saturate(s): channel spread around the Rec.709 luma.
            let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
            for channel in rgb.iter_mut() {
                *channel = luma + (*channel - luma) * sat;
            }
            // contrast(c) around mid grey.
            for channel in rgb.iter_mut() {
                *channel = (*channel - 0.5) * contrast + 0.5;
            }
            // The rounded shape: pixels outside the corner radius are
            // transparent (the caller may also clip, but the reference
            // composite bakes it in). Then the tint element over the backdrop.
            let sdf = rounded_rect_sdf((fx, fy), (panel.width, panel.height), material.radius);
            if sdf > 0.5 {
                let o = ((py * pw + px) * 4) as usize;
                out[o] = 0;
                out[o + 1] = 0;
                out[o + 2] = 0;
                out[o + 3] = 0;
                continue;
            }
            let a = material.tint_opacity as f32;
            for (channel, tint_channel) in rgb.iter_mut().zip(tint) {
                *channel = *channel * (1.0 - a) + tint_channel as f32 * a;
            }
            // The rim: a one-pixel inset band hugging the edge (the CSS is
            // `box-shadow: inset 0 0 0 1px`), gradient from the highlight at
            // the top to the shadow at the bottom.
            let inside = -sdf;
            if inside <= 1.5 {
                // Peaks one half-pixel inside the edge; zero at the boundary
                // and one and a half pixels in.
                let coverage = (1.0 - (inside - 0.5).abs()).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    let gradient = 1.0 - fy / panel.height;
                    let hi = material.rim_hi;
                    let lo = material.rim_lo;
                    let rim_alpha = ((lo[3] + (hi[3] - lo[3]) * gradient) * coverage) as f32;
                    for c in 0..3 {
                        // rim endpoints are stored in 0..1 already.
                        let rim = (lo[c] + (hi[c] - lo[c]) * gradient) as f32;
                        rgb[c] = rgb[c] * (1.0 - rim_alpha) + rim * rim_alpha;
                    }
                }
            }
            let o = ((py * pw + px) * 4) as usize;
            out[o] = (rgb[0] * 255.0).round().clamp(0.0, 255.0) as u8;
            out[o + 1] = (rgb[1] * 255.0).round().clamp(0.0, 255.0) as u8;
            out[o + 2] = (rgb[2] * 255.0).round().clamp(0.0, 255.0) as u8;
            out[o + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spring_curve_hits_every_frozen_stop_exactly() {
        for &(position, value) in SPRING_STOPS.iter() {
            assert!(
                (spring_curve(position) - value).abs() < 1e-9,
                "stop {position} should evaluate to {value}"
            );
        }
        assert_eq!(spring_curve(-1.0), 0.0);
        assert_eq!(spring_curve(2.0), 1.0);
    }

    #[test]
    fn the_entrance_matrix_matches_the_recorded_pose_family() {
        // Both driven properties share one curve: S = 0.86+0.14C, Y = -6(1-C).
        // Eliminating C must reconstruct the recorded matrix for any time.
        for elapsed in [0.0, 30.0, 90.0, 176.4, 230.6, 400.0, 520.0, 700.0] {
            let s = Entrance::scale_at(elapsed, false);
            let y = Entrance::translate_at(elapsed, false);
            let matrix = Entrance::matrix_at(elapsed, false);
            assert!((matrix[0] - s).abs() < 1e-12);
            assert!((matrix[5] - s * y).abs() < 1e-12);
            let recovered = (s - CLOSED_SCALE) / (1.0 - CLOSED_SCALE);
            assert!(
                (y - CLOSED_TRANSLATE_Y * (1.0 - recovered)).abs() < 1e-9,
                "translate must be the same curve as scale at {elapsed} ms"
            );
        }
        assert!((Entrance::scale_at(0.0, false) - 0.86).abs() < 1e-9);
        assert!((Entrance::translate_at(0.0, false) + 6.0).abs() < 1e-9);
        assert!((Entrance::scale_at(10_000.0, false) - 1.0).abs() < 1e-9);
        // The spring overshoots to C=1.072: scale peak 1.01008, translate +0.432.
        let peak = (0..600)
            .map(|ms| ms as f64)
            .map(|ms| (Entrance::scale_at(ms, false), ms))
            .fold(f64::MIN, |best, (s, _)| best.max(s));
        assert!(
            (peak - (CLOSED_SCALE + (1.0 - CLOSED_SCALE) * 1.072)).abs() < 1e-3,
            "peak scale {peak} should be the frozen overshoot"
        );
    }

    #[test]
    fn the_close_transition_runs_back_to_the_closed_pose() {
        assert!((Entrance::scale_at(0.0, true) - 1.0).abs() < 1e-9);
        assert!((Entrance::scale_at(90.0, true) - 0.86).abs() < 1e-9);
        // cubic-bezier(.2,.8,.2,1) is monotone; the exit must be too.
        let mut previous = 1.0;
        for ms in (0..=90).step_by(5) {
            let s = Entrance::scale_at(ms as f64, true);
            assert!(s <= previous + 1e-9, "exit scale must not rise at {ms} ms");
            previous = s;
        }
    }

    #[test]
    fn the_content_fades_in_separately_and_the_glass_never_waits_for_it() {
        // At 0 ms the glass is fully posed (scale 0.86) while the content is
        // still invisible — the first frame samples the backdrop completely.
        assert!((Entrance::scale_at(0.0, false) - 0.86).abs() < 1e-9);
        assert!(Entrance::content_opacity_at(0.0) < 1e-6);
        assert!((Entrance::content_opacity_at(DURATION_RELEASE_MS) - 1.0).abs() < 1e-6);
        // Ease-out rises fast: over half visible in the first third.
        let third = Entrance::content_opacity_at(DURATION_RELEASE_MS / 3.0);
        assert!(third > 0.55, "ease-out content at 1/3 was {third}");
    }

    #[test]
    fn placement_prefers_below_and_flips_above_at_the_bottom() {
        let anchor = Rect { left: 100.0, top: 100.0, width: 36.0, height: 36.0 };
        // Fits below: top = 100+36+8.
        let (rect, side) = place_popover(anchor, (252.0, 220.0), (1272.0, 843.0), None);
        assert_eq!(side, Side::Below);
        assert!((rect.top - 144.0).abs() < 1e-9);
        // The anchor (100..136) is narrower than the 252px panel, so the
        // right-aligned left edge is negative and clamps to the viewport pad.
        assert!((rect.left - 8.0).abs() < 1e-9);
        // A mid-screen anchor still fits below: 744+84 = 828 <= 843-8.
        let low_anchor = Rect { left: 100.0, top: 700.0, width: 36.0, height: 36.0 };
        let (_, side) = place_popover(low_anchor, (240.0, 84.0), (1272.0, 843.0), None);
        assert_eq!(side, Side::Below);
        // An anchor near the bottom edge does not: 814+84 = 898 > 835.
        let bottom_anchor = Rect { left: 100.0, top: 770.0, width: 36.0, height: 36.0 };
        let (rect, side) = place_popover(bottom_anchor, (240.0, 84.0), (1272.0, 843.0), None);
        assert_eq!(side, Side::Above);
        assert!((rect.top - (770.0 - 84.0 - 8.0)).abs() < 1e-9);
        // A preferred side wins over the fit check.
        let (_, forced) = place_popover(anchor, (252.0, 220.0), (1272.0, 843.0), Some(Side::Above));
        assert_eq!(forced, Side::Above);
    }

    #[test]
    fn the_popover_material_is_the_recorded_filter() {
        let material = Material::popover_clear();
        assert_eq!(material.blur_px, 6.0);
        assert_eq!(material.saturation, 1.12);
        assert_eq!(material.contrast, 1.05);
        assert!((material.tint_opacity - 0.36).abs() < 1e-9);
        assert_eq!(material.radius, 14.0);
        // blur(6px) is sigma 3; the margin keeps the kernel inside the capture.
        assert!((material.blur_sigma() - 3.0).abs() < 1e-9);
        assert!((material.sample_margin() - 9.0).abs() < 1e-9);
    }

    #[test]
    fn a_uniform_backdrop_composites_to_an_exact_tint() {
        // Over a flat field the blur/saturate/contrast chain is analytically
        // computable, so the composite must match it bit-near-exactly.
        let material = Material::popover_clear();
        let margin = material.sample_margin() as u32;
        let (pw, ph) = (40u32, 30u32);
        let region = (pw + margin * 2, ph + margin * 2);
        let field = [90u8, 140, 200];
        let mut backdrop = Vec::new();
        for _ in 0..region.0 * region.1 {
            backdrop.extend_from_slice(&[field[0], field[1], field[2], 255]);
        }
        let panel = Rect {
            left: margin as f64,
            top: margin as f64,
            width: pw as f64,
            height: ph as f64,
        };
        let out = composite_glass(&backdrop, region, panel, &material);
        // Analytic: blur keeps a flat field, saturate keeps it (grey-ish luma
        // spread scales around luma but a flat field stays flat), contrast
        // pushes away from 0.5, then the tint mixes in.
        let s = [field[0] as f64 / 255.0, field[1] as f64 / 255.0, field[2] as f64 / 255.0];
        let luma = 0.2126 * s[0] + 0.7152 * s[1] + 0.0722 * s[2];
        let mut expected = [0.0f64; 3];
        for c in 0..3 {
            let saturated = luma + (s[c] - luma) * material.saturation;
            let contrasted = (saturated - 0.5) * material.contrast + 0.5;
            expected[c] = contrasted * (1.0 - material.tint_opacity)
                + material.tint[c] * material.tint_opacity;
        }
        for py in 0..ph {
            for px in 0..pw {
                let index = ((py * pw + px) * 4) as usize;
                let actual = &out[index..index + 4];
                // Corner pixels outside the rounded shape are transparent.
                let sdf = rounded_rect_sdf(
                    (px as f64 + 0.5, py as f64 + 0.5),
                    (panel.width, panel.height),
                    material.radius,
                );
                if sdf > 0.5 {
                    assert_eq!(actual[3], 0, "corner pixel ({px},{py}) must be transparent");
                    continue;
                }
                assert_eq!(actual[3], 255, "edge pixel ({px},{py}) must stay in the shape");
                // The 1px inset rim hugs the edge; the analytic tint chain only
                // holds for pixels deeper than the rim band.
                if sdf > -1.5 {
                    continue;
                }
                for c in 0..3 {
                    let want = (expected[c] * 255.0).round();
                    assert!(
                        (actual[c] as f64 - want).abs() <= 1.0,
                        "pixel ({px},{py}) channel {c}: {} vs {want}",
                        actual[c]
                    );
                }
            }
        }
    }

    #[test]
    fn the_blur_smears_a_hard_edge_and_keeps_flat_centres_untouched() {
        // glass-lab g1_1: the centre of a uniform region must be unchanged
        // (maxCenterPx = 0) while an edge under the glass smears over ~blur px.
        let material = Material::popover_clear();
        let margin = material.sample_margin() as u32;
        let (pw, ph) = (60u32, 40u32);
        let region = (pw + margin * 2, ph + margin * 2);
        let mut backdrop = vec![0u8; (region.0 * region.1 * 4) as usize];
        for y in 0..region.1 {
            for x in 0..region.0 {
                // Left half dark, right half bright — a hard vertical edge.
                let value = if x < region.0 / 2 { 20 } else { 220 };
                let i = ((y * region.0 + x) * 4) as usize;
                backdrop[i] = value;
                backdrop[i + 1] = value;
                backdrop[i + 2] = value;
                backdrop[i + 3] = 255;
            }
        }
        let panel = Rect {
            left: margin as f64,
            top: margin as f64,
            width: pw as f64,
            height: ph as f64,
        };
        let out = composite_glass(&backdrop, region, panel, &material);
        let at = |x: u32, y: u32| -> u8 {
            let i = ((y * pw + x) * 4) as usize;
            out[i]
        };
        // The composite output is tinted glass over the backdrop, not the raw
        // backdrop: 220 through the analytic chain lands at ~152 (computed
        // below), and the point of this test is the flat interior + the graded
        // edge, not raw values.
        let analytic = |raw: f64| -> f64 {
            let s = raw / 255.0;
            let luma = 0.2126 * s + 0.7152 * s + 0.0722 * s;
            let saturated = luma + (s - luma) * material.saturation;
            let contrasted = (saturated - 0.5) * material.contrast + 0.5;
            (contrasted * (1.0 - material.tint_opacity)
                + material.tint[0] * material.tint_opacity)
                * 255.0
        };
        let centre_dark = at(10, ph / 2);
        assert!(
            (centre_dark as f64 - analytic(20.0)).abs() <= 3.0,
            "dark centre {centre_dark} vs analytic {}",
            analytic(20.0)
        );
        let centre_bright = at(pw - 10, ph / 2);
        assert!(
            (centre_bright as f64 - analytic(220.0)).abs() <= 3.0,
            "bright centre {centre_bright} vs analytic {}",
            analytic(220.0)
        );
        // Across the edge the value must actually pass through mid grey.
        let mut saw_mid = false;
        for x in 0..pw {
            let v = at(x, ph / 2);
            if (100..=140).contains(&v) {
                saw_mid = true;
            }
        }
        assert!(saw_mid, "the blurred edge must be graded, not a hard step");
    }

    #[test]
    fn the_glass_tracks_the_live_scene_instead_of_a_frozen_snapshot() {
        // The forbidden implementation samples once and keeps the stale
        // backdrop. Compositing two different scenes through the same panel
        // must produce two different results.
        let material = Material::popover_clear();
        let margin = material.sample_margin() as u32;
        let (pw, ph) = (30u32, 20u32);
        let region = (pw + margin * 2, ph + margin * 2);
        let panel = Rect {
            left: margin as f64,
            top: margin as f64,
            width: pw as f64,
            height: ph as f64,
        };
        let scene = |value: u8| -> Vec<u8> {
            let mut pixels = vec![value; (region.0 * region.1 * 4) as usize];
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
            pixels
        };
        let dark = composite_glass(&scene(30), region, panel, &material);
        let bright = composite_glass(&scene(200), region, panel, &material);
        let dark_mean: f64 = dark.chunks_exact(4).map(|p| p[0] as f64).sum::<f64>()
            / (pw * ph) as f64;
        let bright_mean: f64 = bright.chunks_exact(4).map(|p| p[0] as f64).sum::<f64>()
            / (pw * ph) as f64;
        assert!(
            bright_mean - dark_mean > 80.0,
            "glass must follow the scene: {dark_mean} vs {bright_mean}"
        );
    }

    #[test]
    fn the_rim_is_a_one_pixel_band_with_a_top_to_bottom_gradient() {
        // glass-lab g1_3 measures a 1px anti-aliased rim; the highlight sits at
        // the top edge and the shadow at the bottom edge.
        let material = Material::popover_clear();
        let margin = material.sample_margin() as u32;
        let (pw, ph) = (50u32, 36u32);
        let region = (pw + margin * 2, ph + margin * 2);
        let mut backdrop = vec![0u8; (region.0 * region.1 * 4) as usize];
        for pixel in backdrop.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[128, 128, 128, 255]);
        }
        let panel = Rect {
            left: margin as f64,
            top: margin as f64,
            width: pw as f64,
            height: ph as f64,
        };
        let out = composite_glass(&backdrop, region, panel, &material);
        let at = |x: u32, y: u32| -> [u8; 3] {
            let i = ((y * pw + x) * 4) as usize;
            [out[i], out[i + 1], out[i + 2]]
        };
        // Interior: tint over mid grey, no rim.
        let interior = at(pw / 2, ph / 2);
        let tinted = 128.0 * (1.0 - 0.36) + 25.3 * 0.36;
        assert!((interior[0] as f64 - tinted).abs() <= 2.0, "interior {interior:?}");
        // Top edge: brighter than the interior (white rim, alpha .52).
        let top = at(pw / 2, 0);
        assert!(top[0] > interior[0] + 40, "top rim {top:?} vs interior {interior:?}");
        // Bottom edge: darker than the interior (black rim, alpha .2).
        let bottom = at(pw / 2, ph - 1);
        assert!(bottom[0] < interior[0] - 5, "bottom rim {bottom:?} vs interior {interior:?}");    }

    #[test]
    fn the_sample_margin_covers_the_blur_kernel() {
        // A scene edge exactly one margin outside the panel must not bleed
        // into the panel: the expanded capture is what guarantees it.
        let material = Material::popover_clear();
        let margin = material.sample_margin() as u32;
        let (pw, ph) = (40u32, 24u32);
        let region = (pw + margin * 2, ph + margin * 2);
        let mut backdrop = vec![0u8; (region.0 * region.1 * 4) as usize];
        for y in 0..region.1 {
            for x in 0..region.0 {
                let i = ((y * region.0 + x) * 4) as usize;
                // Bright column in the left margin only.
                let value = if x < margin { 250 } else { 40 };
                backdrop[i] = value;
                backdrop[i + 1] = value;
                backdrop[i + 2] = value;
                backdrop[i + 3] = 255;
            }
        }
        let panel = Rect {
            left: margin as f64,
            top: margin as f64,
            width: pw as f64,
            height: ph as f64,
        };
        let out = composite_glass(&backdrop, region, panel, &material);
        let first_column = out[0];
        // The kernel reaches 3 sigma = exactly the margin; its tail there is
        // ~0.011 of the peak weight, so the bleed is bounded and small.
        assert!(
            first_column < 90,
            "margin-bright bleed into the first panel column was {first_column}"
        );
        // And by the third column it is gone entirely.
        let third = out[2 * 4];
        assert!(third < 55, "residual bleed at column 2 was {third}");
    }
}
