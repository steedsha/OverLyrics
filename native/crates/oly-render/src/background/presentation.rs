//! Where the immersion background is allowed to run and how it is sampled.
//!
//! Ported from `src/windows/lyrics/background/presentation.ts` at commit
//! 708f6b1. Three policies live here, and all three are pure functions of the
//! window state, so they can be replayed without a GPU:
//!
//! * [`uses_immersion`] — the window mode is the only mode that gets the
//!   background by default; compact modes opt in through the display config,
//!   and the glass window background always wins over it.
//! * [`immersion_scrim`] — the three-stop darkening ramp applied per mode. The
//!   values are the CSS overlay's alphas, which the shader applies in float
//!   precision before the final dither.
//! * [`immersion_viewport`] — the render-target size and the vertical slice of
//!   the 200px scene the final pass shows. Compact windows cap both the DPR and
//!   the longest edge; window/fullscreen use the full DPR up to 2.

/// The window family the background is drawn for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMode {
    Window,
    Mini,
    Lyrics,
    Album,
}

/// The part of the display config that can veto or enable the background.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayOptions {
    /// `windowBackground === 'glass'`.
    pub glass: bool,
    /// `immersionCompact === true`.
    pub immersion_compact: bool,
}

/// The reference's `usesImmersion()`.
pub fn uses_immersion(mode: WindowMode, display: Option<DisplayOptions>) -> bool {
    let display = display.unwrap_or_default();
    if display.glass {
        return false;
    }
    match mode {
        WindowMode::Window => true,
        WindowMode::Mini | WindowMode::Lyrics => display.immersion_compact,
        WindowMode::Album => false,
    }
}

/// The reference's `immersionScrim()`: top, 65%, bottom alphas.
pub fn immersion_scrim(mode: WindowMode) -> [f32; 3] {
    match mode {
        WindowMode::Mini => [0.16, 0.16 + (0.28 - 0.16) * 0.65, 0.28],
        WindowMode::Lyrics => [0.22, 0.34, 0.28],
        WindowMode::Window | WindowMode::Album => [0.19, 0.19, 0.19],
    }
}

/// A render target size plus the vertical slice of the scene to show.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImmersionViewport {
    pub width: u32,
    pub height: u32,
    /// Top of the sampled band, 0..1 in scene space.
    pub sample_y: f32,
    /// Height of the sampled band, 0..1 in scene space.
    pub sample_height: f32,
}

impl ImmersionViewport {
    /// The reference's `immersionViewport()`. `dpr` is the window scale factor.
    pub fn for_window(width: f32, height: f32, dpr: f32, compact: bool) -> Self {
        let w = width.max(1.0);
        let h = height.max(1.0);
        // `dpr || 1` in the reference: a NaN or zero scale falls back to 1.
        let dpr = if dpr.is_finite() && dpr > 0.0 { dpr } else { 1.0 };
        let longest = w.max(h);
        let mut density = dpr.min(if compact { 1.5 } else { 2.0 });
        if compact {
            density = density.min(960.0 / longest);
        }
        let sample_height = if compact {
            (h / w).clamp(0.46, 0.86)
        } else {
            0.5625
        };
        Self {
            width: ((w * density).round() as i64).max(1) as u32,
            height: ((h * density).round() as i64).max(1) as u32,
            sample_y: (1.0 - sample_height) / 2.0,
            sample_height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_backgrounds_stay_opt_in() {
        assert!(uses_immersion(WindowMode::Window, None));
        for mode in [WindowMode::Mini, WindowMode::Lyrics, WindowMode::Album] {
            assert!(!uses_immersion(mode, None), "{mode:?}");
        }
    }

    #[test]
    fn the_compact_option_is_shared_and_glass_wins_globally() {
        let on = Some(DisplayOptions {
            glass: false,
            immersion_compact: true,
        });
        for mode in [WindowMode::Mini, WindowMode::Lyrics, WindowMode::Window] {
            assert!(uses_immersion(mode, on), "{mode:?}");
            assert!(
                !uses_immersion(
                    mode,
                    Some(DisplayOptions {
                        glass: true,
                        immersion_compact: true
                    })
                ),
                "{mode:?} with glass"
            );
        }
        assert!(!uses_immersion(WindowMode::Album, on));
    }

    #[test]
    fn the_mode_scrims_keep_their_top_mid_and_bottom_stops() {
        assert_eq!(immersion_scrim(WindowMode::Window), [0.19, 0.19, 0.19]);
        assert_eq!(immersion_scrim(WindowMode::Lyrics), [0.22, 0.34, 0.28]);
        let mini = immersion_scrim(WindowMode::Mini);
        assert_eq!(mini[0], 0.16);
        assert!((mini[1] - 0.238).abs() < 1e-6, "{}", mini[1]);
        assert_eq!(mini[2], 0.28);
    }

    #[test]
    fn compact_resolution_is_capped_and_keeps_its_shape() {
        let mini = ImmersionViewport::for_window(380.0, 176.0, 3.0, true);
        let lyrics = ImmersionViewport::for_window(380.0, 650.0, 3.0, true);
        assert_eq!(mini.width, 570);
        assert_eq!(mini.height, 264);
        assert!(lyrics.width <= 570);
        assert!(lyrics.height <= 960);
        let aspect = lyrics.width as f32 / lyrics.height as f32;
        assert!((aspect - 380.0 / 650.0).abs() < 0.01, "{aspect}");
        assert!((mini.sample_height - 176.0 / 380.0).abs() < 1e-5);
        assert!(lyrics.sample_height > mini.sample_height);
        assert!((lyrics.sample_y * 2.0 + lyrics.sample_height - 1.0).abs() < 1e-6);
    }

    #[test]
    fn window_sampling_keeps_the_full_palette() {
        let wide = ImmersionViewport::for_window(1280.0, 840.0, 2.0, false);
        assert_eq!(wide.width, 2560);
        assert_eq!(wide.height, 1680);
        assert_eq!(wide.sample_y, 0.21875);
        assert_eq!(wide.sample_height, 0.5625);
        // Degenerate inputs still produce a one-pixel target.
        assert_eq!(ImmersionViewport::for_window(0.0, 0.0, 1.0, true).width, 1);
    }
}
