//! `oly-qa compare`: diff two captured frames (old vs new renderer) and
//! emit a per-pixel verdict. Task 10's QA compares full screenshots and
//! named crops (text / glass / motion cover) against the shipped renderer.

use serde::Serialize;

/// The verdict of one compared pair.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct CompareVerdict {
    /// Image width (both images must agree).
    pub width: u32,
    pub height: u32,
    /// Mean absolute difference per channel, 0..255.
    pub mean_abs_diff: f64,
    /// Pixels whose any-channel delta exceeds the threshold.
    pub pixels_over_threshold: u64,
    /// The pixel that differed most, for eyeballing the diff.
    pub max_abs_diff: u8,
}

/// Compares two same-size RGBA8 buffers. Different sizes are a hard error:
/// a silent scale match would produce a meaningless diff.
pub fn compare_rgba8(
    a: &[u8],
    b: &[u8],
    width: u32,
    height: u32,
    threshold: u8,
) -> anyhow::Result<CompareVerdict> {
    let expected = width as usize * height as usize * 4;
    anyhow::ensure!(
        a.len() == expected && b.len() == expected,
        "image size mismatch: expected {expected} bytes ({}x{}x4), got {} and {}",
        width,
        height,
        a.len(),
        b.len()
    );
    let mut sum = 0f64;
    let mut over = 0u64;
    let mut max = 0u8;
    for (pa, pb) in a.iter().zip(b.iter()) {
        let d = pa.abs_diff(*pb);
        sum += d as f64;
        max = max.max(d);
    }
    // Count pixels: any channel over threshold flags the pixel.
    for px in 0..(width as usize * height as usize) {
        let p = &a[px * 4..px * 4 + 4];
        let q = &b[px * 4..px * 4 + 4];
        if p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > threshold) {
            over += 1;
        }
    }
    Ok(CompareVerdict {
        width,
        height,
        mean_abs_diff: sum / expected as f64,
        pixels_over_threshold: over,
        max_abs_diff: max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_buffers_are_a_perfect_match() {
        let img = vec![128u8; 16 * 16 * 4];
        let v = compare_rgba8(&img, &img, 16, 16, 2).expect("compare");
        assert_eq!(v.mean_abs_diff, 0.0);
        assert_eq!(v.pixels_over_threshold, 0);
        assert_eq!(v.max_abs_diff, 0);
    }

    #[test]
    fn one_channel_delta_flags_the_pixel() {
        let a = vec![0u8; 16 * 16 * 4];
        let mut b = vec![0u8; 16 * 16 * 4];
        b[10] = 40; // pixel 2, channel 2
        let v = compare_rgba8(&a, &b, 16, 16, 2).expect("compare");
        assert_eq!(v.pixels_over_threshold, 1);
        assert_eq!(v.max_abs_diff, 40);
        assert!(v.mean_abs_diff > 0.0 && v.mean_abs_diff < 1.0);
    }

    #[test]
    fn size_mismatch_is_a_hard_error() {
        let a = vec![0u8; 16 * 16 * 4];
        let b = vec![0u8; 15 * 16 * 4];
        assert!(compare_rgba8(&a, &b, 16, 16, 2).is_err());
    }
}
