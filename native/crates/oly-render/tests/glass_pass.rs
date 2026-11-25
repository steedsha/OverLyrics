//! GPU glass pass parity: the D3D11 chain must reproduce the software
//! reference (`composite_glass`) within a bounded 8-bit band on the panel
//! interior and keep the rim/shape structure identical.
//!
//! The reference is not the browser here — it is `glass/mod.rs`, whose chain
//! was frozen against the shipped CSS stack (`blur(6px) saturate(1.12)
//! contrast(1.05)` + tint + rim). The GPU pass implements the same chain as
//! three D3D11 draws; this test proves the GPU bytes match the reference
//! bytes, so any future drift in either is caught against the frozen math.

use oly_render::glass::gpu::{GlassRenderer, GlassRequest};
use oly_render::glass::{Material, Rect, composite_glass, rounded_rect_sdf};

/// Maximum per-channel delta tolerated on pixels fully inside the panel and
/// away from the rim band and the rounded corners.
const INTERIOR_TOLERANCE: i64 = 1;

/// How many sampled interior points may exceed the interior tolerance (the
/// two paths quantise the blur slightly differently: the GPU samples with a
/// bilinear tap grid at 19 offsets, the CPU with exact texel offsets; both
/// round once at the end). Zero is the norm; the budget exists so a single
/// boundary pixel cannot fail the suite.
const INTERIOR_OUTLIERS: usize = 8;

/// The regions each test renders. Small enough to keep the whole suite under
/// a second on the CI GPU, large enough to hold a flat interior, two hard
/// scene edges and four rounded corners.
const PANEL_W: u32 = 96;
const PANEL_H: u32 = 64;
const MARGIN: u32 = 10; // >= 3*sigma = 9, matching the real capture margin.

struct Scene {
    region: (u32, u32),
    pixels: Vec<u8>,
}

/// A synthetic scene with everything the assertions need: a flat field, a
/// bright square in one corner (to see the blur bleed), a hard vertical edge
/// (to grade the blur), and an alpha channel that is fully opaque everywhere.
fn scene() -> Scene {
    let region = (PANEL_W + MARGIN * 2, PANEL_H + MARGIN * 2);
    let mut pixels = vec![0u8; (region.0 * region.1 * 4) as usize];
    for y in 0..region.1 {
        for x in 0..region.0 {
            // Base field with a horizontal ramp plus a hard vertical edge at
            // half width; a bright blob in the top-left margin area.
            let ramp = ((x * 255) / region.0) as u8;
            let (r, g, b) = if x >= region.0 / 2 {
                (ramp, ramp, 255u8.saturating_sub(ramp))
            } else {
                (40u8, 90u8.saturating_add(ramp / 2), 60u8)
            };
            let (r, g, b) = if x < 24 && y < 24 {
                (250, 245, 240)
            } else {
                (r, g, b)
            };
            let i = ((y * region.0 + x) * 4) as usize;
            pixels[i] = r;
            pixels[i + 1] = g;
            pixels[i + 2] = b;
            pixels[i + 3] = 255;
        }
    }
    Scene { region, pixels }
}

fn panel_rect() -> Rect {
    Rect {
        left: MARGIN as f64,
        top: MARGIN as f64,
        width: PANEL_W as f64,
        height: PANEL_H as f64,
    }
}

fn material() -> Material {
    Material::popover_clear()
}

/// Renders the same request through both paths and returns two region-sized
/// byte buffers: the reference placed at its panel offset inside an otherwise
/// zeroed region, and the GPU output as-is.
fn render_both(sc: &Scene) -> (Vec<u8>, Vec<u8>) {
    let material = material();
    let panel = panel_rect();

    let panel_bytes = composite_glass(&sc.pixels, sc.region, panel, &material);
    let (pw, ph) = (panel.width.round() as u32, panel.height.round() as u32);
    let (pw, ph) = (pw.min(PANEL_W), ph.min(PANEL_H));
    let mut reference = vec![0u8; (sc.region.0 * sc.region.1 * 4) as usize];
    for py in 0..ph {
        for px in 0..pw {
            let src = ((py * pw + px) * 4) as usize;
            let dst = (((py + MARGIN) * sc.region.0 + (px + MARGIN)) * 4) as usize;
            reference[dst..dst + 4].copy_from_slice(&panel_bytes[src..src + 4]);
        }
    }

    let mut gpu = GlassRenderer::new_own().expect("a hardware D3D11 device");
    let request = GlassRequest {
        material: &material,
        panel,
        region: sc.region,
        scene: &sc.pixels,
    };
    gpu.render(&request)
        .expect("the glass GPU pass renders");
    let gpu_pixels = gpu
        .read_output(sc.region)
        .expect("reading the glass output");
    (reference, gpu_pixels)
}

/// Interior pixel delta stats between the two region-sized buffers. Interior
/// means 2px in from every panel edge (clears the rim band and its AA) and
/// 3px inside the corner curvature (sdf < -3).
fn delta_stats(
    a: &[u8],
    b: &[u8],
    region: (u32, u32),
    panel: Rect,
    radius: f64,
) -> (i64, usize, i64, (u32, u32)) {
    let mut max_delta = 0i64;
    let mut outliers = 0usize;
    let mut worst = 0i64;
    let mut worst_at = (0u32, 0u32);
    for py in 0..PANEL_H {
        for px in 0..PANEL_W {
            if px < 2 || py < 2 || px + 2 >= PANEL_W || py + 2 >= PANEL_H {
                continue;
            }
            let sdf = rounded_rect_sdf(
                (px as f64 + 0.5, py as f64 + 0.5),
                (panel.width, panel.height),
                radius,
            );
            if sdf > -3.0 {
                continue;
            }
            let i = (((py + MARGIN) * region.0 + (px + MARGIN)) * 4) as usize;
            for c in 0..3 {
                let d = (a[i + c] as i64 - b[i + c] as i64).abs();
                if d > max_delta {
                    max_delta = d;
                    worst = d;
                    worst_at = (px, py);
                }
                if d > INTERIOR_TOLERANCE {
                    outliers += 1;
                }
            }
        }
    }
    (max_delta, outliers, worst, worst_at)
}

#[test]
fn the_gpu_chain_matches_the_reference_interior() {
    let sc = scene();
    let (reference, gpu_pixels) = render_both(&sc);
    let material = material();
    let (max_delta, outliers, worst, worst_at) =
        delta_stats(&reference, &gpu_pixels, sc.region, panel_rect(), material.radius);
    assert!(
        outliers <= INTERIOR_OUTLIERS,
        "{outliers} interior channel samples exceeded {INTERIOR_TOLERANCE} (max {max_delta} at {worst_at:?})"
    );
    assert!(
        max_delta <= 4,
        "worst interior delta {worst} at {worst_at:?} is outside the plausible quantisation band"
    );
}

#[test]
fn the_gpu_output_is_sized_to_the_region() {
    let sc = scene();
    let (_, gpu_pixels) = render_both(&sc);
    assert_eq!(
        gpu_pixels.len(),
        (sc.region.0 * sc.region.1 * 4) as usize,
        "the GPU pass must return the whole region"
    );
    // And the panel area must be opaque where the reference is opaque: the
    // GPU writes the shape mask itself, so corners outside the radius are
    // transparent in the region output too.
    let panel = panel_rect();
    let material = material();
    let mut mismatches = 0usize;
    for py in 0..PANEL_H {
        for px in 0..PANEL_W {
            let sdf = rounded_rect_sdf(
                (px as f64 + 0.5, py as f64 + 0.5),
                (panel.width, panel.height),
                material.radius,
            );
            let i = (((py + MARGIN) * sc.region.0 + (px + MARGIN)) * 4 + 3) as usize;
            let gpu_alpha = gpu_pixels[i];
            if sdf > 0.5 {
                if gpu_alpha != 0 {
                    mismatches += 1;
                }
            } else if gpu_alpha != 255 {
                mismatches += 1;
            }
        }
    }
    assert_eq!(mismatches, 0, "the rounded shape must match the reference");
}

#[test]
fn the_gpu_rim_and_reference_rim_agree_on_brightness_ordering() {
    // The rim is excluded from the numeric interior test; here we only prove
    // the GPU keeps its qualitative structure: the top rim is brighter than
    // the interior, the bottom rim darker, both paths.
    let sc = scene();
    let (reference, gpu_pixels) = render_both(&sc);
    let at = |buf: &[u8], x: u32, y: u32| -> u8 {
        let i = ((y * sc.region.0 + x) * 4) as usize;
        buf[i]
    };
    let pw_mid = MARGIN + PANEL_W / 2;
    let top_y = MARGIN;
    let bottom_y = MARGIN + PANEL_H - 1;
    let inner_y = MARGIN + 8;
    for (label, buf) in [("reference", &reference), ("gpu", &gpu_pixels)] {
        let top = at(buf, pw_mid, top_y);
        let bottom = at(buf, pw_mid, bottom_y);
        let inner = at(buf, pw_mid, inner_y);
        assert!(
            top > inner + 20,
            "{label}: top rim {top} must be brighter than the interior {inner}"
        );
        assert!(
            bottom < inner - 2,
            "{label}: bottom rim {bottom} must be darker than the interior {inner}"
        );
    }
}

#[test]
fn repeated_renders_are_stable() {
    // Same input twice: the GPU path must be deterministic frame to frame.
    let sc = scene();
    let material = material();
    let panel = panel_rect();
    let mut renderer = GlassRenderer::new_own().expect("a hardware D3D11 device");
    let request = GlassRequest {
        material: &material,
        panel,
        region: sc.region,
        scene: &sc.pixels,
    };
    renderer.render(&request).expect("first render");
    let first = renderer.read_output(sc.region).expect("first read");
    renderer.render(&request).expect("second render");
    let second = renderer.read_output(sc.region).expect("second read");
    assert_eq!(first, second, "the glass pass must be deterministic");
}

#[test]
fn two_region_sizes_reuse_and_rebuild_the_scratch() {
    // The scratch targets must be rebuilt when the region changes and the
    // output must still match the reference for both sizes.
    let material = material();
    let mut renderer = GlassRenderer::new_own().expect("a hardware D3D11 device");
    for &(mw, mh) in &[(MARGIN, MARGIN), (16u32, 14u32)] {
        let region = (PANEL_W + mw * 2, PANEL_H + mh * 2);
        // The panel must follow the margin so the comparison offset below
        // matches where the panel actually sits; a fixed (10, 10) panel under
        // a (16, 14) offset comparison would read the wrong texels.
        let panel = Rect {
            left: mw as f64,
            top: mh as f64,
            ..panel_rect()
        };
        let mut pixels = vec![0u8; (region.0 * region.1 * 4) as usize];
        for y in 0..region.1 {
            for x in 0..region.0 {
                let v = ((x + y * 3) % 251) as u8;
                let i = ((y * region.0 + x) * 4) as usize;
                pixels[i] = v;
                pixels[i + 1] = v;
                pixels[i + 2] = (251 - v) as u8;
                pixels[i + 3] = 255;
            }
        }
        let request = GlassRequest {
            material: &material,
            panel,
            region,
            scene: &pixels,
        };
        renderer
            .render(&request)
            .expect("rendering after a region change");
        let gpu_pixels = renderer.read_output(region).expect("reading the output");

        let panel_bytes = composite_glass(&pixels, region, panel, &material);
        let (pw, ph) = (panel.width.round() as u32, panel.height.round() as u32);
        let mut worst = 0i64;
        for py in 0..ph.min(PANEL_H) {
            for px in 0..pw.min(PANEL_W) {
                let sdf = rounded_rect_sdf(
                    (px as f64 + 0.5, py as f64 + 0.5),
                    (panel.width, panel.height),
                    material.radius,
                );
                if sdf > -3.0 || px < 2 || py < 2 || px + 2 >= PANEL_W || py + 2 >= PANEL_H {
                    continue;
                }
                let src = ((py * pw + px) * 4) as usize;
                let dst = (((py + mh) * region.0 + (px + mw)) * 4) as usize;
                for c in 0..3 {
                    let d = (panel_bytes[src + c] as i64 - gpu_pixels[dst + c] as i64).abs();
                    worst = worst.max(d);
                }
            }
        }
        assert!(
            worst <= 4,
            "region {region:?}: worst interior delta {worst} exceeds the quantisation band"
        );
    }
}

#[test]
fn the_renderer_survives_dropping_its_scratch() {
    // Dropping the renderer releases its scratch targets; a fresh renderer on
    // the same input must still produce the same bytes.
    let sc = scene();
    let material = material();
    let panel = panel_rect();
    let request = GlassRequest {
        material: &material,
        panel,
        region: sc.region,
        scene: &sc.pixels,
    };
    let first = {
        let mut renderer = GlassRenderer::new_own().expect("a hardware D3D11 device");
        renderer.render(&request).expect("first render");
        renderer.read_output(sc.region).expect("first read")
    };
    let second = {
        let mut renderer = GlassRenderer::new_own().expect("a hardware D3D11 device");
        renderer.render(&request).expect("second render");
        renderer.read_output(sc.region).expect("second read")
    };
    assert_eq!(first, second, "a torn-down renderer must be replaceable");
}

#[test]
fn liquid_refraction_changes_edges_but_keeps_the_frosted_center() {
    let sc=scene(); let m=material(); let request=GlassRequest {material:&m,panel:panel_rect(),region:sc.region,scene:&sc.pixels};
    let mut renderer=GlassRenderer::new_own().expect("D3D11 glass");
    renderer.render(&request).unwrap();let base=renderer.read_output(sc.region).unwrap();
    renderer.set_liquid(3.0,12.0,0.16,1.0);
    renderer.render(&request).unwrap();let liquid=renderer.read_output(sc.region).unwrap();
    let mut changed=0;let mut softened=0;
    for y in 0..PANEL_H {for x in 0..PANEL_W {
        let sdf=rounded_rect_sdf((x as f64+0.5,y as f64+0.5),(PANEL_W as f64,PANEL_H as f64),m.radius);
        let i=(((y+MARGIN)*sc.region.0+x+MARGIN)*4) as usize;
        if sdf < -16.0 {for c in 0..3 {assert!((base[i+c] as i32-liquid[i+c] as i32).abs()<=1);}}
        if sdf < -1.5 && sdf > -10.0 && base[i..i+3]!=liquid[i..i+3] {changed+=1;}
        if liquid[i+3]>0 && liquid[i+3]<255 {softened+=1;}
    }}
    assert!(changed>40,"the edge must refract the scene");
    assert!(softened>0,"the optical contour must be antialiased");
    renderer.set_liquid(3.0,12.0,0.16,0.5);
    renderer.render(&request).unwrap();let faded=renderer.read_output(sc.region).unwrap();
    let center=(((MARGIN+PANEL_H/2)*sc.region.0+MARGIN+PANEL_W/2)*4) as usize;
    assert!((faded[center+3] as i32-128).abs()<=1,"exit opacity must reach the actual GPU output");
    if let Some(dir)=std::env::var_os("OLY_LIQUID_EVIDENCE") {
        let dir=std::path::PathBuf::from(dir);std::fs::create_dir_all(&dir).unwrap();
        image::save_buffer(dir.join("legacy-optics.png"),&base,sc.region.0,sc.region.1,image::ColorType::Rgba8).unwrap();
        image::save_buffer(dir.join("liquid-optics.png"),&liquid,sc.region.0,sc.region.1,image::ColorType::Rgba8).unwrap();
    }

}

#[test]
fn frosted_blur_keeps_full_support_at_fractional_dpi() {
    let sc=scene();
    for scale in [1.0,1.5] {
        let mut m=material();m.blur_px=12.0*scale;
        let panel_bytes=composite_glass(&sc.pixels,sc.region,panel_rect(),&m);
        let mut reference=vec![0u8;(sc.region.0*sc.region.1*4) as usize];
        for y in 0..PANEL_H {
            let src=(y*PANEL_W*4) as usize;
            let dst=(((y+MARGIN)*sc.region.0+MARGIN)*4) as usize;
            reference[dst..dst+(PANEL_W*4) as usize].copy_from_slice(&panel_bytes[src..src+(PANEL_W*4) as usize]);
        }
        let mut renderer=GlassRenderer::new_own().unwrap();
        renderer.render(&GlassRequest{material:&m,panel:panel_rect(),region:sc.region,scene:&sc.pixels}).unwrap();
        let actual=renderer.read_output(sc.region).unwrap();
        let (max_delta,outliers,_,_)=delta_stats(&reference,&actual,sc.region,panel_rect(),m.radius);
        assert!(max_delta<=INTERIOR_TOLERANCE && outliers<=INTERIOR_OUTLIERS,"frosted scale {scale}: max {max_delta}, outliers {outliers}");
    }
}

#[test]
fn moving_pointer_light_changes_the_rim_without_washing_out_the_center() {
    let sc=scene();let m=material();let request=GlassRequest{material:&m,panel:panel_rect(),region:sc.region,scene:&sc.pixels};
    let mut renderer=GlassRenderer::new_own().unwrap();renderer.set_liquid(3.0,12.0,0.16,1.0);
    renderer.set_light(0.1,0.2,1.0);renderer.render(&request).unwrap();let left=renderer.read_output(sc.region).unwrap();
    renderer.set_light(0.9,0.8,1.0);renderer.render(&request).unwrap();let right=renderer.read_output(sc.region).unwrap();
    let mut changed=0;
    for y in 0..PANEL_H {for x in 0..PANEL_W {
        let sdf=rounded_rect_sdf((x as f64+0.5,y as f64+0.5),(PANEL_W as f64,PANEL_H as f64),m.radius);
        let i=(((y+MARGIN)*sc.region.0+x+MARGIN)*4) as usize;
        if sdf < -16.0 {assert_eq!(&left[i..i+4],&right[i..i+4]);}
        if sdf > -10.0 && sdf < -0.5 && left[i..i+3]!=right[i..i+3] {changed+=1;}
    }}
    assert!(changed>40,"the highlight must follow the pointer");
    if let Some(dir)=std::env::var_os("OLY_LIQUID_EVIDENCE") {let dir=std::path::PathBuf::from(dir);std::fs::create_dir_all(&dir).unwrap();image::save_buffer(dir.join("light-left.png"),&left,sc.region.0,sc.region.1,image::ColorType::Rgba8).unwrap();image::save_buffer(dir.join("light-right.png"),&right,sc.region.0,sc.region.1,image::ColorType::Rgba8).unwrap();}
}
