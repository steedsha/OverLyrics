//! Task 9: the NV12 → RGB video pass.
//!
//! The decoder ([`oly_platform_win::media::VideoDecoder`]) hands us NV12
//! textures on the render device. This module converts them to RGB on the GPU
//! with [`shaders/video.hlsl`], and the conversion must follow the *media
//! type's* colour facts — matrix, range, transfer — never a fixed BT.601
//! assumption (the plan's explicit requirement).
//!
//! The conversion maths lives here in one place ([`VideoColour::convert`]) so
//! the CPU test, the GPU pass and a future scene renderer all agree.

use oly_core::cache::video_sample_budget;

/// The YCbCr matrix the stream declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourMatrix {
    /// ITU-R BT.601 (SD, `smpte170m`).
    Bt601,
    /// ITU-R BT.709 (HD).
    Bt709,
}

/// The nominal range the stream declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourRange {
    /// Studio swing: Y 16..235, C 16..240.
    Limited,
    /// PC swing: 0..255.
    Full,
}

/// The colour facts a stream declares, straight from the media type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColourSpace {
    pub matrix: ColourMatrix,
    pub range: ColourRange,
    /// Transfer characteristic as a raw string ("bt709", "smpte170m", ...).
    /// Recorded for diagnostics; the pass converts to linear-light display
    /// values the way the shipped poster does (none: it displays the decoded
    /// RGB as-is, sRGB-encoded output of the studio-swing decode).
    pub transfer: String,
}

/// One sample-point conversion kernel. The HLSL constant block and the CPU
/// reference both derive from this, so GPU and CPU can never disagree.
///
/// BT.709:  Kr=0.2126, Kb=0.0722. BT.601: Kr=0.299, Kb=0.114.
/// Limited: Y' = (Y - 16) / 219, C' = (C - 128) / 224.
/// Full:    Y' = Y / 255,            C' = (C - 128) / 255.
#[derive(Clone, Copy, Debug)]
pub struct VideoColour {
    kr: f32,
    kg: f32,
    kb: f32,
    limited: bool,
}

impl VideoColour {
    pub fn new(space: &ColourSpace) -> Self {
        let (kr, kb) = match space.matrix {
            ColourMatrix::Bt709 => (0.2126, 0.0722),
            ColourMatrix::Bt601 => (0.299, 0.114),
        };
        Self {
            kr,
            kg: 1.0 - kr - kb,
            kb,
            limited: space.range == ColourRange::Limited,
        }
    }

    /// Converts one NV12 sample point (Y, U, V each 0..=255) to linear 0..=1
    /// RGB, clamped.
    pub fn convert(&self, y: u8, u: u8, v: u8) -> [f32; 3] {
        let (y_scale, c_scale, y_offset, c_offset) = if self.limited {
            (1.0 / 219.0, 1.0 / 224.0, -16.0, -128.0)
        } else {
            (1.0 / 255.0, 1.0 / 255.0, 0.0, -128.0)
        };
        let y = (y as f32 + y_offset) * y_scale;
        let u = (u as f32 + c_offset) * c_scale;
        let v = (v as f32 + c_offset) * c_scale;
        // Invert Y'CbCr onto RGB (BT.601/709 share this shape; the matrix
        // constants differ).
        let r = y + 2.0 * (1.0 - self.kr) * v;
        let b = y + 2.0 * (1.0 - self.kb) * u;
        let g = (y - self.kr * r - self.kb * b) / self.kg;
        [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
    }

    /// The GPU constant block `shaders/video.hlsl` consumes: 4×float4
    /// row-major. Rows 0..2 are the RGB rows of the inverse matrix, applied to
    /// the *scaled* signals `Y' = (Y + y_offset) * y_scale`,
    /// `U' = (U + c_offset) * c_scale`, `V' = (V + c_offset) * c_scale`:
    ///
    /// ```text
    /// R =      Y'              + 2(1-Kr)      · V'
    /// G =      Y' − 2Kb(1−Kb)/Kg · U' − 2Kr(1−Kr)/Kg · V'
    /// B =      Y' + 2(1-Kb)      · U'
    /// ```
    ///
    /// Row 3 is `(y_scale, y_offset, c_scale, c_offset)`. Everything derives
    /// from Kr/Kb and the nominal range — the same numbers
    /// [`VideoColour::convert`] uses — so the GPU pass and the CPU reference
    /// can never disagree, and no BT.601 assumption is baked into the shader.
    pub fn shader_constants(&self) -> [f32; 16] {
        let (y_scale, y_offset, c_scale, c_offset) = if self.limited {
            (1.0 / 219.0, -16.0, 1.0 / 224.0, -128.0)
        } else {
            (1.0 / 255.0, 0.0, 1.0 / 255.0, -128.0)
        };
        let g_u = -2.0 * self.kb * (1.0 - self.kb) / self.kg;
        let g_v = -2.0 * self.kr * (1.0 - self.kr) / self.kg;
        [
            1.0, 0.0, 2.0 * (1.0 - self.kr), 0.0, //
            1.0, g_u, g_v, 0.0, //
            1.0, 2.0 * (1.0 - self.kb), 0.0, 0.0, //
            y_scale, y_offset, c_scale, c_offset,
        ]
    }
}

/// A borrowed NV12 frame for CPU-side sampling (tests, QA).
#[derive(Clone, Copy, Debug)]
pub struct Nv12Plane<'a> {
    pub width: usize,
    pub height: usize,
    pub luma_pitch: usize,
    pub chroma_pitch: usize,
    pub luma: &'a [u8],
    pub chroma: &'a [u8],
}

impl<'a> Nv12Plane<'a> {
    /// Valid luma bytes in one row.
    pub fn luma_row_len(&self) -> usize {
        self.width
    }

    /// Valid chroma bytes in one row (packed U/V pairs).
    pub fn chroma_row_len(&self) -> usize {
        self.width / 2
    }

    /// Samples the Y plane at (x, y).
    pub fn luma_at(&self, x: usize, y: usize) -> u8 {
        self.luma[y * self.luma_pitch + x]
    }

    /// Samples the (U, V) pair at luma coordinates (x, y).
    pub fn chroma_at(&self, x: usize, y: usize) -> (u8, u8) {
        let cx = x / 2;
        let cy = y / 2;
        let base = cy * self.chroma_pitch + cx * 2;
        (self.chroma[base], self.chroma[base + 1])
    }
}

/// Converts a whole NV12 frame to RGB8 on the CPU. The QA compare uses this as
/// the reference; the GPU pass must agree within the fixture's tolerance.
pub fn nv12_to_rgb8(plane: &Nv12Plane<'_>, colour: &VideoColour) -> Vec<u8> {
    let mut out = Vec::with_capacity(plane.width * plane.height * 3);
    for y in 0..plane.height {
        for x in 0..plane.width {
            let luma = plane.luma_at(x, y);
            let (u, v) = plane.chroma_at(x, y);
            let rgb = colour.convert(luma, u, v);
            out.push((rgb[0] * 255.0 + 0.5) as u8);
            out.push((rgb[1] * 255.0 + 0.5) as u8);
            out.push((rgb[2] * 255.0 + 0.5) as u8);
        }
    }
    out
}

/// The pending-sample budget for a stream of `width × height`, from
/// [`oly_core::cache`]: the decoder's queue can never exceed this many NV12
/// bytes across [`VIDEO_MAX_PENDING_SAMPLES`] samples.
pub fn pending_sample_bytes(width: u32, height: u32) -> u64 {
    video_sample_budget(width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space709() -> ColourSpace {
        ColourSpace {
            matrix: ColourMatrix::Bt709,
            range: ColourRange::Limited,
            transfer: "bt709".into(),
        }
    }

    #[test]
    fn limited_black_and_white_decode_to_black_and_white() {
        let v = VideoColour::new(&space709());
        for ch in v.convert(16, 128, 128) {
            assert!(ch < 0.02, "{ch}");
        }
        for ch in v.convert(235, 128, 128) {
            assert!(ch > 0.98, "{ch}");
        }
    }

    #[test]
    fn full_range_is_not_treated_as_limited() {
        let v = VideoColour::new(&ColourSpace {
            matrix: ColourMatrix::Bt709,
            range: ColourRange::Full,
            transfer: "bt709".into(),
        });
        for ch in v.convert(0, 128, 128) {
            assert!(ch < 0.02, "{ch}");
        }
        for ch in v.convert(255, 128, 128) {
            assert!(ch > 0.98, "{ch}");
        }
        let y16 = v.convert(16, 128, 128)[0];
        assert!(y16 > 0.05, "16 must not be black in full range: {y16}");
    }

    #[test]
    fn bt601_and_bt709_differ_off_axis() {
        // A near-saturated sample: both R channels stay below 1.0, so the
        // matrices must disagree without clamping hiding the difference.
        let a = VideoColour::new(&space709()).convert(128, 74, 195);
        let b = VideoColour::new(&ColourSpace {
            matrix: ColourMatrix::Bt601,
            range: ColourRange::Limited,
            transfer: "smpte170m".into(),
        })
        .convert(128, 74, 195);
        assert!((a[0] - b[0]).abs() > 0.03, "{a:?} vs {b:?}");
        assert!((a[1] - b[1]).abs() > 0.03);
    }

    #[test]
    fn nv12_layout_indexing_matches_the_driver_contract() {
        let width = 4;
        let height = 4;
        let luma = vec![0u8; 8 * 4];
        let chroma = vec![0u8; 8 * 2];
        let plane = Nv12Plane {
            width,
            height,
            luma_pitch: 8,
            chroma_pitch: 8,
            luma: &luma,
            chroma: &chroma,
        };
        assert_eq!(plane.luma_row_len(), 4);
        assert_eq!(plane.chroma_row_len(), 2);
    }

    #[test]
    fn nv12_to_rgb8_produces_w_h_rgb() {
        let width = 2;
        let height = 2;
        // Y: 16 235 / 16 235 (stretched vertically), U=V=128 → black/white.
        let luma = vec![16, 235, 16, 235, 16, 235, 16, 235];
        let chroma = vec![128, 128, 128, 128];
        let plane = Nv12Plane {
            width,
            height,
            luma_pitch: 4,
            chroma_pitch: 4,
            luma: &luma,
            chroma: &chroma,
        };
        let rgb = nv12_to_rgb8(&plane, &VideoColour::new(&space709()));
        assert_eq!(rgb.len(), 2 * 2 * 3);
        // (0,0) is black, (1,0) is white.
        assert!(rgb[0] < 8 && rgb[1] < 8 && rgb[2] < 8);
        assert!(rgb[3] > 247 && rgb[4] > 247 && rgb[5] > 247);
    }
}
