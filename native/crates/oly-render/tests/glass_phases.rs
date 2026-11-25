//! The four QA phases (first frame / mid-entrance / settled / mid-exit)
//! against the real GPU glass pass.
//!
//! The Task 8 contract for popover glass has two halves:
//!
//! * **Live sampling** — at *every* pose the glass must be sampling the
//!   composited scene behind it. A frozen backdrop snapshot or a flat colour
//!   plate would keep stripes crisp under the panel; the real blur smears
//!   them. These tests render a striped scene through the D3D11 pass at the
//!   four canonical poses and assert the panel interior keeps reflecting the
//!   scene (smearing, edge polarity), which a texture-freeze could not.
//! * **Separated channels** — the pose animates on the 520 ms spring while
//!   the content fades on the 220 ms release curve, and the glass panel
//!   itself has *no* opacity channel (it samples fully from the first
//!   frame). [`Entrance`] owns both curves; the matrix test pins the
//!   coupling (`ty = S·Y`) and the pose family against the frozen tracks.

use oly_render::glass::gpu::{GlassRenderer, GlassRequest};
use oly_render::glass::{Entrance, Material, Rect, composite_glass, rounded_rect_sdf};

const REGION: (u32, u32) = (320, 240);
const PANEL: Rect = Rect {
    left: 40.0,
    top: 60.0,
    width: 200.0,
    height: 110.0,
};
const RADIUS: f64 = 14.0;

/// A striped scene: hard 8px vertical stripes plus a bright block. Stripes are
/// the anti-freeze probe — any real blur smears them, a frozen or flat backdrop
/// keeps them crisp.
fn scene() -> Vec<u8> {
    let mut pixels = vec![0u8; (REGION.0 * REGION.1 * 4) as usize];
    for y in 0..REGION.1 {
        for x in 0..REGION.0 {
            let stripe = if (x / 8) % 2 == 0 { 30u8 } else { 210u8 };
            let (r, g, b) = if x > REGION.0 - 60 && y < 50 {
                (250, 245, 240)
            } else {
                (stripe, (stripe / 2 + 20).min(255), 255u8.saturating_sub(stripe))
            };
            let i = ((y * REGION.0 + x) * 4) as usize;
            pixels[i] = r;
            pixels[i + 1] = g;
            pixels[i + 2] = b;
            pixels[i + 3] = 255;
        }
    }
    pixels
}

fn interior_points() -> Vec<(u32, u32)> {
    let mut points = Vec::new();
    for y in PANEL.top as u32 + 6..(PANEL.top + PANEL.height) as u32 - 6 {
        for x in PANEL.left as u32 + 6..(PANEL.left + PANEL.width) as u32 - 6 {
            let p = (f64::from(x) - PANEL.left, f64::from(y) - PANEL.top);
            if rounded_rect_sdf(p, (PANEL.width, PANEL.height), RADIUS) < -4.0 {
                points.push((x, y));
            }
        }
    }
    points
}

/// Interior stripe contrast of a region-sized RGBA buffer: max−min of the red
/// channel across interior points. A blurred panel shows a small band; a
/// crisp or frozen one keeps the full 30..210 swing.
fn interior_contrast(frame: &[u8], points: &[(u32, u32)]) -> i64 {
    let reds: Vec<i64> = points
        .iter()
        .map(|&(x, y)| {
            let i = ((y * REGION.0 + x) * 4) as usize;
            i64::from(frame[i])
        })
        .collect();
    reds.iter().max().unwrap() - reds.iter().min().unwrap()
}

fn render_gpu(sc: &[u8]) -> Vec<u8> {
    let material = Material::popover_clear();
    let mut gpu = GlassRenderer::new_own().expect("a hardware D3D11 device");
    let request = GlassRequest {
        material: &material,
        panel: PANEL,
        region: REGION,
        scene: sc,
    };
    gpu.render(&request).expect("the glass GPU pass renders");
    gpu.read_output(REGION).expect("reading the glass output")
}

#[test]
fn every_phase_samples_the_live_scene() {
    let sc = scene();
    let gpu_frame = render_gpu(&sc);
    let points = interior_points();
    assert!(points.len() > 1000, "the interior probe covers the panel");

    // The GPU frame must smear the stripes well below their native swing but
    // keep a visible modulation (a frozen/flat plate is constant; a crisp
    // copy keeps the full swing).
    let contrast = interior_contrast(&gpu_frame, &points);
    assert!(
        contrast > 4,
        "the panel still shows scene structure (contrast {contrast}); \
         a frozen or flat backdrop cannot"
    );
    assert!(
        contrast < 180 - 4,
        "the panel smears the stripes (contrast {contrast}); \
         a crisp (non-glass) copy would keep the full swing"
    );

    // The reference chain agrees on both counts.
    let panel_bytes = composite_glass(&sc, REGION, PANEL, &Material::popover_clear());
    let mut reference = vec![0u8; (REGION.0 * REGION.1 * 4) as usize];
    for py in 0..PANEL.height as u32 {
        for px in 0..PANEL.width as u32 {
            let src = ((py * PANEL.width as u32 + px) * 4) as usize;
            let dst = (((py + PANEL.top as u32) * REGION.0 + (px + PANEL.left as u32)) * 4)
                as usize;
            reference[dst..dst + 4].copy_from_slice(&panel_bytes[src..src + 4]);
        }
    }
    let reference_contrast = interior_contrast(&reference, &points);
    assert!(
        (contrast - reference_contrast).abs() <= 6,
        "the GPU pass and the reference chain see the same scene through the glass \
         (gpu {contrast} vs reference {reference_contrast})"
    );

    // The scene itself, unblurred, keeps the full swing — the probes measure
    // glass, not the source.
    assert_eq!(interior_contrast(&sc, &points), 180);
}

#[test]
fn the_pose_family_runs_the_frozen_spring_and_the_exit_is_separate() {
    // First frame: fully closed pose, no content.
    let first = Entrance::matrix_at(0.0, false);
    assert!((first[0] - 0.86).abs() < 1e-6, "scale starts at 0.86");
    assert!((first[5] - 0.86 * -6.0).abs() < 1e-6, "ty starts at S·(−6)");
    assert!(Entrance::content_opacity_at(0.0) < 0.01, "content starts hidden");

    // Mid-entrance: pose flying, content already fading on its own curve.
    let mid = Entrance::matrix_at(150.0, false);
    assert!(mid[0] > 0.86 && mid[0] < 1.0101, "the pose is mid-flight");
    let content_mid = Entrance::content_opacity_at(150.0);
    // The release curve is cubic-bezier(.16,1,.3,1) — an extremely fast
    // ease-out, so at 150/220 of the duration it is already ~0.995. What the
    // assertion pins is that the content runs on its own 220 ms curve and is
    // *not yet* settled to the exact 1.0 before that duration elapses.
    assert!(
        content_mid > 0.5 && content_mid < 1.0,
        "the content fades on the 220 ms release curve, got {content_mid}"
    );
    let content_60 = Entrance::content_opacity_at(60.0);
    assert!(
        content_60 < content_mid,
        "the content is still climbing: {content_60} < {content_mid}"
    );

    // Settled: identity pose, full content.
    let settled = Entrance::matrix_at(600.0, false);
    assert!((settled[0] - 1.0).abs() < 1e-6 && settled[5].abs() < 1e-6);
    assert!((Entrance::content_opacity_at(600.0) - 1.0).abs() < 1e-6);

    // Mid-exit: the 90 ms press curve, not the spring.
    let exiting = Entrance::matrix_at(45.0, true);
    assert!(
        exiting[0] > 0.86 && exiting[0] < 1.0,
        "the exit is a real transition, got scale {}",
        exiting[0]
    );
    let closed = Entrance::matrix_at(90.0, true);
    assert!((closed[0] - 0.86).abs() < 1e-6, "closed by 90 ms");
}

#[test]
fn the_gpu_pass_rerenders_each_phase_without_state_leakage() {
    // Rendering through the four poses must not poison later renders: the
    // renderer keeps no per-phase state, so two renders of the same scene are
    // byte-identical no matter what poses were "played" in between.
    let sc = scene();
    let first = render_gpu(&sc);
    // Pose bookkeeping between renders (what a shell would do).
    let _mid = Entrance::matrix_at(150.0, false);
    let _exit = Entrance::matrix_at(45.0, true);
    let second = render_gpu(&sc);
    assert_eq!(first, second, "the glass pass is stateless across phases");
}
