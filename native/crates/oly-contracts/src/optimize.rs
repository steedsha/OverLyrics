//! AMLL's pre-render line optimisation.
//!
//! The reference renderer never uses the raw `Doc -> AMLL` output. Before the
//! first frame it runs `optimizeLyricLines`, which rewrites line start/end times
//! in place, and every later stage — mask keyframes, focus, scrolling — reads
//! those rewritten values. A port that skips this step looks right for a line
//! whose neighbours are far away and then drifts by up to 600 ms on everything
//! that follows, because `tryAdvanceStartTime` deliberately starts a line early
//! so its first syllable is already lit when it reaches the focus row.
//!
//! The transcription below keeps the reference's order of operations, because
//! the steps are not independent: `resetLineTimestamps` moves a line's start onto
//! its first word, `syncMainAndBackgroundLines` widens a main line to cover its
//! harmony row, `sortLyricLines` re-orders by the start just computed, and only
//! then do overlap cleaning and the 600 ms advance run.

use crate::lyrics::AmllLine;

/// Which steps run. The reference exposes all five as display options, and the
/// frozen profile leaves them at the shipped defaults.
#[derive(Clone, Copy, Debug)]
pub struct OptimizeOptions {
    pub normalize_spaces: bool,
    pub reset_line_timestamps: bool,
    pub sync_main_and_background_lines: bool,
    pub clean_unintentional_overlaps: bool,
    pub try_advance_start_time: bool,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            normalize_spaces: true,
            reset_line_timestamps: true,
            sync_main_and_background_lines: true,
            clean_unintentional_overlaps: true,
            try_advance_start_time: true,
        }
    }
}

/// The two advance amounts the reference hard-codes.
const DEFAULT_ADVANCE_MS: f64 = 600.0;
const OVERLAP_ADVANCE_MS: f64 = 400.0;
const OVERLAP_ADVANCE_RATIO: f64 = 0.7;

/// Rewrite `lines` the way the reference does before rendering.
pub fn optimize_lyric_lines(lines: &mut [AmllLine], options: &OptimizeOptions) {
    if options.normalize_spaces {
        normalize_spaces(lines);
    }
    if options.reset_line_timestamps {
        reset_line_timestamps(lines);
    }
    convert_excessive_background_lines(lines);
    if options.sync_main_and_background_lines {
        sync_main_and_background_lines(lines);
    }
    sort_lyric_lines(lines);
    if options.clean_unintentional_overlaps {
        clean_unintentional_overlaps(lines, options.sync_main_and_background_lines);
    }
    if options.try_advance_start_time {
        try_advance_start_time(lines, options.sync_main_and_background_lines);
    }
}

/// Collapse runs of whitespace inside every word to a single space.
///
/// This is `word.replace(/\s+/g, " ")` in the reference, which collapses runs
/// but does **not** trim. A word like `"All "` keeps its single trailing space,
/// and that space is load-bearing: it is what separates it from the next word
/// when the line is laid out as one string. Trimming here silently welds every
/// Latin word together on the masked path while the timings stay correct.
fn normalize_spaces(lines: &mut [AmllLine]) {
    for line in lines.iter_mut() {
        for word in line.words.iter_mut() {
            if word.word.contains(char::is_whitespace) {
                let mut collapsed = String::with_capacity(word.word.len());
                let mut in_whitespace = false;
                for ch in word.word.chars() {
                    if ch.is_whitespace() {
                        if !in_whitespace {
                            collapsed.push(' ');
                            in_whitespace = true;
                        }
                    } else {
                        collapsed.push(ch);
                        in_whitespace = false;
                    }
                }
                word.word = collapsed;
            }
        }
    }
}

/// Force line timestamps to agree with their word timestamps.
///
/// A line-level chart arrives as one word at `0..0`; that placeholder is
/// replaced by the line's own window rather than the other way round, otherwise
/// every line-level file collapses onto time zero.
fn reset_line_timestamps(lines: &mut [AmllLine]) {
    for line in lines.iter_mut() {
        if line.words.len() == 1
            && line.words[0].start_time == 0
            && line.words[0].end_time == 0
            && (line.start_time != 0.0 || line.end_time != 0.0)
        {
            // The placeholder is replaced by the line's own window, which is
            // still an integer at this point: the advance runs later.
            line.words[0].start_time = line.start_time as i64;
            line.words[0].end_time = line.end_time as i64;
        } else if !line.words.is_empty() {
            line.start_time = line.words[0].start_time as f64;
            line.end_time = line.words[line.words.len() - 1].end_time as f64;
        }
    }
}

/// A background row with no lead row before it, or two background rows in a
/// row, cannot be rendered as harmony: promote them to lead rows.
fn convert_excessive_background_lines(lines: &mut [AmllLine]) {
    let mut consecutive_bg = 0usize;
    for (i, line) in lines.iter_mut().enumerate() {
        if line.is_bg {
            consecutive_bg += 1;
            if i == 0 || consecutive_bg > 1 {
                line.is_bg = false;
            }
        } else {
            consecutive_bg = 0;
        }
    }
}

/// Give a lead row and its harmony row one shared window, wide enough for both.
fn sync_main_and_background_lines(lines: &mut [AmllLine]) {
    if lines.len() < 2 {
        return;
    }
    for i in (0..lines.len() - 1).rev() {
        if lines[i].is_bg {
            continue;
        }
        if !lines[i + 1].is_bg {
            continue;
        }
        let mut final_start = lines[i].start_time.min(lines[i + 1].start_time);
        let mut final_end = lines[i].end_time.max(lines[i + 1].end_time);
        for word in lines[i]
            .words
            .iter()
            .chain(lines[i + 1].words.iter())
            .filter(|w| !w.word.trim().is_empty())
        {
            final_start = final_start.min(word.start_time as f64);
            final_end = final_end.max(word.end_time as f64);
        }
        for line in lines[i..=i + 1].iter_mut() {
            line.start_time = final_start;
            line.end_time = final_end;
        }
    }
}

/// Stable sort by start time, keeping each harmony row glued to its lead row.
fn sort_lyric_lines(lines: &mut [AmllLine]) {
    let mut groups: Vec<(f64, usize, Vec<AmllLine>)> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let mut group = vec![lines[i].clone()];
        let start = lines[i].start_time;
        if !lines[i].is_bg && i + 1 < lines.len() && lines[i + 1].is_bg {
            i += 1;
            group.push(lines[i].clone());
        }
        groups.push((start, groups.len(), group));
        i += 1;
    }
    // f64 is not Ord, so the pair is compared explicitly; ties keep the order
    // the projection produced, which is what a stable sort guarantees.
    groups.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out = 0usize;
    for (_, _, group) in groups {
        for line in group {
            lines[out] = line;
            out += 1;
        }
    }
}

/// Trim overlaps the reference considers accidental.
///
/// An overlap counts as intentional when it is at least 500 ms, or longer than
/// 100 ms and more than a tenth of the following line. Anything shorter is a
/// provider artefact and is cut at the next line's start.
fn clean_unintentional_overlaps(lines: &mut [AmllLine], sync_background_lines: bool) {
    if lines.len() < 2 {
        return;
    }
    for i in 0..lines.len() - 1 {
        if lines[i].is_bg {
            continue;
        }
        for j in i + 1..lines.len() {
            if lines[j].is_bg {
                continue;
            }
            let overlap = lines[i].end_time - lines[j].start_time;
            if overlap <= 0.0 {
                break;
            }
            let next_duration = lines[j].end_time - lines[j].start_time;
            let percentage_threshold = next_duration * 0.1;
            let intentional = overlap >= 500.0
                || (overlap > 100.0 && overlap > percentage_threshold);
            if !intentional {
                lines[i].end_time = lines[j].start_time;
                if sync_background_lines && i + 1 < lines.len() && lines[i + 1].is_bg {
                    lines[i + 1].end_time = lines[j].start_time;
                }
                break;
            }
        }
    }
}

/// Start each line up to 600 ms early, bounded by the previous lead group.
///
/// This is what makes the first syllable already lit as the line arrives. The
/// bound is what keeps a line from being pushed into its predecessor's window,
/// so a dense chart degrades to a smaller advance instead of overlapping.
fn try_advance_start_time(lines: &mut [AmllLine], sync_background_lines: bool) {
    let mut prev_line_start: f64 = 0.0;
    let mut prev_line_end: f64 = 0.0;
    let mut prev_group_start: f64 = 0.0;
    let mut prev_group_end: f64 = 0.0;
    let mut has_prev = false;

    for i in 0..lines.len() {
        if lines[i].is_bg {
            continue;
        }
        let original_start = lines[i].start_time;
        let original_end = lines[i].end_time;

        let (advance, safe_boundary) = if has_prev {
            if original_start >= prev_line_end {
                (DEFAULT_ADVANCE_MS, prev_group_end)
            } else {
                let overlap = prev_line_end - original_start;
                let advance = if overlap < OVERLAP_ADVANCE_MS {
                    overlap * OVERLAP_ADVANCE_RATIO
                } else {
                    OVERLAP_ADVANCE_MS
                };
                (advance, prev_line_start)
            }
        } else {
            (DEFAULT_ADVANCE_MS, 0.0)
        };

        // Kept as a double, like the reference: this is where a fractional
        // start is born, and later stages consume it unchanged.
        let new_start = (lines[i].start_time - advance).max(safe_boundary);
        if new_start < lines[i].start_time {
            lines[i].start_time = new_start;
        }
        if sync_background_lines && i + 1 < lines.len() && lines[i + 1].is_bg {
            lines[i + 1].start_time = lines[i].start_time;
        }

        if has_prev {
            if original_start < prev_group_end && original_end > prev_group_start {
                prev_group_start = prev_group_start.min(original_start);
                prev_group_end = prev_group_end.max(original_end);
            } else {
                prev_group_start = original_start;
                prev_group_end = original_end;
            }
        } else {
            prev_group_start = original_start;
            prev_group_end = original_end;
        }
        prev_line_start = lines[i].start_time;
        prev_line_end = original_end;
        has_prev = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::lyrics::AmllWord;

    fn word(text: &str, start: i64, end: i64) -> AmllWord {
        AmllWord {
            start_time: start,
            end_time: end,
            word: text.to_string(),
        }
    }

    fn line(text: &str, start: i64, end: i64) -> AmllLine {
        AmllLine {
            words: vec![word(text, start, end)],
            translated_lyric: String::new(),
            roman_lyric: String::new(),
            start_time: start as f64,
            end_time: end as f64,
            is_bg: false,
            is_duet: false,
            src_index: 0,
        }
    }

    #[test]
    fn a_line_starts_up_to_six_hundred_ms_early() {
        let mut lines = vec![line("a", 10_000, 12_000), line("b", 20_000, 22_000)];
        optimize_lyric_lines(&mut lines, &OptimizeOptions::default());
        assert_eq!(lines[0].start_time, 9_400.0);
        // The second line may not advance past the first line's end.
        assert_eq!(lines[1].start_time, 19_400.0);
    }

    #[test]
    fn a_dense_line_advances_only_by_the_fallback_amount() {
        // The 300 ms overlap has to survive `clean_unintentional_overlaps`
        // first: it is more than a tenth of the next line, so the cleaner calls
        // it deliberate and `try_advance_start_time` sees it. Below the 400 ms
        // fallback the advance is 70% of the overlap, not the full 600 ms.
        let mut lines = vec![line("a", 10_000, 14_700), line("b", 14_400, 15_000)];
        optimize_lyric_lines(&mut lines, &OptimizeOptions::default());
        assert_eq!(lines[1].start_time, 14_190.0);
    }

    #[test]
    fn a_deliberate_overlap_uses_the_fallback_advance() {
        // A 600 ms overlap reads as intentional, so the cleaner keeps it and
        // the advance takes the 400 ms fallback rather than 70% of the overlap.
        let mut lines = vec![line("a", 10_000, 15_000), line("b", 14_400, 20_000)];
        optimize_lyric_lines(&mut lines, &OptimizeOptions::default());
        assert_eq!(lines[1].start_time, 14_000.0);
    }

    #[test]
    fn a_cleaned_overlap_blocks_the_advance_at_the_previous_end() {
        // A 200 ms overlap is accidental, so the cleaner trims the first line to
        // the second line's start. The advance then has no room left: its safe
        // boundary is that same instant, so the start must not move.
        let mut lines = vec![line("a", 10_000, 15_000), line("b", 14_800, 20_000)];
        optimize_lyric_lines(&mut lines, &OptimizeOptions::default());
        assert_eq!(lines[0].end_time, 14_800.0);
        assert_eq!(lines[1].start_time, 14_800.0);
    }

    #[test]
    fn line_level_words_take_the_line_window() {
        let mut lines = vec![AmllLine {
            words: vec![word("whole line", 0, 0)],
            translated_lyric: String::new(),
            roman_lyric: String::new(),
            start_time: 5_000.0,
            end_time: 7_000.0,
            is_bg: false,
            is_duet: false,
            src_index: 0,
        }];
        optimize_lyric_lines(&mut lines, &OptimizeOptions::default());
        assert_eq!(lines[0].words[0].start_time, 5_000);
        assert_eq!(lines[0].words[0].end_time, 7_000);
    }

    #[test]
    fn a_leading_background_row_is_promoted() {
        let mut lines = vec![line("bg", 1_000, 2_000), line("lead", 3_000, 4_000)];
        lines[0].is_bg = true;
        optimize_lyric_lines(&mut lines, &OptimizeOptions::default());
        assert!(!lines[0].is_bg);
    }

    #[test]
    fn accidental_overlaps_are_trimmed_but_musical_ones_survive() {
        let mut short = vec![line("a", 1_000, 3_000), line("b", 2_900, 6_000)];
        optimize_lyric_lines(&mut short, &OptimizeOptions::default());
        assert_eq!(short[0].end_time, 2_900.0);

        let mut long = vec![line("a", 1_000, 3_000), line("b", 2_000, 6_000)];
        optimize_lyric_lines(&mut long, &OptimizeOptions::default());
        assert_eq!(long[0].end_time, 3_000.0);
    }
}
