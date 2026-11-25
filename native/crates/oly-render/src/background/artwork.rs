//! Cover preparation: framed/full-bleed probe, padding, blur and downsample.
//!
//! Ported from `src/windows/lyrics/background/artwork.ts` at commit 708f6b1.
//! The reference composes an 800x800 artwork square into an 1160x1160 canvas
//! filled with a single "field" colour chosen from the cover's border (framed
//! artwork) or its mean (full-bleed artwork), blurs that canvas with a 60px
//! Gaussian, then downsamples the central 800x800 to the 200px working source
//! the shader pass consumes.
//!
//! Two things are deliberately *not* re-derived here:
//!
//! * The 32x32 cover sample the colour policy reads is taken from the original
//!   decoded cover, exactly like `colourArtwork(source, IMG, inner)` does, so
//!   the gain stays a function of the cover rather than of the blur.
//! * The resampling kernel. Canvas2D's `imageSmoothingQuality:'high'` is an
//!   unspecified cubic; this port uses an area-average box for downscales and a
//!   triangle for upscales. The geometry, the field colour and the gain are
//!   compared exactly against the frozen browser output; the resampling delta is
//!   measured and recorded in `docs/gpui/FIDELITY.md` instead of being hidden by
//!   a tolerance that would also hide a real geometry error.
//!
//! The reference's own parity harness accepted a maximum channel delta of 8/255
//! on this 200px source and a gain delta under 0.001 (`tests/e2e/
//! immersion-parity.mjs`); those are the numbers the Rust test holds this port
//! to, so the comparison stays comparable across the migration.

use oly_core::cache::{BitmapLedger, TempBitmap};

use super::colour::{ColourParams, CoverSample, SAMPLE_SIZE, SIZE};

/// The square the reference composes the artwork into, before padding.
const CANVAS: u32 = 800;
/// Padding around that square, in canvas pixels.
const PAD: u32 = 180;
/// Blur standard deviation, in padded-canvas pixels.
///
/// Canvas2D's `filter: blur(<length>)` takes the standard deviation directly
/// (unlike `text-shadow`, whose radius is twice sigma).
const BLUR_SIGMA: f64 = 60.0;
/// Downsample factor from the 800px canvas to the 200px working source.
const WORK_SCALE: u32 = 4;
/// Side of the probe that decides framed vs full-bleed, in pixels.
pub const PROBE: u32 = 160;

/// A tightly packed, top-left-origin RGBA8 image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    /// # Panics
    ///
    /// Panics when `pixels` is not exactly `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        assert_eq!(
            pixels.len(),
            width as usize * height as usize * 4,
            "image pixels must be {width}x{height} RGBA8"
        );
        Self {
            width,
            height,
            pixels,
        }
    }

    /// A flat opaque image painted from the exact colour the reference passes
    /// to `fillStyle`.
    ///
    /// `artwork.ts` interpolates the probe mean into an `rgb(r,g,b)` string
    /// without rounding it — `field.join(',')` prints the full float — and the
    /// canvas rasteriser quantises only when it paints. Rounding the mean first
    /// would move a flat field by up to one level, and that field is 52% of the
    /// padded canvas by area, so the error would be a visible tint on framed
    /// covers rather than a rounding detail.
    pub fn filled_rgb(width: u32, height: u32, rgb: [f64; 3]) -> Self {
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        for px in pixels.chunks_exact_mut(4) {
            for k in 0..3 {
                px[k] = rgb[k].round().clamp(0.0, 255.0) as u8;
            }
            px[3] = 255;
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// A flat opaque image.
    pub fn filled(width: u32, height: u32, rgb: [u8; 3]) -> Self {
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        for px in pixels.chunks_exact_mut(4) {
            px[0] = rgb[0];
            px[1] = rgb[1];
            px[2] = rgb[2];
            px[3] = 255;
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Copies `src` into this image at `(x, y)`. Both must fit.
    fn blit(&mut self, src: &RgbaImage, x: u32, y: u32) {
        assert!(
            x + src.width <= self.width && y + src.height <= self.height,
            "blit of {}x{} at ({x},{y}) leaves a {}x{} image",
            src.width,
            src.height,
            self.width,
            self.height
        );
        for row in 0..src.height {
            let from = (row * src.width * 4) as usize;
            let to = (((y + row) * self.width + x) * 4) as usize;
            self.pixels[to..to + src.width as usize * 4]
                .copy_from_slice(&src.pixels[from..from + src.width as usize * 4]);
        }
    }

    /// Crops a sub-rectangle.
    fn crop(&self, x: u32, y: u32, width: u32, height: u32) -> RgbaImage {
        assert!(
            x + width <= self.width && y + height <= self.height,
            "crop {width}x{height} at ({x},{y}) leaves a {}x{} image",
            self.width,
            self.height
        );
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for row in 0..height {
            let from = (((y + row) * self.width + x) * 4) as usize;
            pixels.extend_from_slice(&self.pixels[from..from + width as usize * 4]);
        }
        RgbaImage {
            width,
            height,
            pixels,
        }
    }
}

/// Reconstruction filter used when resampling.
///
/// The reference does not use one filter, it uses three, because Canvas2D
/// resolves `drawImage` differently per call site:
///
/// * `Bilinear` — the default `imageSmoothingQuality` (`'low'`), which Skia
///   implements as an unsharpened 2x2 bilinear tap. `artwork.ts` leaves it in
///   place for the 160px probe, and `colour.ts` for the 32x32 statistics
///   sample. A *widened* filter here would average away detail that the
///   reference's aliased sample keeps, and the gain is computed from it.
/// * `Cubic` — `'high'`, Mitchell-Netravali (B = C = 1/3) with the support
///   scaled by the downscale factor. Used for the artwork square and for the
///   final 800 -> 200 source.
/// * `Box` — exact area average, used only for this port's own 4x pre-shrink
///   of the padded canvas (there is no reference call site for it; see the
///   note in `prepare_geometry_tracked`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kernel {
    /// Exact area average.
    Box,
    /// 2x2 bilinear tap, never widened: Skia's `'low'`.
    Bilinear,
    /// Mitchell-Netravali cubic, support scaled on downscale: Skia's `'high'`.
    Cubic,
}

impl Kernel {
    fn radius(self) -> f64 {
        match self {
            Kernel::Box => 0.5,
            Kernel::Bilinear => 1.0,
            Kernel::Cubic => 2.0,
        }
    }

    /// Whether the support grows with a downscale.
    ///
    /// Bilinear must not: widening it turns the 2x2 tap into an area average,
    /// which is a different (and for a statistics sample, differently aliased)
    /// picture.
    fn widens(self) -> bool {
        !matches!(self, Kernel::Bilinear)
    }

    fn eval(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            Kernel::Box => {
                if x <= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            Kernel::Bilinear => (1.0 - x).max(0.0),
            Kernel::Cubic => mitchell(x),
        }
    }
}

/// Mitchell-Netravali with B = C = 1/3, the cubic Skia uses for `'high'`.
fn mitchell(x: f64) -> f64 {
    const B: f64 = 1.0 / 3.0;
    const C: f64 = 1.0 / 3.0;
    if x < 1.0 {
        let x2 = x * x;
        let x3 = x2 * x;
        ((12.0 - 9.0 * B - 6.0 * C) * x3
            + (-18.0 + 12.0 * B + 6.0 * C) * x2
            + (6.0 - 2.0 * B))
            / 6.0
    } else if x < 2.0 {
        let x2 = x * x;
        let x3 = x2 * x;
        ((-B - 6.0 * C) * x3
            + (6.0 * B + 30.0 * C) * x2
            + (-12.0 * B - 48.0 * C) * x
            + (8.0 * B + 24.0 * C))
            / 6.0
    } else {
        0.0
    }
}

/// Per-destination-pixel source weights, normalized to sum to one.
fn weights(src_len: usize, dst_len: usize, kernel: Kernel) -> Vec<Vec<(usize, f64)>> {
    let scale = src_len as f64 / dst_len as f64;
    // A widening kernel (box, cubic) grows its support with the downscale so
    // every source pixel contributes, and measures distance in destination
    // pixels. Bilinear never widens: Chrome's 'low' filter is a fixed 2x2 tap
    // whatever the ratio, which is exactly why a 25x statistics sample drawn
    // through it aliases.
    let unit = if kernel.widens() { scale.max(1.0) } else { 1.0 };
    let support = kernel.radius() * unit;
    let mut table = Vec::with_capacity(dst_len);
    for i in 0..dst_len {
        let center = (i as f64 + 0.5) * scale;
        let first = ((center - support).floor() as i64).max(0) as usize;
        let last = ((center + support).ceil() as i64).min(src_len as i64) as usize;
        let mut entries = Vec::new();
        let mut total = 0.0;
        for j in first..last.max(first + 1).min(src_len) {
            let distance = ((j as f64 + 0.5) - center) / unit;
            // Mitchell's negative lobes are part of the filter: dropping them
            // would leave a positive-only kernel, i.e. a blurrier picture than
            // Chrome's, and the ring where the artwork meets the field is
            // exactly where that shows.
            let weight = kernel.eval(distance);
            if weight != 0.0 {
                entries.push((j, weight));
                total += weight;
            }
        }
        if total > 0.0 {
            for (_, weight) in &mut entries {
                *weight /= total;
            }
        } else {
            // Degenerate support: take the nearest source pixel.
            let nearest = (center as usize).min(src_len - 1);
            entries.push((nearest, 1.0));
        }
        table.push(entries);
    }
    table
}

/// Separable resample of an opaque RGBA8 image.
fn resample(src: &RgbaImage, width: u32, height: u32, kernel: Kernel) -> RgbaImage {
    if src.width == width && src.height == height {
        return src.clone();
    }
    let horizontal = weights(src.width as usize, width as usize, kernel);
    let vertical = weights(src.height as usize, height as usize, kernel);

    // Horizontal pass into an f32 intermediate so the two passes do not each
    // round to 8 bits; the reference's canvas stays in premultiplied 8-bit the
    // whole way, so the single rounding at the end is the closer analogue of
    // `putImageData` than a per-pass round trip would be.
    let mut middle = vec![0f32; width as usize * src.height as usize * 4];
    for y in 0..src.height as usize {
        for (x, taps) in horizontal.iter().enumerate() {
            let mut acc = [0f32; 4];
            for &(sx, weight) in taps {
                let i = (y * src.width as usize + sx) * 4;
                for k in 0..4 {
                    acc[k] += src.pixels[i + k] as f32 * weight as f32;
                }
            }
            let o = (y * width as usize + x) * 4;
            middle[o..o + 4].copy_from_slice(&acc);
        }
    }

    let mut out = vec![0u8; width as usize * height as usize * 4];
    for (y, taps) in vertical.iter().enumerate() {
        for x in 0..width as usize {
            let mut acc = [0f32; 4];
            for &(sy, weight) in taps {
                let i = (sy * width as usize + x) * 4;
                for k in 0..4 {
                    acc[k] += middle[i + k] * weight as f32;
                }
            }
            let o = (y * width as usize + x) * 4;
            for k in 0..4 {
                out[o + k] = acc[k].round().clamp(0.0, 255.0) as u8;
            }
            out[o + 3] = 255;
        }
    }
    RgbaImage {
        width,
        height,
        pixels: out,
    }
}

/// Separable Gaussian blur with clamp-to-edge addressing.
///
/// The radius is truncated at three sigma. The reference's canvas filter is an
/// untruncated Gaussian, so the truncation is a modelled difference: at three
/// sigma the omitted tail is 0.27% of the kernel mass, and it is applied to an
/// image whose border is a flat field.
///
/// The 8-bit store **truncates**, it does not round. Measured against the
/// browser's own `filter: blur(60px)` output on a frozen composed canvas: a
/// float blur is 0.497/0.507/0.456 brighter than the reference in R/G/B, and
/// truncating the store instead of rounding takes that to
/// -0.003/+0.005/-0.044 while halving the mean absolute error. The bias is
/// exactly half a code value and it survives swapping the kernel for a
/// three-pass box blur, so it belongs to the store and not to the filter
/// shape. Left rounding, it lands directly on the exposure: the gain is
/// derived from this picture's mean brightness, and half a code value of
/// brightness is about 2e-3 of gain.
fn gaussian_blur(src: &RgbaImage, sigma: f64) -> RgbaImage {
    assert!(sigma > 0.0, "blur sigma must be positive");
    let radius = (sigma * 3.0).ceil() as i64;
    let kernel: Vec<f64> = (-radius..=radius)
        .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp())
        .collect();
    // `kernel` already runs from -radius to +radius, so the plain sum is the
    // symmetric total. Folding `kernel[1..]` on top of it would count the
    // positive half twice.
    let total: f64 = kernel.iter().sum::<f64>();
    let width = src.width as i64;
    let height = src.height as i64;

    let mut middle = vec![0f32; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let mut acc = [0f64; 4];
            for (index, weight) in kernel.iter().enumerate() {
                let sx = (x + index as i64 - radius).clamp(0, width - 1);
                let i = ((y * width + sx) * 4) as usize;
                for k in 0..4 {
                    acc[k] += src.pixels[i + k] as f64 * weight;
                }
            }
            let o = ((y * width + x) * 4) as usize;
            for k in 0..4 {
                middle[o + k] = (acc[k] / total) as f32;
            }
        }
    }

    let mut out = vec![0u8; src.pixels.len()];
    for y in 0..height {
        for x in 0..width {
            let mut acc = [0f64; 4];
            for (index, weight) in kernel.iter().enumerate() {
                let sy = (y + index as i64 - radius).clamp(0, height - 1);
                let i = ((sy * width + x) * 4) as usize;
                for k in 0..4 {
                    acc[k] += middle[i + k] as f64 * weight;
                }
            }
            let o = ((y * width + x) * 4) as usize;
            for k in 0..4 {
                // Truncating store, not rounding: see the doc comment above.
                out[o + k] = (acc[k] / total).clamp(0.0, 255.0) as u8;
            }
            out[o + 3] = 255;
        }
    }
    RgbaImage {
        width: src.width,
        height: src.height,
        pixels: out,
    }
}

/// Everything the scene pass needs from one cover.
#[derive(Clone, Debug)]
pub struct PreparedArtwork {
    /// The 200x200 RGBA8 working source the shader samples.
    pub source: RgbaImage,
    /// The 32x32 sample of the original cover the colour policy read.
    pub sample: Vec<u8>,
    /// The exposure the final pass multiplies by.
    pub gain: f64,
    /// Artwork square size chosen by the probe (400 or 800).
    pub inner: u32,
    /// Whether the cover looked like artwork framed in a flat field.
    pub framed: bool,
    /// The field colour the artwork was padded with.
    pub field: [u8; 3],
    /// 0.0 (multi-hue) .. 1.0 (monochrome / single-hue cover).
    pub mono_factor: f32,
}

/// Bytes of CPU bitmap a finished [`PreparedArtwork`] hands to its caller: the
/// 200x200 working source and the 32x32 colour sample.
pub const PREPARED_BITMAP_BYTES: u64 = (SIZE * SIZE * 4 + SAMPLE_SIZE * SAMPLE_SIZE * 4) as u64;

/// Upper bound on the scratch one preparation pass may hold at once.
///
/// The largest step is the 1160x1160 padded canvas together with the artwork
/// square being blitted into it (5,382,400 + 2,560,000 bytes). Everything
/// afterwards is a 290x290 or smaller product of those two. The bound is
/// asserted from the ledger by the lifecycle test, which is what stops the
/// padded canvas from quietly becoming a per-frame allocation.
pub const SCRATCH_PEAK_BUDGET: u64 = 8 * 1024 * 1024;

/// The downsampled canvas the Gaussian runs on, in pixels.
const SHRUNK: u32 = (CANVAS + 2 * PAD) / WORK_SCALE;

/// Allocation bookkeeping for one preparation pass.
///
/// Handles are allocated and freed at the point each buffer's last reader
/// finishes, so the ledger mirrors the real lifetimes. A guard that freed
/// everything at the end would report a peak that no moment of the pass
/// actually reaches.
struct Scratch<'a> {
    ledger: &'a mut BitmapLedger,
}

impl Scratch<'_> {
    fn alloc(&mut self, bytes: u64) -> TempBitmap {
        self.ledger.alloc(bytes)
    }

    fn free(&mut self, handle: TempBitmap) {
        let freed = self.ledger.free(handle);
        debug_assert!(freed, "a scratch bitmap was released twice");
    }
}

fn bitmap_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * 4
}

/// The reference's `prepareArtwork()`.
///
/// # Panics
///
/// Panics when `image` is empty.
pub fn prepare_artwork(image: &RgbaImage) -> PreparedArtwork {
    prepare_artwork_tracked(image, &mut BitmapLedger::default())
}

/// [`prepare_artwork`], reporting every intermediate bitmap to `ledger`.
///
/// A finished pass leaves no live scratch behind: what the caller receives is
/// exactly [`PREPARED_BITMAP_BYTES`] of pixels and nothing else. That is the
/// property the lifecycle test measures across twenty cover switches.
///
/// # Panics
///
/// Panics when `image` is empty.
pub fn prepare_artwork_tracked(image: &RgbaImage, ledger: &mut BitmapLedger) -> PreparedArtwork {
    let geometry = prepare_geometry_tracked(image, ledger);

    // Colour policy, on the original cover's own sample. The reference reads
    // that sample straight off the decoded cover, not off the blurred canvas,
    // so the exposure stays a property of the artwork.
    let mut pixels = geometry.source.pixels;
    let mut params = ColourParams::for_inner(geometry.inner);
    let (gain, mono_factor) = super::colour::colour_artwork_with_mono(
        &mut pixels,
        CoverSample::new(&geometry.sample),
        &mut params,
    );

    PreparedArtwork {
        source: RgbaImage {
            width: SIZE as u32,
            height: SIZE as u32,
            pixels,
        },
        sample: geometry.sample,
        gain,
        inner: geometry.inner,
        framed: geometry.framed,
        field: geometry.field,
        mono_factor,
    }
}

/// The geometry stage's product: the composed 200x200 source *before* the
/// colour policy recolours it.
///
/// Exposed because the reference pipeline can be run with `colourArtwork`
/// stubbed out, and comparing that pre-colour canvas is the only way to tell a
/// resampling-kernel difference apart from a colour-policy difference. Folding
/// both into one end-to-end delta would let a gain error hide behind a pixel
/// tolerance chosen for the kernel.
#[derive(Clone, Debug)]
pub struct PreparedGeometry {
    /// The 200x200 RGBA8 working source, not yet recoloured.
    pub source: RgbaImage,
    /// The 32x32 sample of the original cover the colour policy reads.
    pub sample: Vec<u8>,
    /// Artwork square size chosen by the probe (400 or 800).
    pub inner: u32,
    /// Whether the cover looked like artwork framed in a flat field.
    pub framed: bool,
    /// The field colour the artwork was padded with.
    pub field: [u8; 3],
}

/// The geometry half of [`prepare_artwork`]: probe, pad, blur, resample.
///
/// # Panics
///
/// Panics when `image` is empty.
pub fn prepare_geometry(image: &RgbaImage) -> PreparedGeometry {
    prepare_geometry_tracked(image, &mut BitmapLedger::default())
}

/// [`prepare_geometry`], reporting its temporary bitmaps to `ledger`.
///
/// # Panics
///
/// Panics when `image` is empty.
pub fn prepare_geometry_tracked(
    image: &RgbaImage,
    ledger: &mut BitmapLedger,
) -> PreparedGeometry {
    assert!(image.width > 0 && image.height > 0, "cover is empty");
    let mut scratch = Scratch { ledger };

    // 1. Framed/full-bleed probe on a 160x160 downsample.
    let probe_bitmap = scratch.alloc(bitmap_bytes(PROBE, PROBE));
    let probe = probe_downsample(image);
    scratch.free(probe_bitmap);
    let (framed, field) = probe_field(&probe);

    // 2. Compose the padded canvas (always full-bleed 800x800 so minimalist
    //    covers are not shrunk into an isolated spot).
    let inner = 800;
    let canvas_bitmap = scratch.alloc(bitmap_bytes(CANVAS + 2 * PAD, CANVAS + 2 * PAD));
    let artwork_bitmap = scratch.alloc(bitmap_bytes(inner, inner));
    let mut canvas = RgbaImage::filled_rgb(CANVAS + 2 * PAD, CANVAS + 2 * PAD, field);
    // `b.imageSmoothingQuality='high'` before drawing the artwork square, so
    // both directions of the artwork resize are the cubic.
    let artwork = resample(image, inner, inner, Kernel::Cubic);
    let offset = PAD + (CANVAS - inner) / 2;
    canvas.blit(&artwork, offset, offset);
    // The artwork square was only ever a source for the blit.
    scratch.free(artwork_bitmap);

    // 3. Blur first, then reduce, at 290x290 instead of 1160x1160.
    //
    //    The reference blurs the whole 1160 canvas with sigma 60, crops the
    //    central 800 and draws that into 200x200. Doing the blur at 290 with
    //    sigma 15 is only equivalent if the reduction that precedes it is an
    //    exact 4x4 box: averaging over the box commutes with convolution, so
    //    `box(1160 -> 290) then gauss(15)` is identically `gauss(60) then
    //    box(1160 -> 290)`. A cubic there is *not* a box, it has negative
    //    lobes and does not commute: it aliases the artwork's sharp edge
    //    before the blur gets a chance to soften it, and the residue is a
    //    positive DC shift across the artwork (measured 0.24 to 0.69 code
    //    values, enough to miss the signed-off gain tolerance). The box keeps
    //    the cost at 290x290 and keeps the order faithful.
    //
    //    The crop keeps its 45px (three sigma) margin, so no edge of the
    //    reduced buffer is visible in the result.
    let shrunk_bitmap = scratch.alloc(bitmap_bytes(SHRUNK, SHRUNK));
    let shrunk = resample(&canvas, SHRUNK, SHRUNK, Kernel::Box);
    // The padded canvas is not read again after the downscale.
    scratch.free(canvas_bitmap);
    let blurred_bitmap = scratch.alloc(bitmap_bytes(SHRUNK, SHRUNK));
    let blurred = gaussian_blur(&shrunk, BLUR_SIGMA / WORK_SCALE as f64);
    scratch.free(shrunk_bitmap);
    let source = blurred.crop(
        PAD / WORK_SCALE,
        PAD / WORK_SCALE,
        CANVAS / WORK_SCALE,
        CANVAS / WORK_SCALE,
    );
    scratch.free(blurred_bitmap);
    debug_assert_eq!(source.width as usize, SIZE);
    debug_assert_eq!(source.height as usize, SIZE);

    // 4. The 32x32 sample the colour policy reads, taken from the original
    //    cover rather than from the blurred canvas. Only the geometry stage
    //    builds it here; the recolour that consumes it runs in
    //    `prepare_artwork_tracked`, so this stage's output can be compared
    //    against a reference run with `colourArtwork` stubbed out.
    // `colour.ts`'s `x.drawImage(IMG,0,0,N,N)` sets no quality either, so this
    // one is the default `'low'` tap as well.
    let sample = resample(
        image,
        SAMPLE_SIZE as u32,
        SAMPLE_SIZE as u32,
        Kernel::Bilinear,
    );

    PreparedGeometry {
        source,
        sample: sample.pixels,
        inner,
        framed,
        // Rounded only for reporting; the canvas above was painted from the
        // exact floats, which is what the reference does.
        field: [
            field[0].round().clamp(0.0, 255.0) as u8,
            field[1].round().clamp(0.0, 255.0) as u8,
            field[2].round().clamp(0.0, 255.0) as u8,
        ],
    }
}

/// The reference's 160x160 probe draw: `p.drawImage(image,0,0,160,160)` with no
/// `imageSmoothingQuality`, i.e. the platform default rather than the cubic.
///
/// Exposed because it is the one stage whose result feeds the frame colour:
/// the field fills 52% of the padded canvas, so a one-level bias here moves the
/// whole picture and would otherwise be blamed on the resampling kernel.
pub fn probe_downsample(image: &RgbaImage) -> RgbaImage {
    resample(image, PROBE, PROBE, Kernel::Bilinear)
}

/// Framed/full-bleed classification and the fill colour, from a probe image.
///
/// The colour is returned exactly as the reference computes it: floats, not
/// rounded channels. `artwork.ts` writes the border median (an integer) or the
/// float mean straight into a CSS `rgb()` string, so a mean of 41.6 fills as
/// 42. `PreparedGeometry::field` reports the rounded value for logging, but the
/// canvas is painted from these floats.
pub fn probe_field(probe: &RgbaImage) -> (bool, [f64; 3]) {
    let border = border_median(probe);
    let deviation = corner_deviation(probe, border);
    let framed = deviation < 10.0;
    let field = if framed {
        [border[0] as f64, border[1] as f64, border[2] as f64]
    } else {
        channel_mean(probe)
    };
    (framed, field)
}

/// Upper median of the 1px border ring, per channel.
///
/// The reference pushes rows 0, 1, 158 and 159 and columns 0, 1, 158 and 159,
/// so the four corners are counted twice; the sort makes the order irrelevant
/// but the duplication is part of the sample, so it is reproduced.
fn border_median(probe: &RgbaImage) -> [u8; 3] {
    let mut channels: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let add = |x: u32, y: u32, channels: &mut [Vec<u8>; 3]| {
        let px = probe.pixel(x, y);
        for k in 0..3 {
            channels[k].push(px[k]);
        }
    };
    for i in 0..160 {
        for y in [0, 1, 158, 159] {
            add(i, y, &mut channels);
        }
        for x in [0, 1, 158, 159] {
            add(x, i, &mut channels);
        }
    }
    let mut out = [0u8; 3];
    for k in 0..3 {
        channels[k].sort_unstable();
        out[k] = channels[k][channels[k].len() >> 1];
    }
    out
}

/// Largest absolute corner deviation from the border median.
fn corner_deviation(probe: &RgbaImage, border: [u8; 3]) -> f64 {
    let mut deviation = 0f64;
    for (x, y) in [(0u32, 0u32), (152, 0), (0, 152), (152, 152)] {
        let mut sum = [0f64; 3];
        for yy in y..y + 8 {
            for xx in x..x + 8 {
                let px = probe.pixel(xx, yy);
                for k in 0..3 {
                    sum[k] += px[k] as f64;
                }
            }
        }
        for k in 0..3 {
            deviation = deviation.max((sum[k] / 64.0 - border[k] as f64).abs());
        }
    }
    deviation
}

/// Mean probe colour, unrounded.
///
/// The reference accumulates `pixels[i+k]/(160*160)` in a double and never
/// rounds it; rounding here would shift a full-bleed field by up to half a
/// level in the direction of the fractional part.
fn channel_mean(probe: &RgbaImage) -> [f64; 3] {
    let mut sum = [0f64; 3];
    for px in probe.pixels.chunks_exact(4) {
        for k in 0..3 {
            sum[k] += px[k] as f64;
        }
    }
    let n = (probe.width * probe.height) as f64;
    [sum[0] / n, sum[1] / n, sum[2] / n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_cover_is_framed_with_its_own_colour_as_the_field() {
        let cover = RgbaImage::filled(512, 512, [70, 38, 19]);
        let prepared = prepare_artwork(&cover);
        assert!(prepared.framed, "a flat cover has no border contrast");
        assert_eq!(prepared.inner, 800);
        assert_eq!(prepared.field, [70, 38, 19]);
        // Solid covers intentionally gain a subtle Chiaroscuro tonal gradient.
        let first = prepared.source.pixel(0, 0);
        let last = prepared.source.pixel(199, 199);
        assert_eq!(prepared.mono_factor, 1.0);
        assert_ne!(first, last, "a flat cover should gain Chiaroscuro tonal depth");
        assert_eq!(first[3], 255);
        assert_eq!(last[3], 255);
    }

    #[test]
    fn a_framed_cover_keeps_the_frame_colour() {
        // A dark frame with a bright square in the middle.
        let mut cover = RgbaImage::filled(512, 512, [12, 14, 20]);
        let art = RgbaImage::filled(380, 380, [220, 180, 60]);
        cover.blit(&art, 66, 66);
        let prepared = prepare_artwork(&cover);
        assert!(prepared.framed);
        assert_eq!(prepared.inner, 800);
        assert_eq!(prepared.field, [12, 14, 20]);
    }

    #[test]
    fn a_full_bleed_cover_uses_its_mean() {
        // Half black, half white: the border varies, so it cannot be framed.
        let mut cover = RgbaImage::filled(512, 512, [0, 0, 0]);
        let white = RgbaImage::filled(512, 256, [255, 255, 255]);
        cover.blit(&white, 0, 0);
        let prepared = prepare_artwork(&cover);
        assert!(!prepared.framed, "a split cover has border contrast");
        assert_eq!(prepared.inner, 800);
        // The mean of half black and half white is mid grey.
        let field = prepared.field;
        for channel in field {
            assert!((100..=155).contains(&channel), "field {field:?}");
        }
    }

    #[test]
    fn the_working_source_is_always_two_hundred_pixels() {
        for size in [64u32, 200, 512, 1200] {
            let cover = RgbaImage::filled(size, size, [90, 90, 92]);
            let prepared = prepare_artwork(&cover);
            assert_eq!(prepared.source.width, 200);
            assert_eq!(prepared.source.height, 200);
            assert_eq!(prepared.sample.len(), 32 * 32 * 4);
        }
    }

    #[test]
    fn a_box_downscale_averages_a_checker_to_its_mean() {
        let mut cover = RgbaImage::filled(64, 64, [0, 0, 0]);
        for y in 0..64 {
            for x in 0..64 {
                if (x + y) % 2 == 0 {
                    let i = ((y * 64 + x) * 4) as usize;
                    cover.pixels[i] = 200;
                    cover.pixels[i + 1] = 200;
                    cover.pixels[i + 2] = 200;
                }
            }
        }
        let half = resample(&cover, 32, 32, Kernel::Box);
        for px in half.pixels.chunks_exact(4) {
            assert!((px[0] as i32 - 100).abs() <= 1, "checker average {px:?}");
        }
    }

    #[test]
    fn an_upscale_is_bounded_between_its_neighbours() {
        let mut cover = RgbaImage::filled(2, 2, [0, 0, 0]);
        let i = ((0 * 2 + 1) * 4) as usize;
        cover.pixels[i] = 255;
        let big = resample(&cover, 32, 32, Kernel::Cubic);
        for px in big.pixels.chunks_exact(4) {
            assert!(px[0] <= u8::MAX);
        }
        // The corner that started black stays black; the bright corner is
        // reached only near its own pixel.
        assert_eq!(big.pixel(0, 0)[0], 0);
        assert!(big.pixel(31, 0)[0] > 200);
    }

    #[test]
    fn the_blur_preserves_a_flat_field() {
        let cover = RgbaImage::filled(290, 290, [40, 50, 60]);
        let blurred = gaussian_blur(&cover, 15.0);
        for px in blurred.pixels.chunks_exact(4) {
            assert_eq!(&px[..3], &[40, 50, 60]);
        }
    }
}
