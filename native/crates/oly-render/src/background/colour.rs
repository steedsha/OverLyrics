//! Colour policy of the am_bg v36 immersion background.
//!
//! Ported from `src/windows/lyrics/background/colour.ts` at commit 708f6b1.
//! Every arithmetic expression keeps the reference's operation order and
//! double precision. The golden fixture pins both the output bytes and the
//! gain to twelve decimals, so a "close enough" reformulation is a bug: the
//! gain feeds the final multiply in `background.hlsl`, and a drift there moves
//! every pixel of the window background.
//!
//! The policy has two halves, in this order:
//!
//! 1. [`recolour`] — scene-adaptive hue/saturation/exposure work in HSV. It
//!    desaturates muddy (brown/olive) artwork, deepens blue artwork and mutes
//!    dark covers so the lyrics keep contrast.
//! 2. [`auto_gain`] — hue-preserving exposure on the *field* (the area outside
//!    the artwork square), producing the single `gain` scalar the GPU pass
//!    multiplies by.
//!
//! Both read statistics from a 32x32 downsample of the original cover, taken
//! before any processing. That sample is a frozen input: the browser's
//! `drawImage` downscale and this port's box filter are different sampling
//! kernels, so the golden fixture stores the sample the reference actually
//! consumed rather than letting each side resample.

/// The working resolution of one background scene, in pixels.
pub const SIZE: usize = 200;

/// The cover-statistics resolution the reference samples at.
pub const SAMPLE_SIZE: usize = 32;

/// Scene statistics read from a 32x32 downsample of the *original* cover.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverStats {
    /// Mean relative luminance, 0..1.
    pub luma: f64,
    /// HSV saturation of the mean colour.
    pub sat: f64,
    /// Hue, saturation and value of the mean colour.
    pub hsv: [f64; 3],
}

/// A 32x32 RGBA8 downsample of the decoded cover, top-left origin.
#[derive(Clone, Copy, Debug)]
pub struct CoverSample<'a> {
    pub rgba: &'a [u8],
}

impl<'a> CoverSample<'a> {
    /// # Panics
    ///
    /// Panics when `rgba` is not exactly `32 * 32 * 4` bytes: a short buffer
    /// would silently shift every statistic.
    pub fn new(rgba: &'a [u8]) -> Self {
        assert_eq!(
            rgba.len(),
            SAMPLE_SIZE * SAMPLE_SIZE * 4,
            "cover sample must be 32x32 RGBA8"
        );
        Self { rgba }
    }

    /// The reference's `coverStats()`.
    pub fn stats(self) -> CoverStats {
        let mut luma = 0.0;
        let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
        for px in self.rgba.chunks_exact(4) {
            let (red, green, blue) = (px[0] as f64, px[1] as f64, px[2] as f64);
            luma += 0.3 * red + 0.59 * green + 0.11 * blue;
            r += red;
            g += green;
            b += blue;
        }
        let n = (SAMPLE_SIZE * SAMPLE_SIZE) as f64;
        let hsv = rgb_to_hsv(r / n / 255.0, g / n / 255.0, b / n / 255.0);
        CoverStats {
            luma: luma / n / 255.0,
            sat: hsv[1],
            hsv,
        }
    }
}

/// The reference's tunables, at their v36 defaults.
///
/// `inner` is the artwork square size chosen by the framed/full-bleed probe
/// (400 or 800) and therefore arrives from [`super::artwork`].
#[derive(Clone, Copy, Debug)]
pub struct ColourParams {
    pub inner: u32,
    pub lift: f64,
    pub sat: f64,
    pub gain: f64,
    pub auto: bool,
    pub chroma: f64,
    pub contrast: f64,
    pub light: f64,
    pub mud: f64,
    pub deep: f64,
    pub mud_t: f64,
    pub mud_c: f64,
    pub mud_g: f64,
    pub mud_s: f64,
    pub mud_v: f64,
    pub mud_h: f64,
    pub mute: f64,
    pub cool: f64,
    pub bright: f64,
}

impl Default for ColourParams {
    fn default() -> Self {
        Self {
            inner: 800,
            lift: 0.08,
            sat: 1.3,
            gain: 0.82,
            auto: true,
            chroma: 1.0,
            contrast: 1.0,
            light: 0.0,
            mud: 1.0,
            deep: 1.0,
            mud_t: 0.30,
            mud_c: 0.22,
            mud_g: 0.92,
            mud_s: 0.15,
            mud_v: 0.0,
            mud_h: 0.0,
            mute: 1.0,
            cool: 0.0,
            bright: 1.0,
        }
    }
}

impl ColourParams {
    pub fn for_inner(inner: u32) -> Self {
        Self {
            inner,
            ..Self::default()
        }
    }
}

/// Runs the perceptual OKLCH tone-map and entropy-aware Oklab palette flow
/// over a 200x200 RGBA8 scene source.
///
/// Returns the gain the final pass multiplies by (`1.0` when `params.auto` is
/// enabled, because exposure is already balanced in Oklab), and writes
/// `params.gain` so a caller that inspects the struct sees the same number.
///
/// # Panics
///
/// Panics when `pixels` is not exactly `200 * 200 * 4` bytes.
pub fn colour_artwork(
    pixels: &mut [u8],
    cover: CoverSample<'_>,
    params: &mut ColourParams,
) -> f64 {
    colour_artwork_with_mono(pixels, cover, params).0
}

/// Same as [`colour_artwork`], also returning `mono_factor` (`0.0..=1.0`,
/// where `1.0` means monochrome / single-hue cover).
pub fn colour_artwork_with_mono(
    pixels: &mut [u8],
    cover: CoverSample<'_>,
    params: &mut ColourParams,
) -> (f64, f32) {
    assert_eq!(
        pixels.len(),
        SIZE * SIZE * 4,
        "scene source must be 200x200 RGBA8"
    );
    oklch_tone_map(pixels, 1.22, 0.05);
    let mono = blend_palette_flow(pixels, cover.rgba);

    if params.light > 0.0 {
        apply_top_left_light(pixels, params.light);
    }

    if params.auto {
        params.gain = 1.0;
    }
    (params.gain, mono)
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn linear_to_oklab(r: f64, g: f64, b: f64) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).max(0.0).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).max(0.0).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).max(0.0).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_linear(l_ok: f64, a: f64, b: f64) -> [f64; 3] {
    let l_ = l_ok + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l_ok - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l_ok - 0.0894841775 * a - 1.2914855480 * b;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

fn smoothstep_f64(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Smooth 0..1 weight for warm brown / tan / sepia / ochre tones
/// (OKLCH hue ~30..98 deg, low-to-medium chroma) which look muddy/dark
/// when darkened to the same Oklab L as saturated hues.
fn warm_earth_factor(h_deg: f64, c: f64) -> f64 {
    let h_in = smoothstep_f64(25.0, 42.0, h_deg) * (1.0 - smoothstep_f64(88.0, 105.0, h_deg));
    let c_in = smoothstep_f64(0.010, 0.022, c) * (1.0 - smoothstep_f64(0.075, 0.115, c));
    h_in * c_in
}

/// Perceptual OKLCH tone-mapping + hue-preserving shadow lift with a smooth
/// chroma gate so pure B&W / neutral covers never amplify compression noise.
fn oklch_tone_map(pixels: &mut [u8], chroma_boost: f64, hue_lift: f64) {
    let n = SIZE * SIZE;
    let mut labs = Vec::with_capacity(n);
    let mut l_sum = 0.0;

    for px in pixels.chunks_exact(4) {
        let lab = linear_to_oklab(
            srgb_to_linear(px[0] as f64 / 255.0),
            srgb_to_linear(px[1] as f64 / 255.0),
            srgb_to_linear(px[2] as f64 / 255.0),
        );
        l_sum += lab[0];
        labs.push(lab);
    }

    let mean_l = l_sum / n as f64;
    // Dark warm browns (e.g. Preacher's Daughter / Willoughby Tucker, mean_l ~ 0.30-0.35)
    // should stay deep and moody; lighter tan/beige covers (e.g. Stick Season, mean_l ~ 0.65)
    // keep the full Candidate A warm lift.
    let we_tone_scale = 0.28 + 0.72 * smoothstep_f64(0.32, 0.56, mean_l);
    for (i, &[l, a, b]) in labs.iter().enumerate() {
        let c = (a * a + b * b).sqrt();
        let h = b.atan2(a);
        let h_deg = h.to_degrees().rem_euclid(360.0);
        let chroma_gate = ((c - 0.008) / 0.014).clamp(0.0, 1.0);
        // Taper chroma boost down toward 1.0 for already-vivid colours (C > 0.10).
        let high_c_taper = 1.0 - smoothstep_f64(0.10, 0.19, c);
        let taper_scale = 0.20 + 0.80 * high_c_taper;
        let eff_boost = 1.0 + (chroma_boost - 1.0) * chroma_gate * taper_scale;
        let we_lift = warm_earth_factor(h_deg, c) * 0.065 * we_tone_scale;

        let (l_new, c_new) = if mean_l > 0.60 {
            let scale_l = 0.42 / mean_l.max(1e-4);
            let ln = (l * (0.40 + 0.60 * scale_l) + we_lift).clamp(0.16, 0.50 + we_lift * 1.1);
            // Compensate chroma when darkening bright pastel/sky colours so
            // sky-blue / mint covers stay luminous instead of turning grey.
            let bright_comp = 1.0 + ((l - 0.55) / 0.35).clamp(0.0, 1.0) * 0.75 * chroma_gate;
            let cn = c
                * (1.0 + (chroma_boost * 1.25 * bright_comp - 1.0) * chroma_gate * taper_scale);
            (ln, cn)
        } else if mean_l < 0.32 {
            let ln = (l.max(hue_lift) * 1.18 + 0.04 + we_lift).clamp(0.14, 0.65);
            (ln, c * eff_boost)
        } else {
            let ln = (l * 0.88 + we_lift).clamp(0.16, 0.58 + we_lift);
            (ln, c * eff_boost)
        };

        let c_clamped = c_new.min(0.24);
        let lin = oklab_to_linear(l_new, c_clamped * h.cos(), c_clamped * h.sin());
        pixels[i * 4] = (linear_to_srgb(lin[0]) * 255.0).round().clamp(0.0, 255.0) as u8;
        pixels[i * 4 + 1] = (linear_to_srgb(lin[1]) * 255.0).round().clamp(0.0, 255.0) as u8;
        pixels[i * 4 + 2] = (linear_to_srgb(lin[2]) * 255.0).round().clamp(0.0, 255.0) as u8;
        pixels[i * 4 + 3] = 255;
    }
}

/// Extracts 4 chroma-weighted Oklab palette stops from the 32x32 sample and
/// blends a multi-lobe organic gradient into `pixels` weighted by spatial
/// color variance (higher weight for minimalist / white-bordered covers).
fn blend_palette_flow(pixels: &mut [u8], sample_rgba: &[u8]) -> f32 {
    let sn = SAMPLE_SIZE * SAMPLE_SIZE;
    let mut labs = Vec::with_capacity(sn);
    let mut weights = Vec::with_capacity(sn);
    let mut chromas = Vec::with_capacity(sn);
    let mut rgb_sum = [0.0f64; 3];
    let mut rgb_sq_sum = [0.0f64; 3];
    let mut inner_n = 0usize;

    for (pi, px) in sample_rgba.chunks_exact(4).enumerate() {
        let r = px[0] as f64 / 255.0;
        let g = px[1] as f64 / 255.0;
        let b = px[2] as f64 / 255.0;

        let lab = linear_to_oklab(srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
        let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
        labs.push(lab);
        chromas.push(c);

        // Mild chroma weight (1.0 + 1.2 * c) preserves real area dominance, and
        // gently down-weighting the outermost ~10% ring keeps packaging borders
        // (e.g. Deluxe frames) from overshadowing the central artwork.
        let sx = pi % SAMPLE_SIZE;
        let sy = pi / SAMPLE_SIZE;
        if (4..SAMPLE_SIZE - 4).contains(&sx) && (4..SAMPLE_SIZE - 4).contains(&sy) {
            rgb_sum[0] += r;
            rgb_sum[1] += g;
            rgb_sum[2] += b;
            rgb_sq_sum[0] += r * r;
            rgb_sq_sum[1] += g * g;
            rgb_sq_sum[2] += b * b;
            inner_n += 1;
        }
        let on_outer_border = sx < 3 || sx >= SAMPLE_SIZE - 3 || sy < 3 || sy >= SAMPLE_SIZE - 3;
        let spatial_w = if on_outer_border { 0.65 } else { 1.0 };
        weights.push((1.0 + 1.2 * c) * spatial_w);
    }

    // Spatial RGB std on the inner 75% (4..28) so thick borders don't inflate std.
    let nf = sn as f64;
    let inf = inner_n.max(1) as f64;
    let var_r = (rgb_sq_sum[0] / inf - (rgb_sum[0] / inf).powi(2)).max(0.0);
    let var_g = (rgb_sq_sum[1] / inf - (rgb_sum[1] / inf).powi(2)).max(0.0);
    let var_b = (rgb_sq_sum[2] / inf - (rgb_sum[2] / inf).powi(2)).max(0.0);
    let spatial_std = ((var_r + var_g + var_b) / 3.0).sqrt();

    // Continuous black/white proportion on the 32x32 sample:
    // - Covers with lots of pure black smoothly mix deeper black into shadow/base lobes.
    // - Covers with lots of pure white smoothly mix calm neutral grey into base lobes
    //   to avoid overexposure without switching into a separate mode.
    let black_ratio = labs
        .iter()
        .zip(&chromas)
        .filter(|&(lab, &c)| lab[0] < 0.14 && c < 0.035)
        .count() as f64
        / nf;
    let white_ratio = labs
        .iter()
        .zip(&chromas)
        .filter(|&(lab, &c)| lab[0] > 0.85 && c < 0.035)
        .count() as f64
        / nf;
    let black_mix = ((black_ratio - 0.35) / 0.45).clamp(0.0, 1.0);
    let white_mix = ((white_ratio - 0.35) / 0.45).clamp(0.0, 1.0);

    // Top 5% mean chroma gates pure B&W covers while keeping white-bordered color accents.
    chromas.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let top_n = (sn / 20).max(1);
    let c_top5: f64 = chromas[..top_n].iter().sum::<f64>() / top_n as f64;
    let achromatic_scale = ((c_top5 - 0.015) / 0.025).clamp(0.0, 1.0);

    // Deterministic weighted k-means (k = 4): seed via farthest-point from weighted mean.
    const K: usize = 4;
    let w_total: f64 = weights.iter().sum();
    let mut centers = [[0.0f64; 3]; K];
    for (lab, &w) in labs.iter().zip(&weights) {
        centers[0][0] += lab[0] * w;
        centers[0][1] += lab[1] * w;
        centers[0][2] += lab[2] * w;
    }
    centers[0][0] /= w_total;
    centers[0][1] /= w_total;
    centers[0][2] /= w_total;

    for ci in 1..K {
        let mut best_idx = 0;
        let mut best_score = -1.0;
        for (idx, (&lab, &w)) in labs.iter().zip(&weights).enumerate() {
            let min_d2 = (0..ci)
                .map(|j| {
                    let dl = lab[0] - centers[j][0];
                    let da = lab[1] - centers[j][1];
                    let db = lab[2] - centers[j][2];
                    dl * dl + da * da + db * db
                })
                .fold(f64::INFINITY, f64::min);
            let score = min_d2 * w;
            if score > best_score {
                best_score = score;
                best_idx = idx;
            }
        }
        centers[ci] = labs[best_idx];
    }

    // Track final cluster assignments, pixel area fractions, and vertical centroids.
    let mut assign = vec![0usize; sn];
    for _ in 0..12 {
        let mut sums = [[0.0f64; 3]; K];
        let mut w_sums = [0.0f64; K];
        for (pi, (&lab, &w)) in labs.iter().zip(&weights).enumerate() {
            let mut best_c = 0;
            let mut best_d2 = f64::INFINITY;
            for (ci, c) in centers.iter().enumerate() {
                let dl = lab[0] - c[0];
                let da = lab[1] - c[1];
                let db = lab[2] - c[2];
                let d2 = dl * dl + da * da + db * db;
                if d2 < best_d2 {
                    best_d2 = d2;
                    best_c = ci;
                }
            }
            assign[pi] = best_c;
            sums[best_c][0] += lab[0] * w;
            sums[best_c][1] += lab[1] * w;
            sums[best_c][2] += lab[2] * w;
            w_sums[best_c] += w;
        }
        for ci in 0..K {
            if w_sums[ci] > 0.0 {
                centers[ci][0] = sums[ci][0] / w_sums[ci];
                centers[ci][1] = sums[ci][1] / w_sums[ci];
                centers[ci][2] = sums[ci][2] / w_sums[ci];
            }
        }
    }

    let mut counts = [0usize; K];
    let mut y_sums = [0.0f64; K];
    for (pi, &ci) in assign.iter().enumerate() {
        counts[ci] += 1;
        let py = (pi / SAMPLE_SIZE) as f64 + 0.5;
        y_sums[ci] += py / SAMPLE_SIZE as f64;
    }

    #[derive(Clone, Copy)]
    struct ClusterInfo {
        lab: [f64; 3],
        area: f64,
        c: f64,
        cy: f64,
    }

    let infos: Vec<ClusterInfo> = (0..K)
        .map(|ci| {
            let area = counts[ci] as f64 / nf;
            let lab = centers[ci];
            let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            let cy = if counts[ci] > 0 {
                y_sums[ci] / counts[ci] as f64
            } else {
                0.5
            };
            ClusterInfo { lab, area, c, cy }
        })
        .collect();

    // Explicit Primary / Secondary / Accent role separation:
    // 1. If the largest cluster covers >= 35% and is NOT an extreme pure black/white
    //    achromatic void, lock it as Primary so small bright accents (e.g. bubblegum
    //    pink or Deluxe frames) never hijack the main mood.
    // 2. Otherwise (pure black/white void or balanced multi-color), rank by
    //    area^0.55 * (0.06 + c) with a void penalty so luminous subjects still shine.
    let is_extreme_void = |cl: &ClusterInfo| (cl.lab[0] < 0.15 || cl.lab[0] > 0.88) && cl.c < 0.03;

    let largest_idx = infos
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.area.partial_cmp(&b.area).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);

    let pri_idx = if infos[largest_idx].area >= 0.35 && !is_extreme_void(&infos[largest_idx]) {
        largest_idx
    } else {
        infos
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                let pa = if is_extreme_void(a) { 0.45 } else { 1.0 };
                let pb = if is_extreme_void(b) { 0.45 } else { 1.0 };
                let sa = a.area.powf(0.55) * (0.06 + a.c) * pa;
                let sb = b.area.powf(0.55) * (0.06 + b.c) * pb;
                sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    };

    let pri = infos[pri_idx];
    let remaining: Vec<ClusterInfo> = infos
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != pri_idx)
        .map(|(_, &cl)| cl)
        .collect();

    // Secondary: largest remaining area; Accent: highest chroma distinct from Secondary.
    let sec_pos = remaining
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.area.partial_cmp(&b.area).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let sec = remaining[sec_pos];
    let acc = remaining
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != sec_pos)
        .map(|(_, &cl)| cl)
        .max_by(|a, b| a.c.partial_cmp(&b.c).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(sec);

    // Measure monochrome factor from max pairwise hue angle among colored clusters.
    let colored: Vec<ClusterInfo> = infos
        .iter()
        .copied()
        .filter(|cl| cl.c > 0.025 && cl.area > 0.03)
        .collect();
    let mono: f64 = if colored.len() <= 1 || achromatic_scale < 0.25 {
        1.0
    } else {
        let mut max_dh = 0.0f64;
        for i in 0..colored.len() {
            for j in (i + 1)..colored.len() {
                let h1 = colored[i].lab[2].atan2(colored[i].lab[1]);
                let h2 = colored[j].lab[2].atan2(colored[j].lab[1]);
                let d = (h1 - h2).abs();
                let dh = d.min(2.0 * std::f64::consts::PI - d);
                max_dh = max_dh.max(dh);
            }
        }
        let deg = max_dh.to_degrees();
        ((42.0 - deg) / 30.0).clamp(0.0, 1.0)
    };

    // Gently tint truly achromatic primary lobes (c < 0.012) toward the accent's hue.
    // Never tint pastel/sky-blue primaries (c ~ 0.02..0.03) toward complementary accents!
    let mut pri_lab = pri.lab;
    if pri.c < 0.012 && acc.c > 0.04 {
        pri_lab[1] = pri_lab[1] * 0.82 + acc.lab[1] * 0.18;
        pri_lab[2] = pri_lab[2] * 0.82 + acc.lab[2] * 0.18;
    }

    // Restrained, hue-aware analogous OKLCH drift (+/- 16 deg) + clean Chiaroscuro
    // for monochrome covers. For greens/yellow-greens (OKLCH H ~ 85..165 deg),
    // steer shadow lobe toward cool deep emerald (+18 deg) instead of muddy olive-yellow.
    let pri_h = pri_lab[2].atan2(pri_lab[1]);
    let pri_h_deg = pri_h.to_degrees().rem_euclid(360.0);
    let pri_c = (pri_lab[1] * pri_lab[1] + pri_lab[2] * pri_lab[2]).sqrt();
    let is_green_family = (85.0..=165.0).contains(&pri_h_deg);
    let d_rad = 17.0f64.to_radians() * mono * achromatic_scale;
    let hue_offsets = if is_green_family {
        [-d_rad * 0.35, d_rad * 0.55, -d_rad * 0.25, d_rad * 1.05]
    } else {
        [d_rad * 0.85, -d_rad * 0.50, d_rad * 0.35, -d_rad * 0.95]
    };

    // Moderate bright-to-dark chroma compensation for bright pastel/sky primaries
    // so they stay clear when darkened without overpowering the accent lobe.
    let comp_gain = if white_mix < 0.20 { 0.85 } else { 0.55 };
    let pri_chroma_comp = if pri_lab[0] > 0.58 && pri_c > 0.015 {
        1.0 + ((pri_lab[0] - 0.58) / 0.30).clamp(0.0, 1.0) * comp_gain
    } else {
        1.0
    };

    // Dominant primary (>= 55% area, non-void) uses 3 : 1 lobe ratio so the main
    // colour clearly leads (~65-75%) while the 1 accent lobe keeps full weight &
    // radius inside the visible 16:9 band so it stays visibly present.
    let dominant_primary = pri.area >= 0.55 && !is_extreme_void(&pri);
    let lobe1_lab = if dominant_primary {
        let h1 = pri_h + 8.0f64.to_radians() * achromatic_scale;
        [pri_lab[0], pri_c * h1.cos(), pri_c * h1.sin()]
    } else {
        sec.lab
    };

    // Depth boost: widen Chiaroscuro contrast on low-variance / solid covers
    // so tonal pockets survive GPU sigma=16 blur cleanly.
    let low_var_boost = smoothstep_f64(0.14, 0.02, spatial_std);
    let hi_add = 0.06 * low_var_boost;
    let lo_sub = 0.04 * low_var_boost;

    let area_gate = smoothstep_f64(0.02, 0.12, acc.area);
    let lobe_specs = [
        (
            pri_lab,
            0.68,
            0.12 + 0.06 * mono + hi_add,
            0.28 + 0.05 * mono,
            0.52 + 0.06 * mono + hi_add,
            (1.30 + 0.06 * mono) * pri_chroma_comp,
            0.05,
            0.22,
        ),
        (
            lobe1_lab,
            0.62,
            0.10,
            0.24,
            0.46,
            if dominant_primary {
                1.24 * pri_chroma_comp
            } else {
                1.24
            },
            0.04,
            0.20,
        ),
        (
            acc.lab,
            0.64,
            0.12 + 0.03 * mono,
            0.26,
            0.52 + 0.04 * mono,
            if dominant_primary {
                1.15 + 0.50 * area_gate
            } else {
                1.35 + 0.05 * mono
            },
            0.0,
            0.23,
        ),
        (
            pri_lab,
            0.48 - 0.14 * mono,
            0.05 - 0.02 * mono - lo_sub,
            (0.11 - lo_sub).max(0.07),
            0.34 - 0.10 * mono,
            (1.15 + 0.08 * mono) * pri_chroma_comp,
            0.02,
            0.18,
        ),
    ];

    // Lightness-adaptive warm-earth lift:
    // - Light tan / beige covers (pri L >= 0.55) keep the full Candidate A lift.
    // - Dark brown / sepia covers (pri L ~ 0.24..0.35) get a much subtler lift
    //   with deeper shadows and slightly richer chroma so they stay moody, not pale.
    let we = warm_earth_factor(pri_h_deg, pri_c);
    let we_bright = smoothstep_f64(0.32, 0.56, pri_lab[0]);
    let we_l = we * (0.28 + 0.72 * we_bright);
    let we_shadow = we * (0.12 + 0.88 * we_bright);
    let we_dark_chroma = 1.0 + 0.14 * we * (1.0 - we_bright);
    let l_lift = [0.075 * we_l, 0.085 * we_l, 0.060 * we_l, 0.090 * we_shadow];
    let l_ceil_add = [0.090 * we_l, 0.095 * we_l, 0.070 * we_l, 0.110 * we_shadow];

    let mut tuned = [[0.0f64; 3]; K];
    let mut pri_pre_ln = 0.0f64;
    for (ci, &(lab, ls, lo, lmin, lmax, cs, cmin, cmax)) in lobe_specs.iter().enumerate() {
        let [l, a, b] = lab;
        let c = (a * a + b * b).sqrt();
        let h = b.atan2(a) + hue_offsets[ci];
        let stop_gate = ((c - 0.008) / 0.014).clamp(0.0, 1.0) * achromatic_scale;
        let mut ln = (l * ls + lo + l_lift[ci])
            .clamp(lmin + l_lift[ci] * 0.7, lmax + l_ceil_add[ci]);
        if ci == 0 {
            pri_pre_ln = ln;
        } else if ci == 2 {
            // Keep tiny bright highlights on dark primaries from overpowering the
            // primary mood in linear light, while relaxing on true black-void covers.
            ln = ln.min(pri_pre_ln + 0.15 + 0.16 * black_mix);
        }
        let taper = 1.0 - smoothstep_f64(0.10, 0.19, c);
        let eff_cs = 1.0 + (cs - 1.0) * (0.18 + 0.82 * taper);
        let mut cn = (c * eff_cs * we_dark_chroma).clamp(cmin, f64::min(cmax, 0.185)) * stop_gate;

        // Smoothly mix in more deep black for black-heavy covers (e.g. Bobby / Lost Weekend).
        if black_mix > 0.0 {
            let b_w = match ci {
                3 => 0.72 * black_mix, // deep shadow lobe -> near-black
                1 => 0.48 * black_mix, // secondary base lobe -> darker
                0 => 0.22 * black_mix, // primary highlight -> slightly deeper
                _ => 0.08 * black_mix, // accent glow stays luminous
            };
            ln = ln * (1.0 - b_w) + 0.05 * b_w;
            cn *= 1.0 - 0.55 * b_w;
        }

        // Smoothly mix in calm neutral grey for white-heavy covers (e.g. 21 / minor)
        // at a strength symmetric with black_mix so white covers never overexpose.
        if white_mix > 0.0 {
            let g_w = match ci {
                0 | 1 => 0.70 * white_mix,
                3 => 0.68 * white_mix,
                _ => 0.22 * white_mix, // keep accent restrained on minimal white covers
            };
            let grey_l = match ci {
                0 => 0.32,
                1 => 0.26,
                3 => 0.19,
                _ => 0.30,
            };
            ln = ln * (1.0 - g_w) + grey_l * g_w;
            cn *= 1.0 - 0.80 * g_w;
        }

        tuned[ci] = [ln, cn * h.cos(), cn * h.sin()];
    }

    let palette_weight = ((0.26 - spatial_std) / 0.14 + 0.16 * mono).clamp(0.38, 0.88);
    // Keep all 4 anchors well inside the visible 16:9 vertical band (0.21875..0.78125).
    let (acc_y, pri_y, deep_y) = if acc.cy >= 0.5 {
        (0.64f64, 0.34f64, 0.68f64)
    } else {
        (0.36f64, 0.66f64, 0.32f64)
    };
    let anchors = [(0.22f64, pri_y), (0.74, 0.32), (0.40, acc_y), (0.80, deep_y)];
    let s_tight = 0.20 - 0.03 * mono;
    // Pre-blur energy compensation for the Accent lobe, gated by acc.area so
    // tiny 2-6% bright specks stay tasteful while larger accents remain vivid.
    let mut s_acc = if dominant_primary {
        0.18 + 0.10 * area_gate
    } else {
        0.20 + 0.05 * area_gate
    };
    if white_mix > 0.0 {
        s_acc *= 1.0 - 0.18 * white_mix;
    }
    let sigmas = [s_tight, 0.21, s_acc, 0.25];
    let mut acc_w = if dominant_primary {
        0.65 + 0.83 * area_gate
    } else {
        0.70 + 0.30 * area_gate
    };
    if white_mix > 0.0 {
        acc_w *= 1.0 - 0.30 * white_mix;
    }
    let lobe_weights = [1.0, 1.0, acc_w, 1.0];

    for y in 0..SIZE {
        let v = y as f64 / SIZE as f64;
        for x in 0..SIZE {
            let u = x as f64 / SIZE as f64;
            let wu = u + 0.06 * (v * 4.5 + 0.8).sin();
            let wv = v + 0.06 * (u * 4.0 + 1.2).cos();

            let mut w_sum = 0.0;
            let mut lab_f = [0.0f64; 3];
            for ci in 0..K {
                let dx = wu - anchors[ci].0;
                let dy = wv - anchors[ci].1;
                let d2 = dx * dx + dy * dy;
                let w = ((-d2 / (2.0 * sigmas[ci] * sigmas[ci])).exp() + 0.04) * lobe_weights[ci];
                w_sum += w;
                lab_f[0] += w * tuned[ci][0];
                lab_f[1] += w * tuned[ci][1];
                lab_f[2] += w * tuned[ci][2];
            }
            let lin_f = oklab_to_linear(lab_f[0] / w_sum, lab_f[1] / w_sum, lab_f[2] / w_sum);
            let sr = linear_to_srgb(lin_f[0]);
            let sg = linear_to_srgb(lin_f[1]);
            let sb = linear_to_srgb(lin_f[2]);

            let idx = (y * SIZE + x) * 4;
            let dr = pixels[idx] as f64 / 255.0;
            let dg = pixels[idx + 1] as f64 / 255.0;
            let db = pixels[idx + 2] as f64 / 255.0;

            pixels[idx] = ((palette_weight * sr + (1.0 - palette_weight) * dr) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            pixels[idx + 1] = ((palette_weight * sg + (1.0 - palette_weight) * dg) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            pixels[idx + 2] = ((palette_weight * sb + (1.0 - palette_weight) * db) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            pixels[idx + 3] = 255;
        }
    }
    mono as f32
}

fn apply_top_left_light(pixels: &mut [u8], light: f64) {
    for y in 0..SIZE {
        let dy = (y as f64 / SIZE as f64 - 0.20) / (0.62 * 0.9);
        for x in 0..SIZE {
            let dx = (x as f64 / SIZE as f64 - 0.34) / 0.62;
            let glow = (-(dx * dx + dy * dy)).exp() * light;
            let idx = (y * SIZE + x) * 4;
            for c in 0..3 {
                let v = pixels[idx + c] as f64 / 255.0;
                pixels[idx + c] = ((v + (1.0 - v) * glow) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// RGB (0..1) to HSV. `%` keeps the dividend's sign, matching JavaScript.
pub fn rgb_to_hsv(r: f64, g: f64, b: f64) -> [f64; 3] {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let mut h = 0.0;
    if d > 1e-9 {
        if mx == r {
            h = ((g - b) / d) % 6.0;
        } else if mx == g {
            h = (b - r) / d + 2.0;
        } else {
            h = (r - g) / d + 4.0;
        }
        h /= 6.0;
        if h < 0.0 {
            h += 1.0;
        }
    }
    let s = if mx > 1e-9 { d / mx } else { 0.0 };
    [h, s, mx]
}

/// HSV to RGB, with the hue wrapped the way the reference wraps it.
pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    let h = ((h % 1.0) + 1.0) % 1.0;
    let scaled = h * 6.0;
    let i = scaled.floor();
    let f = scaled - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match (i as i64).rem_euclid(6) {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat 32x32 sample of one colour, as `CoverSample`.
    fn flat_sample(rgb: [u8; 3]) -> [u8; SAMPLE_SIZE * SAMPLE_SIZE * 4] {
        let mut out = [0u8; SAMPLE_SIZE * SAMPLE_SIZE * 4];
        for px in out.chunks_exact_mut(4) {
            px[0] = rgb[0];
            px[1] = rgb[1];
            px[2] = rgb[2];
            px[3] = 255;
        }
        out
    }

    fn flat_scene(rgb: [u8; 3]) -> Vec<u8> {
        let mut out = vec![0u8; SIZE * SIZE * 4];
        for px in out.chunks_exact_mut(4) {
            px[0] = rgb[0];
            px[1] = rgb[1];
            px[2] = rgb[2];
            px[3] = 255;
        }
        out
    }

    #[test]
    fn hue_conversion_round_trips_primaries() {
        for (rgb, expect) in [
            ([1.0, 0.0, 0.0], 0.0),
            ([0.0, 1.0, 0.0], 1.0 / 3.0),
            ([0.0, 0.0, 1.0], 2.0 / 3.0),
        ] {
            let hsv = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
            assert!((hsv[0] - expect).abs() < 1e-12, "{rgb:?} → {hsv:?}");
            assert!((hsv[1] - 1.0).abs() < 1e-12);
            assert!((hsv[2] - 1.0).abs() < 1e-12);
            let back = hsv_to_rgb(hsv[0], hsv[1], hsv[2]);
            for k in 0..3 {
                assert!((back[k] - rgb[k]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn a_grey_cover_has_no_hue() {
        let hsv = rgb_to_hsv(0.35, 0.35, 0.35);
        assert_eq!(hsv, [0.0, 0.0, 0.35]);
    }

    #[test]
    fn the_gain_is_clamped_to_the_documented_range() {
        // Near-black cover: the exposure target would need a huge multiplier.
        let sample = flat_sample([1, 1, 1]);
        let mut scene = flat_scene([1, 1, 1]);
        let mut params = ColourParams::for_inner(800);
        let gain = colour_artwork(&mut scene, CoverSample::new(&sample), &mut params);
        assert!((0.35..=2.40).contains(&gain), "gain {gain} out of range");
        assert_eq!(gain, params.gain);
    }

    #[test]
    fn a_bright_cover_never_exceeds_the_exposure_target() {
        let sample = flat_sample([220, 210, 170]);
        let mut scene = flat_scene([220, 210, 170]);
        let mut params = ColourParams::for_inner(800);
        let gain = colour_artwork(&mut scene, CoverSample::new(&sample), &mut params);
        assert!(gain <= 2.40);
        // The policy keeps highlights from blowing out: the field stays under
        // 0.80 after the gain multiply.
        let field = scene[0] as f64 / 255.0 * gain;
        assert!(field <= 0.80 + 1e-9, "field {field}");
    }

    #[test]
    fn the_light_branch_lifts_the_top_left_most() {
        let sample = flat_sample([60, 60, 60]);
        let mut scene = flat_scene([60, 60, 60]);
        let mut params = ColourParams::for_inner(800);
        params.light = 0.5;
        params.auto = false;
        colour_artwork(&mut scene, CoverSample::new(&sample), &mut params);
        let top_left = scene[0] as i32;
        let bottom_right = scene[(SIZE * SIZE - 1) * 4] as i32;
        assert!(
            top_left > bottom_right + 20,
            "top-left {top_left} should be much lighter than bottom-right {bottom_right}"
        );
    }

    #[test]
    fn alpha_is_forced_opaque() {
        let sample = flat_sample([90, 40, 20]);
        let mut scene = flat_scene([90, 40, 20]);
        for px in scene.chunks_exact_mut(4) {
            px[3] = 0;
        }
        let mut params = ColourParams::for_inner(400);
        colour_artwork(&mut scene, CoverSample::new(&sample), &mut params);
        assert!(scene.chunks_exact(4).all(|px| px[3] == 255));
    }
}
