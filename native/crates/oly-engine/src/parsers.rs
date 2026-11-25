//! Lyric text parsers: LRC (with ESLRC word stamps), QRC, KRC, plain text.
//!
//! Ported 1:1 from `src/engine/parsers.ts`. Every quirk here is load-bearing:
//! the QRC word grammar puts the **text before** its `(start,dur)` tag while KRC
//! puts the tag first, the KRC offsets are relative to the line start while QRC's
//! are absolute, and the `[mm:ss:xx]` colon-fraction form exists because users
//! hand-edit LRC files that way. The goldens under
//! `native/tests/fixtures/oracle/engine/parse.json` pin all of it.

use std::collections::BTreeMap;

use regex::Regex;
use std::sync::LazyLock;

use crate::types::{ParsedLine, ParsedLyrics, Word};

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static parser regex compiles")
}

/// `[mm:ss.xx]` / `[mm:ss.xxx]` / `[m:ss]` / `[mm:ss:xx]` (colon fraction) → ms.
pub fn parse_lrc_time(tag: &str) -> Option<i64> {
    static DOT: LazyLock<Regex> =
        LazyLock::new(|| re(r"^\s*(\d+):(\d+(?:\.\d+)?)\s*$"));
    static COLON: LazyLock<Regex> =
        LazyLock::new(|| re(r"^\s*(\d{1,3}):(\d{1,2}):(\d{1,3})\s*$"));

    if let Some(m) = DOT.captures(tag) {
        let minutes: i64 = m[1].parse::<i64>().ok()?;
        let seconds: f64 = m[2].parse::<f64>().ok()?;
        return Some(((minutes as f64 * 60.0 + seconds) * 1000.0).round() as i64);
    }
    // `[00:12:34]` means 12.34 seconds: some sources, and users hand-editing an
    // LRC, use a colon as the decimal separator.
    if let Some(m) = COLON.captures(tag) {
        let minutes: i64 = m[1].parse::<i64>().ok()?;
        let seconds: i64 = m[2].parse::<i64>().ok()?;
        let frac_text = &m[3];
        let frac = frac_text.parse::<f64>().ok()? / 10f64.powi(frac_text.len() as i32);
        return Some(((minutes as f64 * 60.0 + seconds as f64 + frac) * 1000.0).round() as i64);
    }
    None
}

/// `[offset:-500]` → ms; absent means 0.
pub fn lrc_offset(text: &str) -> i64 {
    static OFFSET: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\[offset:\s*([+-]?\d+)\s*\]").expect("offset regex"));
    OFFSET
        .captures(text)
        .and_then(|m| m[1].parse::<i64>().ok())
        .unwrap_or(0)
}

/// Blank lines and `//`-only lines never occupy a row.
pub fn is_placeholder(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true;
    }
    static SLASHES: LazyLock<Regex> = LazyLock::new(|| re(r"^/+$"));
    SLASHES.is_match(t)
}

/// A row with no end (or an end at/before its start) runs until the next row
/// starts; the last row gets five seconds.
fn fill_gaps(lines: &mut [ParsedLine]) {
    for i in 0..lines.len() {
        let invalid = match (lines[i].start, lines[i].end) {
            (Some(start), Some(end)) => end <= start,
            (_, None) => true,
            (None, Some(_)) => false,
        };
        if invalid {
            let start = lines[i].start.unwrap_or(0);
            let next = lines.get(i + 1).and_then(|l| l.start);
            lines[i].end = Some(next.unwrap_or(start + 5000));
        }
    }
}

/// Drop a repeated `(start, text)` row; a translation on the dropped row is
/// inherited by the survivor.
fn dedupe(lines: Vec<ParsedLine>) -> Vec<ParsedLine> {
    let mut out: Vec<ParsedLine> = Vec::with_capacity(lines.len());
    for l in lines {
        match out.last_mut() {
            Some(prev) if prev.start == l.start && prev.text == l.text => {
                // ParsedLine carries no translation yet; nothing to inherit here,
                // but the branch must still swallow the duplicate.
            }
            _ => out.push(l),
        }
    }
    out
}

/// Close every word window: a word ends where the next one starts, and the last
/// word ends with its line.
fn close_words(lines: &mut [ParsedLine]) {
    for l in lines.iter_mut() {
        let line_end = l.end;
        let starts: Vec<Option<i64>> = l.words.iter().map(|w| w.start).collect();
        for i in 0..l.words.len() {
            if l.words[i].end.is_none() {
                l.words[i].end = if i + 1 < starts.len() {
                    starts[i + 1]
                } else {
                    line_end
                };
            }
        }
    }
}

/// Split `rest` on a delimiter regex with one capture group, keeping the
/// delimiters as their own segments, exactly like `String.split(/(...)/)`.
fn split_keep(rest: &str, delim: &Regex) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cursor = 0usize;
    for m in delim.find_iter(rest) {
        if m.start() > cursor {
            parts.push(rest[cursor..m.start()].to_string());
        }
        parts.push(m.as_str().to_string());
        cursor = m.end();
    }
    if cursor < rest.len() {
        parts.push(rest[cursor..].to_string());
    }
    parts
}

/// Parse LRC, including the enhanced `<mm:ss.xx>` per-word stamps.
pub fn parse_lrc(raw: &str) -> Option<ParsedLyrics> {
    static GATE: LazyLock<Regex> =
        LazyLock::new(|| re(r"\[\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?\]"));
    static STAMP: LazyLock<Regex> =
        LazyLock::new(|| re(r"^\[(\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?)\]"));
    static TAG: LazyLock<Regex> = LazyLock::new(|| re(r"^\[([a-zA-Z#]+):([^\]]*)\]$"));
    static WORD_MARK: LazyLock<Regex> =
        LazyLock::new(|| re(r"<\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?>"));
    static WORD_SPLIT: LazyLock<Regex> =
        LazyLock::new(|| re(r"(<\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?>)"));

    let text = raw.replace("\r\n", "\n").replace('\r', "\n");
    if !GATE.is_match(&text) {
        return None;
    }

    let offset = lrc_offset(&text);
    let mut lines: Vec<ParsedLine> = Vec::new();
    let mut meta: BTreeMap<String, String> = BTreeMap::new();

    for raw_line in text.split('\n') {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        // Consecutive timestamps at the head of the line.
        let mut stamps: Vec<String> = Vec::new();
        let mut rest = line.to_string();
        while let Some(m) = STAMP.captures(&rest) {
            stamps.push(m[1].to_string());
            let whole = m.get(0).unwrap();
            rest = rest[whole.end()..].to_string();
        }
        if stamps.is_empty() {
            if let Some(tag) = TAG.captures(line) {
                meta.insert(tag[1].to_lowercase(), tag[2].trim().to_string());
            }
            continue;
        }

        // Enhanced LRC per-word stamps.
        let mut words: Vec<Word> = Vec::new();
        if WORD_MARK.is_match(&rest) {
            let parts = split_keep(&rest, &WORD_SPLIT);
            let mut pending: Option<Option<i64>> = None;
            let mut buf = String::new();
            for p in parts {
                if p.starts_with('<') {
                    if let Some(start) = pending {
                        words.push(Word {
                            start,
                            end: None,
                            text: std::mem::take(&mut buf),
                        });
                    }
                    pending = Some(parse_lrc_time(&p[1..p.len() - 1]));
                    buf.clear();
                } else {
                    buf.push_str(&p);
                }
            }
            if let Some(start) = pending {
                words.push(Word {
                    start,
                    end: None,
                    text: std::mem::take(&mut buf),
                });
            }
            rest = words.iter().map(|w| w.text.as_str()).collect();
        }

        let body = rest.trim().to_string();
        if is_placeholder(&body) && words.is_empty() {
            continue;
        }

        for s in &stamps {
            let Some(t) = parse_lrc_time(s) else { continue };
            let shifted: Vec<Word> = words
                .iter()
                .map(|w| Word {
                    start: w.start.map(|x| x + offset),
                    end: w.end,
                    text: w.text.clone(),
                })
                .collect();
            lines.push(ParsedLine {
                start: Some(t + offset),
                end: None,
                text: body.clone(),
                words: shifted,
                ..Default::default()
            });
        }
    }

    if lines.is_empty() {
        return None;
    }
    lines.sort_by_key(|l| l.start.unwrap_or(i64::MIN));
    fill_gaps(&mut lines);

    let word_lines = lines.iter().filter(|l| l.words.len() > 1).count();
    close_words(&mut lines);
    let count = lines.len();

    Some(ParsedLyrics {
        format: "lrc".to_string(),
        timing: Some(if word_lines > 0 { "Word" } else { "Line" }.to_string()),
        lang: Some(String::new()),
        lines: dedupe(lines),
        has_word_level: word_lines > 0,
        has_translation: false,
        meta: Some(meta),
        plain_only: false,
        has_romanization: None,
        has_background: None,
        duet: None,
        credits: None,
    })
    .filter(|_| count > 0)
}

/// Parse QRC (QQ Music word-level, after decryption).
pub fn parse_qrc(raw: &str) -> Option<ParsedLyrics> {
    static GATE: LazyLock<Regex> = LazyLock::new(|| re(r"\[\d+,\d+\]"));
    static CONTENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?s)LyricContent="(.*?)"\s*/?>"#).expect("content regex"));
    static HEAD: LazyLock<Regex> = LazyLock::new(|| re(r"^\[(\d+),(\d+)\](.*)$"));
    static TAG: LazyLock<Regex> = LazyLock::new(|| re(r"^\[([a-zA-Z#]+):([^\]]*)\]$"));
    static HAS_PAIR: LazyLock<Regex> = LazyLock::new(|| re(r"\(\d+,\d+\)"));
    static PAIR: LazyLock<Regex> = LazyLock::new(|| re(r"\((\d+),(\d+)\)"));
    static HAS_ANGLE: LazyLock<Regex> = LazyLock::new(|| re(r"<\d+,\d+(?:,\d+)?>"));
    static ANGLE_SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r"(<\d+,\d+(?:,\d+)?>)"));
    static ANGLE: LazyLock<Regex> = LazyLock::new(|| re(r"^<(\d+),(\d+)(?:,\d+)?>$"));

    if !GATE.is_match(raw) {
        return None;
    }

    let mut body = raw.to_string();
    if let Some(m) = CONTENT.captures(raw) {
        body = m[1].to_string();
    }
    body = body
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">");

    let offset = lrc_offset(&body);
    let mut lines: Vec<ParsedLine> = Vec::new();
    let mut meta: BTreeMap<String, String> = BTreeMap::new();

    static LINE_SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r"[\r\n]+"));
    for raw_line in LINE_SPLIT.split(&body) {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(head) = HEAD.captures(line) else {
            if let Some(tag) = TAG.captures(line) {
                meta.insert(tag[1].to_lowercase(), tag[2].trim().to_string());
            }
            continue;
        };
        let start: i64 = head[1].parse::<i64>().ok()? + offset;
        let dur: i64 = head[2].parse::<i64>().ok()?;
        let mut rest = head[3].to_string();

        let mut words: Vec<Word> = Vec::new();
        // QRC writes the text **before** its `(start,dur)` tag; the tag carries
        // absolute milliseconds.
        if HAS_PAIR.is_match(&rest) {
            let mut cursor = 0usize;
            for m in PAIR.captures_iter(&rest.clone()) {
                let whole = m.get(0).unwrap();
                let st: i64 = m[1].parse::<i64>().ok()? + offset;
                let wdur: i64 = m[2].parse::<i64>().ok()?;
                words.push(Word {
                    start: Some(st),
                    end: Some(st + wdur),
                    text: rest[cursor..whole.start()].to_string(),
                });
                cursor = whole.end();
            }
            if !words.is_empty() && cursor < rest.len() {
                let tail = rest[cursor..].to_string();
                if let Some(last) = words.last_mut() {
                    last.text.push_str(&tail);
                }
            }
            rest = words.iter().map(|w| w.text.as_str()).collect();
        } else if HAS_ANGLE.is_match(&rest) {
            let parts = split_keep(&rest, &ANGLE_SPLIT);
            let mut cur: Option<Word> = None;
            for p in parts {
                if let Some(pm) = ANGLE.captures(&p) {
                    if let Some(done) = cur.take() {
                        if !done.text.is_empty() {
                            words.push(done);
                        }
                    }
                    let off: i64 = pm[1].parse::<i64>().ok()?;
                    let wdur: i64 = pm[2].parse::<i64>().ok()?;
                    cur = Some(Word {
                        start: Some(start + off),
                        end: (wdur > 0).then_some(start + off + wdur),
                        text: String::new(),
                    });
                } else if let Some(open) = cur.as_mut() {
                    open.text.push_str(&p);
                }
            }
            if let Some(done) = cur.take() {
                if !done.text.is_empty() {
                    words.push(done);
                }
            }
            rest = words.iter().map(|w| w.text.as_str()).collect();
        }

        let txt = rest.trim().to_string();
        if is_placeholder(&txt) && words.is_empty() {
            continue;
        }
        lines.push(ParsedLine {
            start: Some(start),
            end: Some(start + dur),
            text: txt,
            words,
            ..Default::default()
        });
    }

    if lines.is_empty() {
        return None;
    }
    lines.sort_by_key(|l| l.start.unwrap_or(i64::MIN));
    fill_gaps(&mut lines);
    let word_lines = lines.iter().filter(|l| l.words.len() > 1).count();
    close_words(&mut lines);

    Some(ParsedLyrics {
        format: "qrc".to_string(),
        timing: Some(if word_lines > 0 { "Word" } else { "Line" }.to_string()),
        lang: Some(String::new()),
        lines: dedupe(lines),
        has_word_level: word_lines > 0,
        has_translation: false,
        meta: Some(meta),
        plain_only: false,
        has_romanization: None,
        has_background: None,
        duet: None,
        credits: None,
    })
}

/// Parse KRC (Kugou word-level, after decryption). Angle-bracket offsets are
/// relative to the line start.
pub fn parse_krc(raw: &str) -> Option<ParsedLyrics> {
    static GATE: LazyLock<Regex> = LazyLock::new(|| re(r"\[\d+,\d+\]"));
    static HEAD: LazyLock<Regex> = LazyLock::new(|| re(r"^\[(\d+),(\d+)\](.*)$"));
    static TAG: LazyLock<Regex> = LazyLock::new(|| re(r"^\[([a-zA-Z#]+):([^\]]*)\]$"));
    static ANGLE_SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r"(<\d+,\d+(?:,\d+)?>)"));
    static ANGLE: LazyLock<Regex> = LazyLock::new(|| re(r"^<(\d+),(\d+)(?:,\d+)?>$"));

    if !GATE.is_match(raw) {
        return None;
    }

    let offset = lrc_offset(raw);
    let mut lines: Vec<ParsedLine> = Vec::new();
    let mut meta: BTreeMap<String, String> = BTreeMap::new();

    static LINE_SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r"[\r\n]+"));
    for raw_line in LINE_SPLIT.split(raw) {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(head) = HEAD.captures(line) else {
            if let Some(tag) = TAG.captures(line) {
                meta.insert(tag[1].to_lowercase(), tag[2].trim().to_string());
            }
            continue;
        };
        let start: i64 = head[1].parse::<i64>().ok()? + offset;
        let dur: i64 = head[2].parse::<i64>().ok()?;
        let rest = head[3].to_string();

        let mut words: Vec<Word> = Vec::new();
        let parts = split_keep(&rest, &ANGLE_SPLIT);
        let mut cur: Option<Word> = None;
        for p in parts {
            if let Some(pm) = ANGLE.captures(&p) {
                if let Some(done) = cur.take() {
                    if !done.text.is_empty() {
                        words.push(done);
                    }
                }
                let off: i64 = pm[1].parse::<i64>().ok()?;
                let wdur: i64 = pm[2].parse::<i64>().ok()?;
                cur = Some(Word {
                    start: Some(start + off),
                    end: (wdur > 0).then_some(start + off + wdur),
                    text: String::new(),
                });
            } else if let Some(open) = cur.as_mut() {
                open.text.push_str(&p);
            }
        }
        if let Some(done) = cur.take() {
            if !done.text.is_empty() {
                words.push(done);
            }
        }

        let txt = if words.is_empty() {
            rest.trim().to_string()
        } else {
            words.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_string()
        };
        if is_placeholder(&txt) {
            continue;
        }
        lines.push(ParsedLine {
            start: Some(start),
            end: Some(start + dur),
            text: txt,
            words,
            ..Default::default()
        });
    }

    if lines.is_empty() {
        return None;
    }
    lines.sort_by_key(|l| l.start.unwrap_or(i64::MIN));
    fill_gaps(&mut lines);
    let word_lines = lines.iter().filter(|l| l.words.len() > 1).count();
    close_words(&mut lines);
    for l in &mut lines {
        if let (Some(line_start), Some(last_end)) = (
            l.start,
            l.words
                .iter()
                .rev()
                .find(|w| !w.text.trim().is_empty())
                .and_then(|w| w.end),
        ) {
            if last_end > line_start {
                l.end = Some(last_end);
            }
        }
    }

    Some(ParsedLyrics {
        format: "krc".to_string(),
        timing: Some(if word_lines > 0 { "Word" } else { "Line" }.to_string()),
        lang: Some(String::new()),
        lines: dedupe(lines),
        has_word_level: word_lines > 0,
        has_translation: false,
        meta: Some(meta),
        plain_only: false,
        has_romanization: None,
        has_background: None,
        duet: None,
        credits: None,
    })
}

/// Untimed fallback: one row per non-metadata line.
pub fn parse_plain(raw: &str) -> ParsedLyrics {
    static TAG: LazyLock<Regex> = LazyLock::new(|| re(r"^\[[a-zA-Z#]+:[^\]]*\]$"));
    static LINE_SPLIT: LazyLock<Regex> = LazyLock::new(|| re(r"\r?\n"));

    let mut lines = Vec::new();
    for raw_line in LINE_SPLIT.split(raw) {
        let line = raw_line.trim();
        if line.is_empty() || TAG.is_match(line) {
            continue;
        }
        lines.push(ParsedLine {
            start: None,
            end: None,
            text: line.to_string(),
            words: Vec::new(),
            ..Default::default()
        });
    }

    ParsedLyrics {
        format: "plain".to_string(),
        timing: None,
        lang: None,
        has_word_level: false,
        has_translation: false,
        lines,
        meta: None,
        plain_only: true,
        has_romanization: None,
        has_background: None,
        duet: None,
        credits: None,
    }
}

// ---------------------------------------------------------------- TTML ----
//
// The AMLL community library ships hand-authored TTML: per-syllable `<span>`s,
// a `ttm:role="x-translation"` span per line, and `x-bg` harmony blocks with
// their own word timeline and translation. The oracle exports no TTML case, so
// this block is ported straight from `parseTtml` in `src/engine/parsers.ts` and
// pinned by `tests/ttml_parity.rs`.

/// A `<span>…</span>` block read out of a TTML body.
struct SpanBlock {
    inner: String,
    /// Byte offset just past the closing tag.
    end: usize,
    attrs: String,
}

/// `MM:SS.mmm`, `HH:MM:SS.mmm`, a bare `SS.mmm`, or an offset-time with a
/// `ms`/`s`/`m`/`h` suffix. AMLL submissions mix the bare-second form with the
/// clock form in one file, so both must parse.
pub fn parse_ttml_time(value: &str) -> Option<i64> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }
    static OFFSET: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(\d+(?:\.\d+)?)(ms|s|m|h)?$"));
    if let Some(m) = OFFSET.captures(text) {
        let value: f64 = m[1].parse().ok()?;
        let factor = match m.get(2).map(|x| x.as_str()).unwrap_or("s") {
            "ms" => 1.0,
            "s" => 1000.0,
            "m" => 60_000.0,
            _ => 3_600_000.0,
        };
        return Some((value * factor).round() as i64);
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut seconds = 0.0f64;
    for part in parts {
        let value: f64 = part.trim().parse().ok()?;
        seconds = seconds * 60.0 + value;
    }
    Some((seconds * 1000.0).round() as i64)
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Earliest `<span` / `</span` at or after `from`, with whether it closes.
fn next_span_tag(html: &str, from: usize) -> Option<(usize, bool)> {
    let open = html[from..].find("<span").map(|i| from + i);
    let close = html[from..].find("</span").map(|i| from + i);
    match (open, close) {
        (Some(o), Some(c)) => Some(if o <= c { (o, false) } else { (c, true) }),
        (Some(o), None) => Some((o, false)),
        (None, Some(c)) => Some((c, true)),
        (None, None) => None,
    }
}

/// Read one possibly nested `<span>` block starting exactly at `pos`.
///
/// Self-closing tags (`<span/>`) neither open nor close a level: treating one
/// as an opening tag leaves the depth permanently unbalanced and swallows the
/// rest of the line.
fn read_span_block(html: &str, pos: usize) -> Option<SpanBlock> {
    if !html[pos..].starts_with("<span") {
        return None;
    }
    let bytes = html.as_bytes();
    let gt = pos + html[pos..].find('>')?;
    if gt > pos && bytes[gt - 1] == b'/' {
        return Some(SpanBlock {
            inner: String::new(),
            end: gt + 1,
            attrs: html[pos + 5..gt - 1].to_string(),
        });
    }
    let mut depth = 1i32;
    let mut cursor = gt + 1;
    while let Some((at, closing)) = next_span_tag(html, cursor) {
        let tag_gt = at + html[at..].find('>')?;
        let self_closing = tag_gt > at && bytes[tag_gt - 1] == b'/';
        if !self_closing {
            depth += if closing { -1 } else { 1 };
            if depth == 0 {
                return Some(SpanBlock {
                    inner: html[gt + 1..at].to_string(),
                    end: tag_gt + 1,
                    attrs: html[pos + 5..gt].to_string(),
                });
            }
        }
        cursor = tag_gt + 1;
    }
    None
}

fn ttml_attr_begin(attrs: &str) -> Option<i64> {
    static BEGIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\bbegin="\s*([^"]*)\s*""#).expect("begin attr"));
    BEGIN.captures(attrs).and_then(|m| parse_ttml_time(&m[1]))
}

fn ttml_attr_end(attrs: &str) -> Option<i64> {
    static END: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\bend="\s*([^"]*)\s*""#).expect("end attr"));
    END.captures(attrs).and_then(|m| parse_ttml_time(&m[1]))
}

fn ttml_role(attrs: &str) -> Option<String> {
    static ROLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\bttm:role="([^"]*)""#).expect("role attr"));
    ROLE.captures(attrs).map(|m| m[1].to_string())
}

/// Parse AMLL-style TTML. Returns `None` when the text carries no timed `<p>`.
pub fn parse_ttml(raw: &str) -> Option<ParsedLyrics> {
    static P_BEGIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<p\b[^>]*\bbegin\s*=\s*""#).expect("p begin"));
    if !P_BEGIN.is_match(raw) {
        return None;
    }

    // Agent declaration order is the only singer signal TTML carries (`v1`,
    // `v2`, …); the renderer maps that order onto the artist list.
    static AGENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<ttm:agent\b[^>]*xml:id="([^"]+)""#).expect("agent"));
    let _agent_ids: Vec<String> = AGENT
        .captures_iter(raw)
        .map(|m| m[1].to_string())
        .collect();

    static P_BLOCK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<p\b([^>]*)>([\s\S]*?)</p>").expect("p block"));
    static AGENT_ATTR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\bttm:agent="([^"]*)""#).expect("agent attr"));
    static WS: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));

    let mut lines: Vec<ParsedLine> = Vec::new();
    for capture in P_BLOCK.captures_iter(raw) {
        let attrs = &capture[1];
        let Some(start) = ttml_attr_begin(attrs) else {
            continue;
        };
        let end = ttml_attr_end(attrs);
        let agent = AGENT_ATTR.captures(attrs).map(|m| m[1].to_string());
        let body = &capture[2];

        let mut words: Vec<Word> = Vec::new();
        let mut trans: Option<String> = None;
        let mut roma: Option<String> = None;
        let mut bg: Option<String> = None;
        let mut bg_words: Option<Vec<Word>> = None;
        let mut bg_trans: Option<String> = None;
        let mut text_buf = String::new();

        let mut pos = 0usize;
        while pos < body.len() {
            let Some(lt_rel) = body[pos..].find('<') else {
                text_buf.push_str(&xml_unescape(&body[pos..]));
                break;
            };
            let lt = pos + lt_rel;
            if lt > pos {
                text_buf.push_str(&xml_unescape(&body[pos..lt]));
            }
            if !body[lt..].starts_with("<span") {
                pos = match body[lt..].find('>') {
                    Some(gt_rel) => lt + gt_rel + 1,
                    None => body.len(),
                };
                continue;
            }
            let Some(block) = read_span_block(body, lt) else {
                break;
            };
            match ttml_role(&block.attrs).as_deref() {
                Some("x-translation") => {
                    trans = Some(WS.replace_all(&xml_unescape(&block.inner), " ").trim().to_string());
                }
                Some("x-roman") => {
                    let r = WS.replace_all(&strip_tags(&xml_unescape(&block.inner)), " ").trim().to_string();
                    if !r.is_empty() {
                        roma = Some(r);
                    }
                }
                Some("x-bg") => {
                    // The harmony block gets its own timeline; it stays attached
                    // to the main row rather than becoming a row of its own, so
                    // translation-list line alignment keeps working.
                    let mut bg_text = String::new();
                    let mut bws: Vec<Word> = Vec::new();
                    let mut btr: Option<String> = None;
                    let mut inner_pos = 0usize;
                    while inner_pos < block.inner.len() {
                        let Some(lt2_rel) = block.inner[inner_pos..].find('<') else {
                            bg_text.push_str(&xml_unescape(&block.inner[inner_pos..]));
                            break;
                        };
                        let lt2 = inner_pos + lt2_rel;
                        if lt2 > inner_pos {
                            bg_text.push_str(&xml_unescape(&block.inner[inner_pos..lt2]));
                        }
                        let Some(inner_block) = read_span_block(&block.inner, lt2) else {
                            inner_pos = lt2 + 1;
                            continue;
                        };
                        match ttml_role(&inner_block.attrs).as_deref() {
                            Some("x-translation") => {
                                btr = Some(
                                    WS.replace_all(&xml_unescape(&inner_block.inner), " ")
                                        .trim()
                                        .to_string(),
                                );
                            }
                            Some("x-roman") => {}
                            _ => {
                                let word_text = strip_tags(&xml_unescape(&inner_block.inner));
                                if let Some(word_start) = ttml_attr_begin(&inner_block.attrs) {
                                    bws.push(Word {
                                        start: Some(word_start),
                                        end: ttml_attr_end(&inner_block.attrs),
                                        text: word_text.clone(),
                                    });
                                }
                                bg_text.push_str(&word_text);
                            }
                        }
                        inner_pos = inner_block.end;
                    }
                    bg = Some(WS.replace_all(&bg_text, " ").trim().to_string());
                    if bws.len() > 1 {
                        bg_words = Some(bws);
                    }
                    bg_trans = btr;
                }
                _ => {
                    // A bare word span: without a `begin` it is plain text.
                    let word_text = strip_tags(&xml_unescape(&block.inner));
                    if let Some(word_start) = ttml_attr_begin(&block.attrs) {
                        words.push(Word {
                            start: Some(word_start),
                            end: ttml_attr_end(&block.attrs),
                            text: word_text.clone(),
                        });
                    }
                    text_buf.push_str(&word_text);
                }
            }
            pos = block.end;
        }

        let text = WS.replace_all(&text_buf, " ").trim().to_string();
        if is_placeholder(&text) && bg.is_none() {
            continue;
        }
        lines.push(ParsedLine {
            start: Some(start),
            end,
            text,
            words,
            trans,
            roma,
            singer: agent,
            bg: bg.map(serde_json::Value::String),
            bg_words,
            bg_trans,
            ..Default::default()
        });
    }

    if lines.is_empty() {
        return None;
    }
    lines.sort_by_key(|l| l.start.unwrap_or(i64::MIN));
    fill_gaps(&mut lines);

    let word_lines = lines.iter().filter(|l| l.words.len() > 1).count();
    for line in lines.iter_mut() {
        let line_end = line.end;
        for i in 0..line.words.len() {
            if line.words[i].end.is_none() {
                line.words[i].end = if i + 1 < line.words.len() {
                    line.words[i + 1].start
                } else {
                    line_end
                };
            }
        }
        if let Some(bg_words) = line.bg_words.as_mut() {
            for i in 0..bg_words.len() {
                if bg_words[i].end.is_none() {
                    bg_words[i].end = if i + 1 < bg_words.len() {
                        bg_words[i + 1].start
                    } else {
                        bg_words[i].start.map(|s| s + 4000)
                    };
                }
            }
        }
    }

    // Two or more agents in the body mean a duet; a single voice clears the
    // column so the renderer has one code path.
    let used_agents: std::collections::BTreeSet<String> =
        lines.iter().filter_map(|l| l.singer.clone()).collect();
    let duet = used_agents.len() >= 2;
    if !duet {
        for line in lines.iter_mut() {
            line.singer = None;
        }
    }

    let has_translation = lines.iter().any(|l| l.trans.is_some());
    let has_romanization = lines.iter().any(|l| l.roma.is_some());
    let has_background = lines.iter().any(|l| l.bg.is_some());

    Some(ParsedLyrics {
        format: "ttml".to_string(),
        timing: Some(if word_lines > 0 { "Word" } else { "Line" }.to_string()),
        lang: Some(String::new()),
        lines: dedupe(lines),
        has_word_level: word_lines > 0,
        has_translation,
        meta: Some(BTreeMap::new()),
        plain_only: false,
        has_romanization: has_romanization.then_some(true),
        has_background: Some(has_background),
        duet: Some(duet),
        credits: None,
    })
}

fn strip_tags(text: &str) -> String {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| re(r"<[^>]*>"));
    TAGS.replace_all(text, "").to_string()
}

/// Parse NetEase YRC word-timed lyrics (`[line_start,line_dur](word_start,word_dur,0)text`).
pub fn parse_yrc(raw: &str) -> Option<ParsedLyrics> {
    static LINE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^\[(\d+),(\d+)\](.*)$"));
    static WORD_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\((\d+),(\d+),\d+\)([^()]*)"));

    if raw.trim().is_empty() {
        return None;
    }

    let mut lines: Vec<ParsedLine> = Vec::new();
    let mut has_words = false;

    for raw_line in raw.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('{') {
            continue;
        }
        let Some(caps) = LINE_RE.captures(trimmed) else {
            continue;
        };
        let Ok(start) = caps[1].parse::<i64>() else {
            continue;
        };
        let dur = caps[2].parse::<i64>().unwrap_or(0).max(0);
        let end = (dur > 0).then_some(start + dur);
        let body = &caps[3];

        let mut words: Vec<Word> = Vec::new();
        let mut plain = String::new();
        for wcap in WORD_RE.captures_iter(body) {
            let Ok(ws) = wcap[1].parse::<i64>() else {
                continue;
            };
            let wd = wcap[2].parse::<i64>().unwrap_or(0).max(0);
            let text = &wcap[3];
            if text.is_empty() {
                continue;
            }
            plain.push_str(text);
            words.push(Word {
                start: Some(ws),
                end: (wd > 0).then_some(ws + wd),
                text: text.to_string(),
            });
        }
        if plain.trim().is_empty() {
            continue;
        }
        if !words.is_empty() {
            has_words = true;
        }
        lines.push(ParsedLine {
            start: Some(start),
            end,
            text: plain,
            words,
            trans: None,
            roma: None,
            singer: None,
            bg: None,
            bg_words: None,
            bg_trans: None,
        });
    }

    if lines.is_empty() {
        return None;
    }
    Some(ParsedLyrics {
        format: "yrc".to_string(),
        timing: Some(if has_words { "Word" } else { "Line" }.to_string()),
        lang: Some(String::new()),
        lines,
        has_word_level: has_words,
        has_translation: false,
        meta: Some(BTreeMap::new()),
        plain_only: false,
        has_romanization: None,
        has_background: None,
        duet: None,
        credits: None,
    })
}

/// Auto-detect the format and parse. Order matters: QRC's XML wrapper and its
/// text-first word grammar have to be tried before the KRC shape, and LRC is
/// only attempted once every `[start,dur]` shape has been ruled out.
pub fn parse_lyrics(raw: &str) -> Option<ParsedLyrics> {
    static TTML: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"<p\b[^>]*\bbegin\s*=\s*""#).expect("ttml regex"));
    static YRC_LINE: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?m)^\[\d+,\d+\]\(\d+,\d+,\d+\)"));
    static QRC_XML: LazyLock<Regex> = LazyLock::new(|| re(r#"LyricContent=""#));
    static QRC_LINE: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?m)^\[\d+,\d+\][^(<\n]*\(\d+,\d+\)"));
    static KRC_LINE: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\[\d+,\d+\]<"));

    if raw.trim().is_empty() {
        return None;
    }

    // TTML is checked first: its `<p begin="…">` signature cannot collide with
    // the QRC/KRC/LRC shapes. A malformed TTML document falls through to the
    // rest instead of becoming an empty chart.
    if TTML.is_match(raw) {
        if let Some(doc) = parse_ttml(raw) {
            return Some(doc);
        }
    }

    if YRC_LINE.is_match(raw) {
        if let Some(doc) = parse_yrc(raw) {
            return Some(doc);
        }
    }

    if QRC_XML.is_match(raw) {
        if let Some(doc) = parse_qrc(raw) {
            return Some(doc);
        }
    }
    if QRC_LINE.is_match(raw) {
        if let Some(doc) = parse_qrc(raw).or_else(|| parse_krc(raw)) {
            return Some(doc);
        }
    }
    if KRC_LINE.is_match(raw) {
        if let Some(doc) = parse_krc(raw).or_else(|| parse_qrc(raw)) {
            return Some(doc);
        }
    }
    if let Some(doc) = parse_lrc(raw) {
        return Some(doc);
    }
    Some(parse_plain(raw))
}

/// Parse an imported local lyric file according to its extension, with auto-detection fallback.
pub fn parse_imported_lyrics(raw: &str, ext: &str) -> Option<ParsedLyrics> {
    if raw.trim().is_empty() {
        return None;
    }
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "ttml" | "xml" => parse_ttml(raw).or_else(|| parse_lyrics(raw)),
        "qrc" => parse_qrc(raw).or_else(|| parse_lyrics(raw)),
        "yrc" => parse_yrc(raw).or_else(|| parse_lyrics(raw)),
        "lrc" => parse_lrc(raw).or_else(|| parse_lyrics(raw)),
        _ => parse_lyrics(raw),
    }
}

#[cfg(test)]
mod krc_word_duration_tests {
    use super::*;

    #[test]
    fn parse_krc_preserves_word_duration_instead_of_stretching_to_next_line() {
        let raw = "[67649,17321]<0,191,0>Now <191,193,0>I'm <384,200,0>on <584,232,0>the <816,1618,0>outside\n\
                   [84970,3007]<0,182,0>Though <182,234,0>you <416,249,0>give <665,247,0>me <912,440,0>no <1352,1655,0>reason\n";
        let parsed = parse_krc(raw).expect("krc");
        assert_eq!(parsed.lines.len(), 2);
        let first = &parsed.lines[0];
        let last_word = first.words.last().expect("outside word");
        assert_eq!(last_word.text, "outside");
        assert_eq!(last_word.start, Some(68_465));
        assert_eq!(last_word.end, Some(70_083));
        assert_eq!(first.end, Some(70_083));
        let gap = parsed.lines[1].start.unwrap() - first.end.unwrap();
        assert_eq!(gap, 14_887);
    }

    #[test]
    fn parse_krc_falls_back_to_close_words_when_word_duration_is_zero() {
        let raw = "[1000,2000]<0,0,0>Hello <500,0,0>world\n[3000,1000]<0,500,0>Next\n";
        let parsed = parse_krc(raw).expect("krc");
        assert_eq!(parsed.lines[0].words[0].end, Some(1_500));
        assert_eq!(parsed.lines[0].words[1].end, Some(3_000));
    }

    #[test]
    fn parse_ttml_separates_x_roman_without_polluting_lyric_text() {
        let raw = r#"<tt><body><div><p begin="00:01.000" end="00:03.000"><span begin="00:01.000" end="00:02.000">夢</span><span begin="00:02.000" end="00:03.000">ならば</span><span ttm:role="x-roman">yu me na ra ba</span><span ttm:role="x-translation">如果是梦</span></p></div></body></tt>"#;
        let parsed = parse_ttml(raw).expect("ttml");
        assert_eq!(parsed.lines.len(), 1);
        assert_eq!(parsed.lines[0].text, "夢ならば");
        assert_eq!(parsed.lines[0].roma.as_deref(), Some("yu me na ra ba"));
        assert_eq!(parsed.lines[0].trans.as_deref(), Some("如果是梦"));
        assert_eq!(parsed.has_romanization, Some(true));
    }
}
