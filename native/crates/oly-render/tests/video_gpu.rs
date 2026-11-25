//! Task 10 red test: the GPU NV12 → BGRA8 video pass must agree with the
//! proven CPU conversion, and must respect the declared colour facts.
//!
//! The decoder side is already proven bit-exact against FFmpeg (Task 9 QA).
//! This test proves the *conversion* half: same device, no CPU round trip in
//! the path (the input is an NV12 texture created on the device, the output a
//! BGRA8 texture the fork can composite through `register_gpu_surface`).

#![cfg(target_os = "windows")]

use oly_render::video::{ColourMatrix, ColourRange, ColourSpace, VideoColour};

fn headless_device() -> Option<(
    windows::Win32::Graphics::Direct3D11::ID3D11Device,
    windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
)> {
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    let mut device = None;
    let mut context = None;
    let hr = unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[
                D3D_FEATURE_LEVEL_11_1,
                D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_10_1,
            ]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    };
    if hr.is_err() {
        eprintln!("no hardware D3D11 device; skipping GPU test");
        return None;
    }
    Some((device.unwrap(), context.unwrap()))
}

/// A small synthetic NV12 frame with known values, converted both ways.
#[test]
fn gpu_pass_matches_cpu_reference() {
    let Some((device, context)) = headless_device() else {
        return;
    };

    const W: u32 = 64;
    const H: u32 = 64;

    // BT.709 limited — the fixture's declared facts.
    let space = ColourSpace {
        matrix: ColourMatrix::Bt709,
        range: ColourRange::Limited,
        transfer: "bt709".into(),
    };
    let colour = VideoColour::new(&space);

    // Build a synthetic NV12 CPU frame: luma gradient plus chroma quadrants.
    let mut luma = vec![0u8; (W * H) as usize];
    let mut chroma = vec![0u8; (W * H / 4 * 2) as usize];
    for y in 0..H {
        for x in 0..W {
            luma[(y * W + x) as usize] = ((x * 3 + y * 5) % 256) as u8;
        }
    }
    for cy in 0..H / 2 {
        for cx in 0..W / 2 {
            let i = (cy * (W / 2) + cx) as usize;
            chroma[i * 2] = if cx < W / 4 { 60 } else { 190 }; // U
            chroma[i * 2 + 1] = if cy < H / 4 { 90 } else { 170 }; // V
        }
    }

    // CPU reference.
    let plane = oly_render::video::Nv12Plane {
        width: W as usize,
        height: H as usize,
        luma_pitch: W as usize,
        chroma_pitch: W as usize,
        luma: &luma,
        chroma: &chroma,
    };
    let cpu = oly_render::video::nv12_to_rgb8(&plane, &colour);

    // GPU pass.
    let renderer =
        match oly_render::video_gpu::VideoRenderer::new(&device, &context, W, H, &space) {
            Ok(r) => r,
            Err(e) => panic!("VideoRenderer::new: {e:#}"),
        };
    renderer
        .upload_nv12(&context, &luma, &chroma)
        .expect("upload_nv12");
    let output = renderer.render(&context).expect("render");
    let bgra = read_bgra8(&device, &context, &output, W, H);

    // Compare: BGRA (B first) against the CPU RGB reference.
    let mut worst = (0u8, 0u8, 0u8);
    for (i, px) in bgra.chunks_exact(4).enumerate() {
        let (b, g, r) = (px[0], px[1], px[2]);
        let (cr, cg, cb) = (cpu[i * 3], cpu[i * 3 + 1], cpu[i * 3 + 2]);
        let d = (
            r.abs_diff(cr),
            g.abs_diff(cg),
            b.abs_diff(cb),
        );
        if d.0 > worst.0 || d.1 > worst.1 || d.2 > worst.2 {
            worst = d;
        }
        assert!(
            d.0 <= 1 && d.1 <= 1 && d.2 <= 1,
            "pixel {i}: gpu=({r},{g},{b}) cpu=({cr},{cg},{cb})"
        );
    }
    let _ = worst;

    // The pass must be repeatable: a second render gives identical bytes.
    let output2 = renderer.render(&context).expect("render 2");
    let bgra2 = read_bgra8(&device, &context, &output2, W, H);
    assert_eq!(bgra, bgra2, "second render diverged");
}

/// A distinct colour space must produce a visibly different result on the
/// same luma/chroma data — the matrix/range actually flow through.
#[test]
fn gpu_pass_honours_colour_space() {
    let Some((device, context)) = headless_device() else {
        return;
    };

    const W: u32 = 32;
    const H: u32 = 32;

    // Saturated chroma where 709 vs 601 disagree (same sample family as the
    // CPU colour tests: (Y=128, U≈74, V≈195)).
    let luma = vec![128u8; (W * H) as usize];
    let mut chroma = vec![0u8; (W * H / 4 * 2) as usize];
    for i in 0..(W * H / 4) as usize {
        chroma[i * 2] = 74; // U
        chroma[i * 2 + 1] = 195; // V
    }

    let make = |matrix, range| -> Vec<u8> {
        let space = ColourSpace {
            matrix,
            range,
            transfer: "bt709".into(),
        };
        let renderer =
            oly_render::video_gpu::VideoRenderer::new(&device, &context, W, H, &space)
                .expect("VideoRenderer::new");
        renderer
            .upload_nv12(&context, &luma, &chroma)
            .expect("upload");
        let out = renderer.render(&context).expect("render");
        read_bgra8(&device, &context, &out, W, H)
    };

    let bt709 = make(ColourMatrix::Bt709, ColourRange::Limited);
    let bt601 = make(ColourMatrix::Bt601, ColourRange::Limited);

    // At least the red channel must differ by more than the rounding slack.
    let mut max_dr = 0i32;
    for (a, b) in bt709.chunks_exact(4).zip(bt601.chunks_exact(4)) {
        max_dr = max_dr.max((a[2] as i32 - b[2] as i32).abs());
    }
    assert!(
        max_dr > 3,
        "BT.709 vs BT.601 outputs nearly identical (max dR={max_dr}); \
         the colour space is not flowing through"
    );
}

/// The upload validates row pitch: a short luma plane must be an error, not
/// a silent garbage read.
#[test]
fn upload_rejects_short_planes() {
    let Some((device, context)) = headless_device() else {
        return;
    };
    let space = ColourSpace {
        matrix: ColourMatrix::Bt709,
        range: ColourRange::Limited,
        transfer: "bt709".into(),
    };
    let renderer = oly_render::video_gpu::VideoRenderer::new(&device, &context, 16, 16, &space)
        .expect("VideoRenderer::new");
    let short = vec![0u8; 16 * 15]; // one row short
    assert!(
        renderer.upload_nv12(&context, &short, &short).is_err(),
        "short luma plane accepted"
    );
}

fn read_bgra8(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    width: u32,
    height: u32,
) -> Vec<u8> {
    use windows::Win32::Graphics::Direct3D11::*;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    assert_eq!(desc.Format, windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM);
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: (D3D11_CPU_ACCESS_READ.0) as u32,
        MiscFlags: 0,
        ..desc
    };
    let mut staging = None;
    unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
        .expect("CreateTexture2D staging");
    let staging = staging.expect("no staging texture");
    unsafe { context.CopyResource(&staging, texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
        .expect("Map staging");
    let row = mapped.RowPitch as usize;
    let mut bytes = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let start = y * row;
        bytes.extend_from_slice(
            &unsafe {
                std::slice::from_raw_parts(
                    mapped.pData as *const u8,
                    row * height as usize,
                )
            }[start..start + width as usize * 4],
        );
    }
    unsafe { context.Unmap(&staging, 0) };
    bytes
}
