//! Long-note emphasis: glow, scale and lift, ported from the patched AMLL.
//!
//! The reference build ships two deliberate deviations from upstream AMLL, and
//! both are part of the look the frozen fixtures record:
//!
//!   1. the word-length test is removed, so a held syllable of any length is
//!      emphasised (upstream also required at most seven characters), and
//!   2. the glow alpha is multiplied by 0.75 (upstream writes the raw level).
//!
//! The numbers here are not re-derived from the geometry: they are checked
//! against `native/tests/fixtures/oracle/emphasize/amll-emphasize.json`, which
//! is produced by running the shipped animator itself and parsing back the CSS
//! it emits. `emp_easing` mirrors `bezier-easing@3.2.0`, the exact module the
//! library imports, including its Newton iteration and subdivision fallback;
//! a "close enough" curve would move every frame by a visible fraction of an em.

/// Keyframes per emphasis curve. AMLL samples both curves at 32 points.
pub const FRAME_COUNT: usize = 32;

/// The glow profile this build ships with: `glowLevel * 0.75`.
pub const GLOW_ALPHA_SCALE: f64 = 0.75;

/// A word is emphasised when it is held for at least one second.
///
/// Upstream additionally required `trim().len() <= 7` for non-CJK words. The
/// trial build removes that test, which is why the whole rule is now the
/// duration alone: the reference also drops the CJK branch's separate wording,
/// leaving one predicate for every script.
pub fn should_emphasize(duration_ms: i64) -> bool {
    duration_ms >= 1000
}

/// The per-word animation parameters AMLL derives from the word's timing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmphasisParams {
    /// Peak extra scale, `1 + transX * 0.1 * amount`.
    pub amount: f64,
    /// Peak glow level before the shipped 0.75 factor.
    pub blur: f64,
    /// Animation duration after the last-word 1.2 multiplier.
    pub du: f64,
    /// Delay of the word's first character, in ms from the line start.
    pub de: f64,
    /// Divisor for the per-character delay ramp.
    pub anchor_char_count: f64,
    /// `du` when finite, else 0. The reference guards against a non-finite
    /// duration here rather than at each use site.
    pub animate_du: f64,
}

impl EmphasisParams {
    /// `calculateEmphasizeParams` from the shipped animator.
    ///
    /// `duration_ms` is the word's own span, `delay_ms` its start relative to
    /// the line, `ruby_char_count` the summed ruby length when the word has
    /// ruby, and `char_count` the number of grapheme elements the word will be
    /// split into.
    pub fn new(
        duration_ms: f64,
        delay_ms: f64,
        ruby_char_count: usize,
        char_count: usize,
        is_last_word: bool,
    ) -> Self {
        let de = delay_ms.max(0.0);
        let mut du = duration_ms.max(1000.0);
        let anchor_char_count = if ruby_char_count > 0 {
            ruby_char_count as f64
        } else {
            char_count.max(1) as f64
        };
        // The reference curves the raw ratios, not the milliseconds: below one
        // the response is cubic (gentle), above it square-root (saturating).
        let mut amount = du / 2000.0;
        amount = if amount > 1.0 {
            amount.sqrt()
        } else {
            amount.powi(3)
        };
        let mut blur = du / 3000.0;
        blur = if blur > 1.0 { blur.sqrt() } else { blur.powi(3) };
        amount *= 0.6;
        blur *= 0.5;
        if is_last_word {
            amount *= 1.6;
            blur *= 1.5;
            du *= 1.2;
        }
        Self {
            amount: amount.min(1.2),
            blur: blur.min(0.8),
            du,
            de,
            anchor_char_count,
            animate_du: if du.is_finite() { du } else { 0.0 },
        }
    }
}

/// One frame of the glow curve, in the units the CSS uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowKeyframe {
    /// Normalized time in `(0, 1]`; the reference never emits an offset of 0.
    pub offset: f64,
    /// Uniform scale about the character's own origin.
    pub scale: f64,
    /// Horizontal shift, in em.
    pub translate_x_em: f64,
    /// Vertical lift, in em.
    pub translate_y_em: f64,
    /// Text-shadow blur radius, in em. Constant for a given word.
    pub shadow_blur_em: f64,
    /// Text-shadow alpha, already including [`GLOW_ALPHA_SCALE`].
    pub shadow_alpha: f64,
}

/// One frame of the lift curve, in em.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatKeyframe {
    pub offset: f64,
    pub translate_y_em: f64,
}

/// The animations for one character of an emphasised word.
#[derive(Clone, Debug, PartialEq)]
pub struct CharAnimation {
    pub index: usize,
    /// Delay of this character's glow, in ms from the line start. Later
    /// characters trail by `du / 2.5 / anchorCharCount` each, which is what
    /// makes a long syllable ripple left to right.
    pub word_delay: f64,
    pub glow: Vec<GlowKeyframe>,
    pub float: Vec<FloatKeyframe>,
}

/// Everything needed to animate one emphasised word.
#[derive(Clone, Debug, PartialEq)]
pub struct EmphasisPlan {
    pub params: EmphasisParams,
    pub chars: Vec<CharAnimation>,
    pub glow_duration: f64,
    pub float_duration: f64,
}

impl EmphasisPlan {
    /// Build the plan for a word, or `None` when nothing would animate.
    ///
    /// AMLL splits the word into grapheme elements only when the word has a
    /// non-whitespace body; a whitespace-only word therefore reports
    /// `shouldEmphasize` but never gets a plan. Keeping that distinction here
    /// means the renderer cannot accidentally allocate glow for invisible
    /// material.
    pub fn new(
        text: &str,
        start_ms: i64,
        end_ms: i64,
        line_start_ms: i64,
        is_bg: bool,
        is_last_word: bool,
    ) -> Option<Self> {
        Self::new_with_threshold(text,start_ms,end_ms,line_start_ms,is_bg,is_last_word,1000)
    }

    /// Use the configured held-note threshold while preserving the animation curves.
    pub fn new_with_threshold(text:&str,start_ms:i64,end_ms:i64,line_start_ms:i64,is_bg:bool,is_last_word:bool,threshold_ms:i64)->Option<Self> {
        if end_ms-start_ms < threshold_ms.max(0) {return None;}
        let chars: Vec<&str> = split_graphemes(text);
        // Whitespace words are skipped by the reference before character
        // elements are built, so there is no animation to reproduce.
        if chars.iter().all(|c| c.trim().is_empty()) {
            return None;
        }
        let params = EmphasisParams::new(
            (end_ms - start_ms) as f64,
            (start_ms - line_start_ms) as f64,
            0,
            chars.len(),
            is_last_word,
        );
        let total = chars.len();
        let mut animations = Vec::with_capacity(total);
        for (index, _) in chars.iter().enumerate() {
            let word_delay = params.de
                + params.du / 2.5 / params.anchor_char_count * index as f64;
            animations.push(CharAnimation {
                index,
                word_delay,
                glow: glow_keyframes(index, total, params.amount, params.blur),
                float: float_keyframes(is_bg),
            });
        }
        Some(Self {
            glow_duration: params.animate_du,
            float_duration: params.animate_du * 1.4,
            params,
            chars: animations,
        })
    }

    /// Delay of the trailing lift, which starts 400 ms before the glow.
    pub fn float_delay(&self, char: &CharAnimation) -> f64 {
        char.word_delay - 400.0
    }
}

/// The 32-frame glow curve.
pub fn glow_keyframes(
    char_index: usize,
    total_chars: usize,
    amount: f64,
    blur: f64,
) -> Vec<GlowKeyframe> {
    (0..FRAME_COUNT)
        .map(|j| {
            let x = (j + 1) as f64 / FRAME_COUNT as f64;
            let trans_x = emp_easing(x);
            let glow_level = trans_x * blur;
            GlowKeyframe {
                offset: x,
                scale: 1.0 + trans_x * 0.1 * amount,
                // Characters fan out from the word's centre: the left half
                // moves left and the right half right, so the syllable opens.
                translate_x_em: -trans_x * 0.03 * amount * (total_chars as f64 / 2.0 - char_index as f64),
                translate_y_em: -trans_x * 0.025 * amount,
                shadow_blur_em: (0.3f64).min(blur * 0.3),
                shadow_alpha: glow_level * GLOW_ALPHA_SCALE,
            }
        })
        .collect()
}

/// The 32-frame lift curve: one sine arch, doubled for harmony rows.
pub fn float_keyframes(is_bg: bool) -> Vec<FloatKeyframe> {
    (0..FRAME_COUNT)
        .map(|j| {
            let x = (j + 1) as f64 / FRAME_COUNT as f64;
            let mut y = (x * std::f64::consts::PI).sin();
            if is_bg {
                y *= 2.0;
            }
            FloatKeyframe {
                offset: x,
                translate_y_em: -y * 0.05,
            }
        })
        .collect()
}

/// Sample a keyframe list linearly, which is what Web Animations does here.
pub fn sample_glow(frames: &[GlowKeyframe], at: f64) -> GlowKeyframe {
    match frames {
        [] => GlowKeyframe {
            offset: 0.0,
            scale: 1.0,
            translate_x_em: 0.0,
            translate_y_em: 0.0,
            shadow_blur_em: 0.0,
            shadow_alpha: 0.0,
        },
        [first, ..] => {
            let last = frames.last().expect("checked above");
            if at <= first.offset {
                return *first;
            }
            if at >= last.offset {
                return *last;
            }
            for pair in frames.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if at <= b.offset {
                    let span = b.offset - a.offset;
                    if span <= 0.0 {
                        return b;
                    }
                    let f = (at - a.offset) / span;
                    return GlowKeyframe {
                        offset: at,
                        scale: lerp(a.scale, b.scale, f),
                        translate_x_em: lerp(a.translate_x_em, b.translate_x_em, f),
                        translate_y_em: lerp(a.translate_y_em, b.translate_y_em, f),
                        shadow_blur_em: lerp(a.shadow_blur_em, b.shadow_blur_em, f),
                        shadow_alpha: lerp(a.shadow_alpha, b.shadow_alpha, f),
                    };
                }
            }
            *last
        }
    }
}

/// Sample the lift curve linearly.
pub fn sample_float(frames: &[FloatKeyframe], at: f64) -> f64 {
    match frames {
        [] => 0.0,
        [first, ..] => {
            let last = frames.last().expect("checked above");
            if at <= first.offset {
                return first.translate_y_em;
            }
            if at >= last.offset {
                return last.translate_y_em;
            }
            for pair in frames.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if at <= b.offset {
                    let span = b.offset - a.offset;
                    if span <= 0.0 {
                        return b.translate_y_em;
                    }
                    let f = (at - a.offset) / span;
                    return lerp(a.translate_y_em, b.translate_y_em, f);
                }
            }
            last.translate_y_em
        }
    }
}

/// The reference's composite easing: ease in to the midpoint, ease out after.
pub fn emp_easing(x: f64) -> f64 {
    let mid = 0.5;
    if x < mid {
        BEZ_IN.eval(x / mid)
    } else {
        1.0 - BEZ_OUT.eval((x - mid) / mid)
    }
}

/// Split a word into the grapheme elements AMLL animates.
///
/// AMLL uses `Intl.Segmenter` with grapheme granularity. `unicode-segmentation`
/// is the same Unicode boundary rule, so a flag emoji or a combining sequence
/// stays one element instead of splitting into bytes or code points.
pub fn split_graphemes(text: &str) -> Vec<&str> {
    use unicode_segmentation::UnicodeSegmentation as _;
    text.graphemes(true).collect()
}

fn lerp(a: f64, b: f64, f: f64) -> f64 {
    a + (b - a) * f
}

use oly_core::easing::BezierEasing;

const BEZ_IN: BezierEasing = BezierEasing::new(0.2, 0.4, 0.58, 1.0);
const BEZ_OUT: BezierEasing = BezierEasing::new(0.3, 0.0, 0.58, 1.0);

/// The ordinary-word lift, which every word gets whether or not it emphasises.
///
/// This is a different animation from the long-note lift above: two keyframes
/// and an `ease-out` curve against the long-note path's 32-frame sine arch. The
/// renderer keeps them apart on purpose, because merging them would either give
/// every word a sine arch or give a held syllable an ease-out that lands in the
/// wrong place at the same timestamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlainFloat {
    pub duration_ms: f64,
    /// Offset from the line start, in ms. May be negative when the word starts
    /// before the (optimised) line.
    pub delay_ms: f64,
    /// Peak lift in em; harmony rows lift twice as far.
    pub lift_em: f64,
}

impl PlainFloat {
    pub fn new(start_ms: f64, end_ms: f64, line_start_ms: f64, is_bg: bool) -> Self {
        Self {
            duration_ms: (end_ms - start_ms).max(1000.0),
            delay_ms: start_ms - line_start_ms,
            lift_em: if is_bg { 0.1 } else { 0.05 },
        }
    }

    /// The lift at a given offset into the animation, in em.
    ///
    /// Web Animations evaluates the two-keyframe `ease-out` in the *timing*
    /// domain, so the value is `lift * easeOut(progress)` with the CSS
    /// ease-out cubic-bezier(0, 0, 0.58, 1).
    pub fn lift_at(&self, progress: f64) -> f64 {
        let p = progress.clamp(0.0, 1.0);
        -self.lift_em * CSS_EASE_OUT.eval(p)
    }
}

/// `cubic-bezier(0, 0, 0.58, 1)`, the CSS `ease-out` keyword.
const CSS_EASE_OUT: BezierEasing = BezierEasing::new(0.0, 0.0, 0.58, 1.0);

impl EmphasisPlan {
    /// Animation progress in `[0, 1]` for a playhead, or `None` when the line
    /// the word belongs to is not the one holding the playhead.
    ///
    /// `fill: both` is what the reference relies on: before the word's delay the
    /// first keyframe is already applied, so a held syllable is very slightly
    /// lit from the moment its line becomes active, and after the animation ends
    /// the last (dark) frame is held. Returning `None` outside the active line
    /// is the gate that keeps a rebuilt or future line from lighting up: the
    /// caller cannot obtain a progress value for a line that is not current.
    pub fn progress_at(&self, playhead_ms: f64, line_start_ms: f64, line_is_active: bool) -> Option<f64> {
        if !line_is_active {
            return None;
        }
        let elapsed = playhead_ms - line_start_ms;
        let span = self.params.animate_du;
        if span <= 0.0 {
            return Some(1.0);
        }
        Some(((elapsed - self.params.de) / span).clamp(0.0, 1.0))
    }

    /// The frame to draw for one character at a given progress.
    pub fn char_state(&self, char: &CharAnimation, progress: f64) -> (GlowKeyframe, f64) {
        (
            sample_glow(&char.glow, progress),
            sample_float(&char.float, progress),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emphasis_threshold_is_the_word_duration_only() {
        assert!(!should_emphasize(999));
        assert!(should_emphasize(1000));
        assert!(should_emphasize(1001));
        // The trial build removed the seven-character ceiling, so a long word
        // held for a second animates just like a short one.
        assert!(should_emphasize(1200));
    }

    #[test]
    fn params_follow_the_reference_curve() {
        // 3904 ms held note, started 3856 ms into the line, three characters.
        let p = EmphasisParams::new(3904.0, 3856.0, 0, 3, true);
        assert!((p.amount - 1.2).abs() < 1e-12, "amount {}", p.amount);
        assert!((p.blur - 0.8).abs() < 1e-12, "blur {}", p.blur);
        assert!((p.du - 4684.8).abs() < 1e-9, "du {}", p.du);
        assert!((p.de - 3856.0).abs() < 1e-12);
        assert!((p.anchor_char_count - 3.0).abs() < 1e-12);
        assert!((p.animate_du - 4684.8).abs() < 1e-9);
    }

    #[test]
    fn short_words_use_the_cubic_branch() {
        let p = EmphasisParams::new(1000.0, 1000.0, 0, 2, false);
        assert!((p.amount - 0.075).abs() < 1e-12, "amount {}", p.amount);
        assert!((p.blur - 0.018_518_518_518_518_52).abs() < 1e-12, "blur {}", p.blur);
    }

    #[test]
    fn glow_curve_ends_dark_and_peaks_near_the_middle() {
        let frames = glow_keyframes(0, 1, 1.2, 0.8);
        assert_eq!(frames.len(), FRAME_COUNT);
        let last = frames.last().expect("32 frames");
        assert!(last.shadow_alpha.abs() < 1e-12, "end alpha {}", last.shadow_alpha);
        assert!((last.scale - 1.0).abs() < 1e-12);
        let mid = sample_glow(&frames, 0.5);
        assert!(mid.shadow_alpha > 0.3, "mid alpha {}", mid.shadow_alpha);
        // The blur radius is the word's, not a per-frame value.
        assert!((frames[0].shadow_blur_em - 0.24).abs() < 1e-12);
    }

    #[test]
    fn characters_fan_out_from_the_word_centre() {
        let first = glow_keyframes(0, 4, 1.0, 0.5);
        let last = glow_keyframes(3, 4, 1.0, 0.5);
        let f = sample_glow(&first, 1.0);
        let l = sample_glow(&last, 1.0);
        assert!(f.translate_x_em <= 0.0, "left char {}", f.translate_x_em);
        assert!(l.translate_x_em >= 0.0, "right char {}", l.translate_x_em);
    }

    #[test]
    fn harmony_rows_lift_twice_as_far() {
        let plain = float_keyframes(false);
        let bg = float_keyframes(true);
        let a = sample_float(&plain, 0.5);
        let b = sample_float(&bg, 0.5);
        assert!((a + 0.05).abs() < 1e-12, "plain {}", a);
        assert!((b + 0.1).abs() < 1e-12, "bg {}", b);
    }

    #[test]
    fn whitespace_words_have_no_plan() {
        assert!(EmphasisPlan::new("   ", 0, 5000, 0, false, true).is_none());
        assert!(EmphasisPlan::new("ooh", 0, 5000, 0, false, true).is_some());
    }

    #[test]
    fn plan_delays_trail_per_character() {
        let plan = EmphasisPlan::new("ooh", 75882, 79786, 72026, false, true)
            .expect("three-second note");
        assert_eq!(plan.chars.len(), 3);
        let step = plan.params.du / 2.5 / plan.params.anchor_char_count;
        for (i, ch) in plan.chars.iter().enumerate() {
            let expected = plan.params.de + step * i as f64;
            assert!((ch.word_delay - expected).abs() < 1e-9, "char {i}");
            assert!((plan.float_delay(ch) - (expected - 400.0)).abs() < 1e-9);
        }
    }

    #[test]
    fn graphemes_keep_combining_sequences_whole() {
        // A family emoji is one grapheme made of many code points; splitting it
        // would give every component its own glow animation.
        let parts = split_graphemes("a\u{0301}b");
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[0], "a\u{0301}");
    }
}

#[cfg(test)] mod configurable_threshold_tests {
    use super::*;
    #[test] fn threshold_changes_eligibility_without_changing_the_curve() {
        assert!(EmphasisPlan::new_with_threshold("held",0,800,0,false,false,700).is_some());
        assert!(EmphasisPlan::new_with_threshold("held",0,1500,0,false,false,2000).is_none());
        let configured=EmphasisPlan::new_with_threshold("held",0,1500,0,false,false,1000).unwrap();
        let legacy=EmphasisPlan::new("held",0,1500,0,false,false).unwrap();
        assert_eq!(configured.glow_duration,legacy.glow_duration);
    }
}
