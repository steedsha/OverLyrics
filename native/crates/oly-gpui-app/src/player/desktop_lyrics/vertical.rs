use super::*;

/// Vertical layout token: upright CJK/symbol glyph, 90° CW rotated Latin run, or space.
#[derive(Clone, Debug, PartialEq)]
pub enum VerticalToken {
    UprightChar {
        ch: char,
        word_source: usize,
        char_index: usize,
        start_ms: f64,
        end_ms: f64,
    },
    RotatedRun {
        text: String,
        word_source: usize,
        start_ms: f64,
        end_ms: f64,
    },
    Space,
}

fn is_latin_vertical_char(ch: char) -> bool {
    ch.is_ascii_graphic() || (ch.is_alphabetic() && matches!(ch as u32, 0x00C0..=0x024F))
}

fn map_vertical_punct(ch: char) -> char {
    match ch {
        '(' | '（' => '︵',
        ')' | '）' => '︶',
        '「' => '﹁',
        '」' => '﹂',
        '『' => '﹃',
        '』' => '﹄',
        '【' => '︻',
        '】' => '︼',
        '《' => '︽',
        '》' => '︾',
        '〈' => '︿',
        '〉' => '﹀',
        '—' | '–' => '｜',
        '…' => '⋮',
        '~' | '～' => '︱',
        other => other,
    }
}

/// Tokenizes a lyric row into upright CJK chars and 90° clockwise rotated Latin/English runs.
pub fn tokenize_vertical_row(row: &Row) -> Vec<VerticalToken> {
    let mut tokens = Vec::new();
    let words: Vec<(usize, &str, f64, f64)> = if row.words.is_empty() {
        vec![(0, row.text.as_str(), row.start_ms, row.end_ms)]
    } else {
        row.words
            .iter()
            .enumerate()
            .map(|(idx, (t, s, e))| (idx, t.as_str(), *s as f64, *e as f64))
            .collect()
    };

    for (word_source, text, w_start, w_end) in words {
        let chars: Vec<char> = text.chars().collect();
        let total = chars.len().max(1) as f64;
        let span = (w_end - w_start).max(0.0);
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            if ch.is_whitespace() {
                if !matches!(tokens.last(), Some(VerticalToken::Space)) {
                    tokens.push(VerticalToken::Space);
                }
                i += 1;
            } else if is_latin_vertical_char(ch) {
                let start_i = i;
                let mut run = String::new();
                while i < chars.len() && is_latin_vertical_char(chars[i]) {
                    run.push(chars[i]);
                    i += 1;
                }
                let s = w_start + span * (start_i as f64 / total);
                let e = w_start + span * (i as f64 / total);
                tokens.push(VerticalToken::RotatedRun {
                    text: run,
                    word_source,
                    start_ms: s,
                    end_ms: e,
                });
            } else {
                let mapped = map_vertical_punct(ch);
                let s = w_start + span * (i as f64 / total);
                let e = w_start + span * ((i + 1) as f64 / total);
                tokens.push(VerticalToken::UprightChar {
                    ch: mapped,
                    word_source,
                    char_index: i,
                    start_ms: s,
                    end_ms: e,
                });
                i += 1;
            }
        }
    }
    while matches!(tokens.first(), Some(VerticalToken::Space)) {
        tokens.remove(0);
    }
    while matches!(tokens.last(), Some(VerticalToken::Space)) {
        tokens.pop();
    }
    tokens
}
