//! Cached lyric raster images, with transparent support for linear sampling.
pub(crate) fn lyric_blur(src: &[u8], w: usize, h: usize, radius: usize) -> (usize, usize, Vec<u8>) {
    // Two zero texels surround the filter support. The draw crops the outer
    // texel, so even a bilinear sample at its quad edge reads only zeros.
    let pad = radius + 2;
    let (ow, oh) = (w + 2 * pad, h + 2 * pad);
    let sigma = (radius as f32 / 3.0).max(0.5);
    let mut kernel: Vec<f32> = (0..=2 * radius).map(|i| {
        let x = i as f32 - radius as f32; (-x*x/(2.0*sigma*sigma)).exp()
    }).collect();
    let sum: f32 = kernel.iter().sum();
    for k in &mut kernel { *k /= sum; }
    let mut tmp = vec![0.0f32; ow * oh];
    for y in 0..h { for x in 0..ow {
        let mut v = 0.0;
        for (k, weight) in kernel.iter().enumerate() {
            let sx = x as isize + k as isize - (radius + pad) as isize;
            if sx >= 0 && sx < w as isize { v += src[y*w + sx as usize] as f32 * weight; }
        }
        tmp[(y + pad)*ow + x] = v;
    }}
    let mut out = vec![0u8; ow*oh];
    for y in 0..oh { for x in 0..ow {
        let mut v = 0.0;
        for (k, weight) in kernel.iter().enumerate() {
            let sy = y as isize + k as isize - radius as isize;
            if sy >= 0 && sy < oh as isize { v += tmp[sy as usize*ow+x] * weight; }
        }
        out[y*ow+x] = v.round().clamp(0.0,255.0) as u8;
    }}
    (ow, oh, out)
}

#[cfg(test)]
mod lyric_raster_tests {
    use super::lyric_blur;
    #[test]
    fn sharp_glyph_has_a_transparent_sampling_border() {
        let (w,h,image) = lyric_blur(&[255,64,128,255],2,2,0);
        assert_eq!((w,h),(6,6));
        assert_eq!(&image[14..16], &[255,64]);
        assert_eq!(&image[20..22], &[128,255]);
        for y in 0..h { for x in 0..w {
            if x < 2 || y < 2 || x+2 >= w || y+2 >= h { assert_eq!(image[y*w+x],0); }
        }}
    }
    #[test]
    fn blurred_glyph_keeps_a_zero_border_outside_its_gaussian_support() {
        for radius in [1,2,4,8] {
            let (w,h,image) = lyric_blur(&[255;9],3,3,radius);
            assert_eq!((w,h),(3+2*(radius+2),3+2*(radius+2)));
            assert!(image.iter().any(|v| *v > 0));
            for y in 0..h { for x in 0..w {
                if x < 2 || y < 2 || x+2 >= w || y+2 >= h { assert_eq!(image[y*w+x],0); }
            }}
        }
    }
}

/// Raster density in device pixels per DIP. Blurred layers stay native: their
/// filter already removes detail and supersampling would waste atlas space.
pub(crate) fn lyric_raster_scale(display_scale: f32, blur_radius: u16) -> f32 {
    if blur_radius == 0 { display_scale.max(2.0) } else { display_scale }
}

#[cfg(test)]
mod lyric_density_tests {
    use super::lyric_raster_scale;
    #[test]
    fn sharp_lyrics_keep_two_samples_per_dip_on_low_density_displays() {
        for sf in [1.0_f32, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
            let raster_sf = lyric_raster_scale(sf, 0);
            assert_eq!(raster_sf, sf.max(2.0));
        }
    }
    #[test]
    fn blurred_layers_keep_native_density_and_filter_radius() {
        for sf in [1.0, 1.25, 1.5, 2.0, 3.0] {
            for blur in [1, 4, 16, 64] {
                assert_eq!(lyric_raster_scale(sf, blur), sf);
            }
        }
    }
}
