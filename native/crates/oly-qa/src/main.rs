//! QA driver. Task 10 turns this into the fixture replay and measurement tool.
//! `video-dump` (Task 9) proves the Media Foundation decoder's NV12 output
//! matches FFmpeg's reference decode of the same fixture. The frame-index
//! mode (`--index N`) is the robust comparison: sequential decode avoids the
//! MP4 edit-list seek quirks that differ between demuxers.

use std::path::{Path, PathBuf};

use anyhow::Context;
use oly_contracts::MediaMs;
use oly_platform_win::media::{readback_nv12, VideoDecoder, VIDEO_MAX_PENDING_SAMPLES};

type D3D = windows::Win32::Graphics::Direct3D11::ID3D11Device;
type D3DCtx = windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext;

fn device() -> anyhow::Result<(D3D, D3DCtx)> {
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    let mut device = None;
    let mut context = None;
    unsafe {
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
    }
    .ok()
    .context("D3D11CreateDevice failed")?;
    Ok((device.unwrap(), context.unwrap()))
}

enum Target {
    /// First presentable frame at-or-past this PTS (seek + pump discipline).
    Pts(i64),
    /// The N-th decoded frame, 0-based (sequential decode, no seek).
    Index(i64),
    /// Diagnostic: print "index pts_ms" for the first N decoded frames.
    PtsList(usize),
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init()
        .ok();
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        eprintln!(
            "usage: oly-qa video-dump <video> <pts_ms|--index N|--pts-list N> [--out <file>] [--raw] [--stats]\n       oly-qa replay <case.json|acceptance.json> [--json]"
        );
        std::process::exit(2);
    };
    if cmd == "replay" {
        let path: PathBuf = args.next().context("replay case or acceptance plan path")?.into();
        let json = args.any(|a| a == "--json");
        return replay(&path, json);
    }
    if cmd != "video-dump" {
        eprintln!("unknown subcommand: {cmd} (video-dump | replay)");
        std::process::exit(2);
    }
    let video: PathBuf = args.next().context("video path")?.into();
    let target = args.next().context("pts_ms or --index N")?;
    let target = if target == "--index" {
        Target::Index(args.next().context("--index value")?.parse()?)
    } else if let Some(stripped) = target.strip_prefix("--index=") {
        Target::Index(stripped.parse()?)
    } else if target == "--pts-list" {
        Target::PtsList(args.next().context("--pts-list value")?.parse()?)
    } else {
        Target::Pts(target.parse()?)
    };
    let mut out = None;
    let mut raw = false;
    let mut stats = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().context("--out value")?)),
            "--raw" => raw = true,
            "--stats" => stats = true,
            other => anyhow::bail!("unknown flag: {other}"),
        }
    }
    let out = out.unwrap_or_else(|| PathBuf::from("frame.ppm"));

    let (device, context) = device()?;
    let mut decoder = VideoDecoder::new(device.clone(), context.clone())?;
    let info = decoder.open(&video)?;
    eprintln!(
        "stream: {}x{} fps {}/{} dur {}ms colour {:?}",
        info.width,
        info.height,
        info.fps_num,
        info.fps_den,
        info.duration_ms.millis(),
        info.colour
    );

    let sample = match target {
        Target::PtsList(n) => {
            // Diagnostic only: print the PTS of each frame in MF's output
            // order and exit. Monotonic ~33ms steps prove display ordering.
            for i in 0..n {
                if !decoder.decode_one()? {
                    eprintln!("stream ended after {i} frames");
                    break;
                }
                while decoder.pending() > 0 {
                    if let Some(s) = decoder.pop_front() {
                        println!("{i} {}", s.pts_ms.millis());
                    }
                }
            }
            return Ok(());
        }
        Target::Index(n) => {
            // Frame-index mode: sequential decode, keep the N-th (0-based)
            // frame. No seek — immune to edit-list quirks; matches ffmpeg's
            // `eq(n,N)`. The queue evicts from the FRONT when full, so
            // draining after every decode keeps exactly the newest frame.
            let mut keep = None;
            for i in 0..=n {
                if !decoder.decode_one()? {
                    anyhow::bail!("stream ended at frame {i}, requested index {n}");
                }
                while decoder.pending() > 0 {
                    if let Some(s) = decoder.pop_front() {
                        keep = Some(s);
                    }
                }
            }
            keep.context("no sample decoded")?
        }
        Target::Pts(pts_ms) => {
            // Seek, then decode until the queue front is at-or-past the
            // requested time, mirroring production pump() drop discipline.
            decoder.seek(MediaMs::from_millis(pts_ms))?;
            let mut guard = 0;
            loop {
                if decoder.front().is_some_and(|s| s.pts_ms.millis() >= pts_ms) {
                    break;
                }
                if !decoder.decode_one()? {
                    anyhow::bail!("stream ended before PTS {pts_ms}");
                }
                while decoder.front().is_some_and(|s| s.pts_ms.millis() < pts_ms) {
                    decoder.pop_front();
                }
                guard += 1;
                if guard > 1000 {
                    anyhow::bail!("decode loop did not converge");
                }
            }
            decoder.pop_front().context("sample at PTS")?
        }
    };
    eprintln!(
        "sample pts={}ms width={} height={} pending={} stats={:?}",
        sample.pts_ms.millis(),
        sample.width,
        sample.height,
        decoder.pending(),
        decoder.stats()
    );

    // Read the NV12 texture back through a staging texture (QA-only path).
    let (luma, chroma, luma_pitch, chroma_pitch) =
        readback_nv12(&device, &context, &sample.texture, sample.width, sample.height)?;

    if raw {
        // Concatenated luma+chroma rows trimmed to width (NV12, no pitch
        // padding) — byte-comparable with `ffmpeg -pix_fmt nv12`.
        let w = sample.width as usize;
        let h = sample.height as usize;
        let mut nv12 = Vec::with_capacity(w * h * 3 / 2);
        for row in 0..h {
            let start = row * luma_pitch;
            nv12.extend_from_slice(&luma[start..start + w]);
        }
        for row in 0..h / 2 {
            let start = row * chroma_pitch;
            nv12.extend_from_slice(&chroma[start..start + w]);
        }
        std::fs::write(&out, &nv12).context("writing raw NV12")?;
        eprintln!("wrote raw NV12 ({} bytes) to {}", nv12.len(), out.display());
    } else {
        let plane = oly_render::video::Nv12Plane {
            width: sample.width as usize,
            height: sample.height as usize,
            luma_pitch,
            chroma_pitch,
            luma: &luma,
            chroma: &chroma,
        };
        let space = oly_render::video::ColourSpace::from(&MFColour(info.colour));
        let colour = oly_render::video::VideoColour::new(&space);
        let rgb = oly_render::video::nv12_to_rgb8(&plane, &colour);
        write_ppm(&out, sample.width, sample.height, &rgb)?;
        eprintln!("wrote PPM to {}", out.display());
    }
    if stats {
        eprintln!("decode stats: {:?}", decoder.stats());
        eprintln!("queue bound: VIDEO_MAX_PENDING_SAMPLES={VIDEO_MAX_PENDING_SAMPLES}");
    }
    Ok(())
}

struct MFColour(oly_platform_win::media::VideoColourInfo);

impl From<&MFColour> for oly_render::video::ColourSpace {
    fn from(c: &MFColour) -> Self {
        oly_render::video::ColourSpace {
            matrix: c.0.matrix,
            range: c.0.range,
            transfer: c.0.transfer.to_string(),
        }
    }
}

fn write_ppm(path: &Path, width: u32, height: u32, rgb: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    write!(f, "P6\n{width} {height}\n255\n")?;
    f.write_all(rgb)?;
    Ok(())
}

/// `oly-qa replay <path>`: the Task 10 packet-script replay, plus the Task 13
/// acceptance plan check.
///
/// A replay case is recognised by its `packets` array; an acceptance plan by
/// its `phases` array. Both are pure data, so this driver never starts the
/// player, never opens a window and never touches the network — it proves the
/// scripted sequence and the acceptance coverage, nothing more.
fn replay(path: &Path, as_json: bool) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;

    if value.get("phases").is_some() {
        let plan = oly_qa::acceptance::AcceptancePlan::load(path)?;
        let report = plan.validate();
        if as_json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!(
                "acceptance: {} ({}s, {} shapes, {} viewports, {} DPI steps)",
                report.name,
                report.total_seconds,
                report.shapes.len(),
                report.viewports.len(),
                report.dpis.len()
            );
            println!(
                "workload: {} track changes, {} panel toggles, {} shape cycles",
                report.tracks, report.panel_toggles, report.shape_cycles
            );
            println!("interactions: {}", report.interactions.join(", "));
            if !report.unsupported.is_empty() {
                println!("unsupported: {}", report.unsupported.join(", "));
            }
            if report.ok() {
                println!("verdict: PASS (every G4 minimum is covered by the plan)");
            } else {
                for gap in &report.gaps {
                    println!("gap: {gap}");
                }
                anyhow::bail!("acceptance plan misses {} requirement(s)", report.gaps.len());
            }
        }
        if !report.ok() {
            anyhow::bail!("acceptance plan misses {} requirement(s)", report.gaps.len());
        }
        return Ok(());
    }

    let case = oly_qa::replay::ReplayCase::load(path)?;
    let result = oly_qa::replay::run(&case)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("replay: {} ({} packets)", case.name, case.packets.len());
        for (index, playhead) in result.playheads.iter().enumerate() {
            println!(
                "{index:>4}  now {:>10.1}  projected {:>10.1}  held {}  seek {}",
                playhead.now_ms, playhead.projected, playhead.held, playhead.seek_pending
            );
        }
    }
    Ok(())
}
