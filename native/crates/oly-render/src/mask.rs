//! Per-word gradient mask: the geometry and the keyframe timeline.
//!
//! The reference masks each word with a horizontal gradient whose fade band is
//! half the word's height wide, then slides that gradient across the word with a
//! keyframe per word boundary. The numbers here are the ones the frozen
//! fixtures recorded (`maskSize` and the two gradient stops), so this module is
//! checked against real recorded values rather than against a re-derivation.

/// One word's mask box.
///
/// `text_width` is the shaped advance, `padding` is one em of side padding that
/// the reference puts on every word element, and `fade_width` is the gradient
/// band width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskSpec {
    pub text_width: f32,
    pub padding: f32,
    pub fade_width: f32,
}

impl MaskSpec {
    pub fn new(text_width: f32, padding: f32, height: f32, word_fade_width: f32) -> Self {
        Self {
            text_width,
            padding,
            fade_width: height * word_fade_width,
        }
    }

    /// Width of the whole word box, padding included.
    pub fn total_width(&self) -> f32 {
        self.text_width + self.padding * 2.0
    }

    /// `fade / total` in the reference's units.
    pub fn width_fraction(&self) -> f32 {
        self.fade_width / self.total_width()
    }

    /// Horizontal mask size as a multiple of the word box (the reference prints
    /// this as a percentage).
    pub fn total_aspect(&self) -> f32 {
        2.0 + self.width_fraction()
    }

    /// Left gradient stop, as a percentage of the mask box.
    pub fn left_percent(&self) -> f32 {
        50.0 - self.width_fraction() / self.total_aspect() * 50.0
    }

    /// Right gradient stop, as a percentage of the mask box.
    pub fn right_percent(&self) -> f32 {
        50.0 + self.width_fraction() / self.total_aspect() * 50.0
    }
}

/// One word as the keyframe generator needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskWord {
    pub start_ms: i64,
    pub end_ms: i64,
    pub width: f32,
    pub padding: f32,
}

impl MaskWord {
    pub fn duration_ms(&self) -> f64 {
        (self.end_ms - self.start_ms).max(0) as f64
    }
}

/// A single stop in the `mask-position` timeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskKeyframe {
    /// Normalized time in `[0, 1]` over the line's duration.
    pub offset: f32,
    /// Mask x offset in pixels.
    pub position: f32,
}

struct Cursor {
    current: f64,
    last: f64,
    time: f64,
    last_time: f64,
    line_duration: f64,
}

impl Cursor {
    fn push(&mut self, frames: &mut Vec<MaskKeyframe>, min_offset: f64) {
        let moved = self.current - self.last;
        let time = self.time.clamp(0.0, 1.0);
        let duration = time - self.last_time;
        let ms_per_pixel = if moved != 0.0 {
            (duration / moved).abs()
        } else {
            0.0
        };

        // The band crossing the word's left edge, then the band crossing the
        // right edge, then the steady position. Each intermediate stop exists
        // only when the band actually straddles a clamp boundary.
        if self.current > min_offset && self.last < min_offset {
            let static_time = (self.last - min_offset).abs() * ms_per_pixel;
            frames.push(MaskKeyframe {
                offset: (self.last_time + static_time) as f32,
                position: self.last.clamp(min_offset, 0.0) as f32,
            });
        }
        if self.current > 0.0 && self.last < 0.0 {
            let static_time = self.last.abs() * ms_per_pixel;
            frames.push(MaskKeyframe {
                offset: (self.last_time + static_time) as f32,
                position: self.current.clamp(min_offset, 0.0) as f32,
            });
        }
        frames.push(MaskKeyframe {
            offset: time as f32,
            position: self.current.clamp(min_offset, 0.0) as f32,
        });
        self.last = self.current;
        self.last_time = time;
    }

    fn step(&mut self, frames: &mut Vec<MaskKeyframe>, min_offset: f64, duration: f64, moved: f64) {
        self.time += duration / self.line_duration;
        self.current += moved;
        if duration > 0.0 {
            self.push(frames, min_offset);
        }
    }
}

/// Generate every mask stop for one word of a line.
///
/// `line_start_ms` is the *optimised* line start, not the first word's start:
/// the reference runs `optimizeLyricLines` first, and `tryAdvanceStartTime`
/// pulls that start up to 600 ms earlier so the first syllable is already lit
/// when the row reaches the focus line. The timeline is normalised against the
/// gap between the line start and the first word too, which is why the first
/// recorded keyframe of a 898 ms line sits at `600 / 1498` rather than at zero.
///
/// `line_duration_ms` is `optimised_end - optimised_start`; the reference
/// normalises every stop against it so one animation drives the whole line.
pub fn word_keyframes(
    words: &[MaskWord],
    target: usize,
    fade_width: f32,
    line_start_ms: i64,
    line_duration_ms: f64,
) -> Vec<MaskKeyframe> {
    let frames = Vec::new();
    if words.is_empty() || target >= words.len() || line_duration_ms <= 0.0 {
        return frames;
    }
    let target_word = words[target];

    // Everything left of the word plus one leading fade band, so the first
    // band starts fully outside the box.
    let width_before: f64 = words[..target].iter().map(|w| w.width as f64).sum::<f64>()
        + if words.is_empty() { 0.0 } else { fade_width as f64 };
    let min_offset =
        -(target_word.width as f64 + target_word.padding as f64 * 2.0 + fade_width as f64);
    let initial = -width_before - target_word.width as f64 - target_word.padding as f64
        - fade_width as f64;

    let mut cursor = Cursor {
        current: initial,
        last: initial,
        time: 0.0,
        last_time: 0.0,
        line_duration: line_duration_ms,
    };
    let mut out = Vec::new();
    cursor.push(&mut out, min_offset);

    let mut last_stamp = 0.0f64;
    for (index, word) in words.iter().enumerate() {
        // `advancePauseTimeline` measures from the line start, not from the
        // first word: the optimiser may have moved the line up to 600 ms
        // earlier, and that lead-in is real time the band must sit still for.
        let stamp = (word.start_ms - line_start_ms) as f64;
        let gap = stamp - last_stamp;
        if gap > 0.0 {
            cursor.step(&mut out, min_offset, gap, 0.0);
        }
        last_stamp = stamp;

        let mut moved = word.width as f64;
        if index == 0 {
            moved += fade_width as f64 * 1.5;
        }
        if index == words.len() - 1 {
            moved += fade_width as f64 * 0.5;
        }
        let duration = word.duration_ms();
        cursor.step(&mut out, min_offset, duration, moved);
        last_stamp += duration;
    }

    let _ = target_word;
    out
}

/// Mask position at a normalized time, linearly interpolated between stops.
pub fn position_at(frames: &[MaskKeyframe], offset: f32) -> f32 {
    match frames.first() {
        None => 0.0,
        Some(first) if offset <= first.offset => first.position,
        Some(_) => {
            for pair in frames.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if offset <= b.offset {
                    let span = b.offset - a.offset;
                    if span <= f32::EPSILON {
                        return b.position;
                    }
                    let t = (offset - a.offset) / span;
                    return a.position + (b.position - a.position) * t;
                }
            }
            frames.last().map(|f| f.position).unwrap_or(0.0)
        }
    }
}

/// Mask alpha at a local x inside the word box.
///
/// The gradient runs bright -> dark across `fade_width` pixels, and the mask box
/// is `total_aspect` word-boxes wide, so the band starts at
/// `position + left_percent * box_width`.
pub fn alpha_at_x(spec: &MaskSpec, mask_position: f32, x: f32, dark: f32, bright: f32) -> f32 {
    let mask_width = spec.total_aspect() * spec.total_width();
    let band_start = mask_position + spec.left_percent() / 100.0 * mask_width;
    let t = ((x - band_start) / spec.fade_width.max(1e-4)).clamp(0.0, 1.0);
    bright + (dark - bright) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_stops_match_the_recorded_word() {
        // hard-times line 0 word 0: 40.66 advance, 1em side padding, 218.264%
        let spec = MaskSpec {
            text_width: 40.66,
            padding: 32.64,
            fade_width: 19.349,
        };
        assert!((spec.total_aspect() * 100.0 - 218.264).abs() < 0.01);
        assert!((spec.left_percent() - 45.816).abs() < 0.01);
        assert!((spec.right_percent() - 54.184).abs() < 0.01);
    }

    #[test]
    fn fully_lit_word_reports_the_bright_alpha() {
        let spec = MaskSpec {
            text_width: 100.0,
            padding: 10.0,
            fade_width: 20.0,
        };
        // total_width 120 with a 20px fade puts the band start at
        // 0.46154 * 260 = 120, so the ramp runs from x=120 to x=140.
        assert_eq!(alpha_at_x(&spec, 0.0, 5.0, 0.5, 1.0), 1.0);
        assert_eq!(alpha_at_x(&spec, 0.0, 119.0, 0.5, 1.0), 1.0);
        assert_eq!(alpha_at_x(&spec, 0.0, 130.0, 0.5, 1.0), 0.75);
        assert_eq!(alpha_at_x(&spec, 0.0, 140.0, 0.5, 1.0), 0.5);
    }

    #[test]
    fn unlit_word_keeps_the_dark_alpha() {
        let spec = MaskSpec {
            text_width: 100.0,
            padding: 10.0,
            fade_width: 20.0,
        };
        let hidden = -(100.0 + 20.0 + 20.0);
        assert_eq!(alpha_at_x(&spec, hidden, 50.0, 0.5, 1.0), 0.5);
    }
}
