//! Lyric document contracts and the `Doc -> AMLL line` projection.
//!
//! Every rule in [`doc_to_lines`] exists because a specific real chart broke
//! without it; the port is 1:1 with `src/render/doc-to-lines.ts` and is pinned
//! by the goldens under `native/tests/fixtures/oracle/doc-to-lines/`.
//!
//! The output shape is AMLL's, not ours: times are milliseconds and a line with
//! no word timings still becomes a single word spanning the whole line so the
//! renderer can highlight it line by line.

use serde::{Deserialize, Serialize};

/// One timed syllable/word inside a line.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Word {
    pub start: Option<i64>,
    pub end: Option<i64>,
    #[serde(deserialize_with = "null_to_empty")]
    pub text: String,
}

/// The caches are JSON produced by an older TypeScript layer, and a provider
/// that has nothing to say for a field writes `null` instead of omitting the
/// key: `rolling-in-the-deep` really does carry `"trans": null`. The reference
/// reads every one of these through `String(value ?? '')`, so a missing key, an
/// explicit null and a present string must all land on the same value. Without
/// this, one null translation in one chart fails the whole document.
fn null_to_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// `line.bg` is either a boolean flag or the harmony text itself.
///
/// The distinction matters: a truthy string emits a *second* line after the
/// lead, while `true` marks the row as a background row. Collapsing the two
/// swallows the lead vocal.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum BgField {
    /// `bg: true` — this row itself is a background row.
    Flag(bool),
    /// `bg: "harmony text"` — a nested harmony line follows this one.
    Text(String),
}

impl BgField {
    pub fn is_flag(&self) -> bool {
        matches!(self, Self::Flag(true))
    }

    pub fn harmony_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) if !text.trim().is_empty() => Some(text),
            _ => None,
        }
    }
}

/// One source row of a lyric document.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LyricLine {
    pub start: Option<i64>,
    pub end: Option<i64>,
    #[serde(deserialize_with = "null_to_empty")]
    pub text: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub trans: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub roma: String,
    pub words: Vec<Word>,
    pub bg: Option<BgField>,
    pub bg_words: Vec<Word>,
    pub bg_trans: Option<String>,
    pub singer: Option<String>,
}

/// A lyric document as the caches and providers hand it to the renderer.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LyricDoc {
    #[serde(deserialize_with = "null_to_empty")]
    pub title: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub artist: String,
    pub artists: Vec<String>,
    #[serde(deserialize_with = "null_to_empty")]
    pub album: String,
    pub duration_sec: f64,
    pub lines: Vec<LyricLine>,
    pub credits: Vec<String>,
    pub duet: bool,
    pub instrumental: bool,
}

/// A word as AMLL consumes it: times in milliseconds, text with its own spaces.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AmllWord {
    #[serde(rename = "startTime")]
    pub start_time: i64,
    #[serde(rename = "endTime")]
    pub end_time: i64,
    pub word: String,
}

/// An AMLL line. `src_index` points back at the source row so a click can seek
/// to the right timestamp even after the player rebuilds and merges lines.
///
/// Line times are doubles because the reference's are: `tryAdvanceStartTime`
/// takes 70% of a short overlap, and 70% of 205 ms of overlap is 143.5 ms, not
/// 143 or 144. A chart whose neighbours overlap by an odd number of milliseconds
/// therefore carries a fractional start, and every later stage — mask offsets,
/// focus, scrolling — reads that exact value. Rounding at the boundary would
/// bake a sub-millisecond error into every downstream comparison.
///
/// Word windows stay integers: providers emit integers and the reference never
/// writes a fractional value into one.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AmllLine {
    pub words: Vec<AmllWord>,
    #[serde(rename = "translatedLyric")]
    pub translated_lyric: String,
    #[serde(rename = "romanLyric")]
    pub roman_lyric: String,
    #[serde(rename = "startTime")]
    pub start_time: f64,
    #[serde(rename = "endTime")]
    pub end_time: f64,
    #[serde(rename = "isBG")]
    pub is_bg: bool,
    #[serde(rename = "isDuet")]
    pub is_duet: bool,
    #[serde(rename = "_srcIndex")]
    pub src_index: usize,
}

/// Which optional sub-lines the display profile asks for.
#[derive(Clone, Copy, Debug)]
pub struct LineDisplayOptions {
    pub show_translation: bool,
    pub show_romanization: bool,
}

impl Default for LineDisplayOptions {
    fn default() -> Self {
        Self {
            show_translation: true,
            show_romanization: false,
        }
    }
}

/// Whether this is a CJK code point. CJK breaks between characters, Latin does
/// not, and mixing the two rules moves every wrap point in a bilingual chart.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF
        | 0x2E80..=0x2FDF
        | 0x3000..=0x303F
        | 0x3040..=0x30FF
        | 0x3130..=0x318F
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFF)
}

fn ascii_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// UTF-16 code-unit length, which is what the reference JavaScript `.length`
/// counts. Byte or scalar counts would place spaces inside surrogate pairs.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn strip_whitespace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whether a document's word spans carry their own trailing spaces.
///
/// Submissions come in two camps: one embeds the space in the word span, the
/// other drops every trailing space. The first is trusted as-is; the second
/// needs spaces synthesised or "alpha line one" renders as "alphalineone".
/// Measured across real charts the two camps sit at 81% and 0%, so 0.2
/// separates them with a wide margin.
pub fn detect_space_convention(doc: &LyricDoc) -> bool {
    let mut with_space = 0usize;
    let mut without_space = 0usize;
    for line in &doc.lines {
        for word in &line.words {
            if !word.text.chars().any(ascii_alnum) {
                continue;
            }
            if word.text.chars().last().is_some_and(char::is_whitespace) {
                with_space += 1;
            } else {
                without_space += 1;
            }
        }
    }
    let total = with_space + without_space;
    total > 0 && (with_space as f64 / total as f64) < 0.2
}

/// Recover word gaps from the source line's own space positions.
///
/// Returns `None` when the text is missing or does not reconstruct from the
/// tokens, so the caller can fall back to the Latin-boundary heuristic. A
/// trailing gap is never emitted: the last token needs no space after it.
pub fn gaps_from_text(text: &str, tokens: &[String]) -> Option<Vec<bool>> {
    if text.is_empty() {
        return None;
    }
    let stripped_raw = strip_whitespace(text);
    let joined: String = tokens.iter().map(|t| strip_whitespace(t)).collect();
    if joined != stripped_raw {
        return None;
    }

    let mut cumulative = Vec::with_capacity(tokens.len());
    let mut acc = 0usize;
    for token in tokens {
        acc += utf16_len(&strip_whitespace(token));
        cumulative.push(acc);
    }

    let mut gaps = vec![false; tokens.len()];
    let mut seen = 0usize;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if let Some(index) = cumulative.iter().position(|&edge| edge == seen)
                && index + 1 < gaps.len()
            {
                gaps[index] = true;
            }
        } else {
            seen += ch.len_utf16();
        }
    }
    Some(gaps)
}

/// Latin-boundary fallback, used only for whole-word submissions that dropped
/// every space. Syllable-granular data goes through [`gaps_from_text`] instead:
/// splitting "always" into "al ways" is the failure this avoids.
pub fn gaps_latin(tokens: &[String]) -> Vec<bool> {
    (0..tokens.len())
        .map(|i| {
            let before = tokens[i].trim_end();
            let after = tokens.get(i + 1).map(|t| t.trim_start()).unwrap_or("");
            i + 1 < tokens.len()
                && !before.is_empty()
                && before.chars().last().is_some_and(ascii_alnum)
                && after.chars().next().is_some_and(ascii_alnum)
        })
        .collect()
}

fn words_of(
    text: &str,
    input: &[Word],
    start: i64,
    end: i64,
    synth_word_spaces: bool,
) -> Vec<AmllWord> {
    // No word timings, or a single one: emit the whole line as one word. AMLL
    // then highlights line by line, which is exactly what line-timed charts want.
    if input.len() <= 1 {
        return vec![AmllWord {
            start_time: start,
            end_time: end,
            word: text.to_string(),
        }];
    }

    let tokens: Vec<String> = input.iter().map(|w| w.text.clone()).collect();
    let gaps = gaps_from_text(text, &tokens)
        .or_else(|| if synth_word_spaces { Some(gaps_latin(&tokens)) } else { None });

    input
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let mut token = tokens[i].clone();
            if gaps.as_ref().is_some_and(|g| g[i]) && !token.ends_with(char::is_whitespace) {
                token.push(' ');
            }
            let word_start = word.start.unwrap_or(start);
            AmllWord {
                start_time: word_start,
                // A zero-length word never lights up: the player computes NaN
                // progress for it.
                end_time: word.end.unwrap_or(word_start + 1),
                word: token,
            }
        })
        .collect()
}

/// Project a document onto AMLL lines.
///
/// Harmony handling is the subtle part. AMLL wants a nested harmony as a
/// separate `isBG` line *following* the main line; marking the main line itself
/// as `isBG` swallows the lead vocal. A row whose text is empty but which has
/// harmony content emits the harmony alone, and that lone line must not be
/// flagged `isBG` or the whole row drops into the background layer.
pub fn doc_to_lines(doc: &LyricDoc, display: LineDisplayOptions) -> Vec<AmllLine> {
    let synth_word_spaces = detect_space_convention(doc);

    // Duet sides follow first-appearance order: the first singer is "a", every
    // other singer "b".
    let mut seen_singers: Vec<&str> = Vec::new();

    let mut out = Vec::new();
    for (index, line) in doc.lines.iter().enumerate() {
        let mut side_is_b = false;
        if let Some(singer) = line.singer.as_deref() {
            let at = match seen_singers.iter().position(|s| *s == singer) {
                Some(at) => at,
                None => {
                    seen_singers.push(singer);
                    seen_singers.len() - 1
                }
            };
            side_is_b = at != 0;
        }

        let start = line.start.unwrap_or(0);
        // A document may leave `end` out or zero it; AMLL needs a finite interval.
        let end = match line.end {
            Some(end) if end > start => end,
            _ => start + 4000,
        };

        let translated = if display.show_translation {
            line.trans.clone()
        } else {
            String::new()
        };
        let roman = if display.show_romanization {
            line.roma.clone()
        } else {
            String::new()
        };

        let main = AmllLine {
            words: words_of(&line.text, &line.words, start, end, synth_word_spaces),
            translated_lyric: translated,
            roman_lyric: roman,
            start_time: start as f64,
            end_time: end as f64,
            is_bg: line.bg.as_ref().is_some_and(BgField::is_flag),
            is_duet: side_is_b,
            src_index: index,
        };

        let Some(harmony_text) = line.bg.as_ref().and_then(BgField::harmony_text) else {
            out.push(main);
            continue;
        };

        let harmony_start = line
            .bg_words
            .first()
            .and_then(|w| w.start)
            .unwrap_or(start);
        let mut harmony_end = line.bg_words.last().and_then(|w| w.end).unwrap_or(end);
        if harmony_end <= harmony_start {
            harmony_end = end;
        }

        let mut harmony = AmllLine {
            words: words_of(
                harmony_text,
                &line.bg_words,
                harmony_start,
                harmony_end,
                synth_word_spaces,
            ),
            translated_lyric: if display.show_translation {
                line.bg_trans.clone().unwrap_or_default()
            } else {
                String::new()
            },
            roman_lyric: String::new(),
            start_time: harmony_start as f64,
            end_time: harmony_end as f64,
            is_bg: true,
            is_duet: side_is_b,
            src_index: index,
        };

        // Both lines go out: the lead vocal first, then the harmony right after
        // it. Marking the main line itself `isBG` instead would swallow the lead.
        if line.text.trim().is_empty() {
            harmony.is_bg = false;
            out.push(harmony);
        } else {
            out.push(main);
            out.push(harmony);
        }
    }
    out
}

/// Credit line shown at the end of the timeline, intelligently deduplicated
/// across punctuation/case variants, abbreviated vs full legal names, and
/// well-known solo stage-name/real-name pairs (keeping the fuller name).
pub fn credit_names(doc: &LyricDoc) -> String {
    let mut names: Vec<String> = Vec::new();
    for raw in &doc.credits {
        push_credit_entry(&mut names, raw, false);
    }
    for raw in &doc.artists {
        push_credit_entry(&mut names, raw, true);
    }
    if names.is_empty() {
        String::new()
    } else {
        // The label and separator are part of the frozen golden, not a
        // translatable string: the reference builds them in code.
        format!("创作者：{}", names.join("、"))
    }
}

fn push_credit_entry(names: &mut Vec<String>, raw: &str, from_artist: bool) {
    for part in split_credit_parts(raw, from_artist) {
        let cleaned = strip_credit_role_prefix(&part);
        let (outer, inner_alias) = split_parenthetical_alias(cleaned);
        let outer = outer.trim();
        if outer.chars().count() < 2 {
            continue;
        }
        if let Some(alias) = inner_alias.as_deref().map(str::trim).filter(|a| a.chars().count() >= 2) {
            let chosen = if is_more_complete_name(alias, outer) { alias } else { outer };
            upsert_creator(names, chosen, Some(&[outer, alias]));
        } else {
            upsert_creator(names, outer, None);
        }
    }
}

fn upsert_creator(names: &mut Vec<String>, candidate: &str, aliases: Option<&[&str]>) {
    let matches_existing = |existing: &str| {
        creators_match(existing, candidate)
            || aliases.is_some_and(|list| list.iter().any(|a| creators_match(existing, a)))
    };
    if let Some(pos) = names.iter().position(|prev| matches_existing(prev)) {
        if is_more_complete_name(candidate, &names[pos]) {
            names[pos] = candidate.to_string();
        }
        return;
    }
    names.push(candidate.to_string());
}

fn split_credit_parts(raw: &str, from_artist: bool) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let flat = normalize_creator_flat(trimmed);
    if from_artist
        && matches!(
            flat.as_str(),
            "tyler the creator" | "earth wind and fire" | "earth wind fire" | "simon garfunkel" | "hall oates" | "florence the machine"
        )
    {
        return vec![trimmed.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let chars: Vec<char> = trimmed.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' | '（' | '[' | '【' => {
                depth += 1;
                cur.push(c);
                i += 1;
            }
            ')' | '）' | ']' | '】' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
                i += 1;
            }
            _ if depth == 0 && matches!(c, '、' | '/' | '／' | ';' | '；' | '，') => {
                let s = cur.trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
                cur.clear();
                i += 1;
            }
            _ if depth == 0 && from_artist && matches!(c, ',' | '&' | '＋' | '+') => {
                let s = cur.trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
                cur.clear();
                i += 1;
            }
            _ => {
                cur.push(c);
                i += 1;
            }
        }
    }
    let tail = cur.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    if !from_artist {
        return out;
    }
    let mut expanded = Vec::new();
    for item in out {
        let lower = item.to_lowercase();
        let mut split_idx = None;
        for marker in [" feat. ", " feat ", " ft. ", " ft ", " featuring "] {
            if let Some(pos) = lower.find(marker) {
                split_idx = Some((pos, marker.len()));
                break;
            }
        }
        if let Some((pos, mlen)) = split_idx {
            let head = item[..pos].trim();
            let rest = item[pos + mlen..].trim();
            if !head.is_empty() {
                expanded.push(head.to_string());
            }
            if !rest.is_empty() {
                expanded.extend(split_credit_parts(rest, true));
            }
        } else {
            expanded.push(item);
        }
    }
    expanded
}

fn strip_credit_role_prefix(s: &str) -> &str {
    let trimmed = s.trim();
    let Some(colon) = trimmed.find([':', '：']) else {
        return trimmed;
    };
    let head = trimmed[..colon].trim().to_lowercase();
    let is_role = matches!(
        head.as_str(),
        "词" | "曲" | "编" | "唱" | "作词" | "作詞" | "作曲" | "填词" | "填詞"
            | "编曲" | "編曲" | "改编" | "改編" | "制作" | "製作" | "制作人" | "製作人"
            | "和声" | "和聲" | "歌词" | "歌詞"
            | "lyrics by" | "written by" | "composed by" | "music by"
            | "arranged by" | "produced by" | "lyricist" | "composer" | "writer" | "songwriter"
    );
    if is_role {
        trimmed[colon + trimmed[colon..].chars().next().map_or(1, char::len_utf8)..].trim()
    } else {
        trimmed
    }
}

fn split_parenthetical_alias(s: &str) -> (&str, Option<String>) {
    let trimmed = s.trim();
    for (open, close) in [('(', ')'), ('（', '）')] {
        if trimmed.ends_with(close) {
            if let Some(idx) = trimmed.rfind(open) {
                let outer = trimmed[..idx].trim();
                let inner = trimmed[idx + open.len_utf8()..trimmed.len() - close.len_utf8()].trim();
                if !outer.is_empty() && !inner.is_empty() {
                    let inner_lower = inner.to_lowercase();
                    let role_only = matches!(
                        inner_lower.as_str(),
                        "vocals" | "vocal" | "backing vocals" | "lead vocals" | "piano" | "guitar"
                            | "bass" | "drums" | "synth" | "synthesizer" | "producer" | "writer"
                            | "composer" | "lyricist" | "arranger" | "作词" | "作曲" | "编曲" | "演唱" | "和声"
                    );
                    if role_only {
                        return (outer, None);
                    }
                    return (outer, Some(inner.to_string()));
                }
            }
        }
    }
    (trimmed, None)
}

fn fold_latin_diacritic(c: char) -> &'static str {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => "a",
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => "e",
        'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => "i",
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' | 'Ø' => "o",
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => "u",
        'ý' | 'ÿ' | 'Ý' | 'Ÿ' => "y",
        'ñ' | 'Ñ' => "n",
        'ç' | 'Ç' => "c",
        'æ' | 'Æ' => "ae",
        'œ' | 'Œ' => "oe",
        'ß' => "ss",
        _ => "",
    }
}

fn creator_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for raw in s.chars() {
        let c = match raw {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(raw as u32 - 0xFEE0).unwrap_or(raw),
            '\u{3000}' => ' ',
            _ => raw,
        };
        if matches!(c, '\'' | '’' | '‘' | 'ʼ' | '`') {
            continue;
        }
        let folded = fold_latin_diacritic(c);
        if !folded.is_empty() {
            cur.push_str(folded);
            continue;
        }
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                cur.push(lc);
            }
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn normalize_creator_flat(s: &str) -> String {
    creator_tokens(s).join(" ")
}

const CREATOR_ALIAS_GROUPS: &[&[&str]] = &[
    &["birdy", "jasmine van den bogaerde", "jasmine lucilla elizabeth jennifer van den bogaerde"],
    &["the weeknd", "abel tesfaye", "abel makkonen tesfaye"],
    &["billie eilish", "billie eilish oconnell", "billie eilish baird oconnell", "billie eilish pirate baird oconnell"],
    &["finneas", "finneas oconnell", "finneas baird oconnell"],
    &["lady gaga", "stefani germanotta", "stefani joanne angelina germanotta"],
    &["lorde", "ella yelich oconnor", "ella marija lani yelich oconnor"],
    &["bruno mars", "peter gene hernandez"],
    &["lana del rey", "elizabeth woolridge grant"],
    &["sia", "sia furler", "sia kate isobelle furler"],
    &["halsey", "ashley nicolette frangipane"],
    &["chappell roan", "kayleigh rose amstutz"],
    &["bad bunny", "benito antonio martinez ocasio"],
    &["sza", "solana imani rowe"],
    &["drake", "aubrey drake graham"],
    &["post malone", "austin richard post"],
    &["katy perry", "katheryn elizabeth hudson"],
    &["nicki minaj", "onika tanya maraj"],
    &["cardi b", "belcalis marlenis almanzar"],
    &["doja cat", "amala ratna zandile dlamini"],
    &["travis scott", "jacques bermon webster", "jacques bermon webster ii"],
    &["eminem", "marshall bruce mathers", "marshall bruce mathers iii"],
    &["calvin harris", "adam richard wiles"],
    &["david guetta", "pierre david guetta"],
    &["avicii", "tim bergling"],
    &["zedd", "anton zaslavski"],
    &["miley cyrus", "destiny hope cyrus"],
    &["pnk", "pink", "alecia beth moore"],
    &["macklemore", "ben haggerty", "benjamin hammond haggerty"],
    &["wiz khalifa", "cameron jibril thomaz"],
    &["kid cudi", "scott ramon seguro mescudi"],
    &["frank ocean", "christopher edwin breaux"],
    &["childish gambino", "donald glover", "donald mckinley glover"],
    &["tyler the creator", "tyler gregory okonma"],
    &["aap rocky", "asap rocky", "rakim mayers"],
    &["kendrick lamar", "kendrick lamar duckworth"],
    &["j cole", "jermaine lamarr cole"],
    &["21 savage", "sheyaa bin abraham joseph"],
    &["lil nas x", "montero lamar hill"],
    &["troye sivan", "troye sivan mellet"],
    &["lauv", "ari staprans leff"],
    &["bazzi", "andrew bazzi"],
    &["jvke", "jacob lawson"],
    &["d4vd", "david anthony burke"],
    &["kesha", "kesha rose sebert"],
    &["lizzo", "melissa viviane jefferson"],
    &["charli xcx", "charlotte emma aitchison"],
    &["freddie mercury", "farrokh bulsara"],
    &["elton john", "reginald kenneth dwight"],
    &["bob dylan", "robert allen zimmerman"],
    &["david bowie", "david robert jones"],
    &["sting", "gordon matthew thomas sumner"],
    &["bono", "paul david hewson"],
    &["jay z", "shawn corey carter"],
    &["kanye west", "ye", "kanye omari west"],
    &["pharrell williams", "pharrell lanscilo williams"],
    &["deadmau5", "joel thomas zimmerman"],
    &["skrillex", "sonny john moore"],
    &["marshmello", "christopher comstock"],
    &["diplo", "thomas wesley pentz"],
    &["kygo", "kyrre gorvell dahll"],
    &["martin garrix", "martijn gerard garritsen"],
    &["nf", "nathan john feuerstein"],
    &["ed sheeran", "edward christopher sheeran"],
    &["sam smith", "samuel frederick smith"],
    &["shawn mendes", "shawn peter raul mendes"],
    &["charlie puth", "charles otto puth", "charles otto puth jr"],
    &["olivia rodrigo", "olivia isabel rodrigo"],
    &["sabrina carpenter", "sabrina annlynn carpenter"],
    &["taylor swift", "taylor alison swift"],
    &["ariana grande", "ariana grande butera"],
    &["justin bieber", "justin drew bieber"],
    &["harry styles", "harry edward styles"],
    &["zayn", "zain javadd malik"],
    &["rihanna", "robyn rihanna fenty"],
    &["adele", "adele adkins", "adele laurie blue adkins"],
    &["beyonce", "beyonce knowles", "beyonce giselle knowles", "beyonce giselle knowles carter"],
    &["shakira", "shakira mebarak", "shakira isabel mebarak ripoll"],
    &["gem", "邓紫棋", "gloria tang", "邓诗颖"],
    &["周杰伦", "jay chou"],
    &["林俊杰", "jj lin"],
    &["陈奕迅", "eason chan"],
    &["王菲", "faye wong"],
    &["孙燕姿", "yanzi sun", "stefanie sun"],
    &["蔡依林", "jolin tsai"],
    &["张学友", "jacky cheung"],
    &["刘德华", "andy lau"],
    &["邓丽君", "teresa teng"],
    &["米津玄师", "kenshi yonezu", "hachi"],
    &["藤井風", "藤井风", "fujii kaze"],
    &["ikura", "lilas ikuta", "幾田りら"],
    &["iu", "lee ji eun", "이지은"],
    &["g dragon", "kwon ji yong"],
    &["rose", "roseanne park", "park chae young"],
    &["jennie", "jennie kim"],
    &["lisa", "lalisa manoban"],
    &["jisoo", "kim ji soo"],
];

fn alias_group_id(flat: &str) -> Option<usize> {
    CREATOR_ALIAS_GROUPS
        .iter()
        .position(|group| group.iter().any(|entry| *entry == flat))
}

fn is_token_subsequence(short: &[String], long: &[String]) -> bool {
    if short.is_empty() || short.len() > long.len() {
        return false;
    }
    let mut j = 0usize;
    for tok in short {
        let Some(offset) = long[j..].iter().position(|x| x == tok) else {
            return false;
        };
        j += offset + 1;
    }
    true
}

fn creators_match(a: &str, b: &str) -> bool {
    let ta = creator_tokens(a);
    let tb = creator_tokens(b);
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let fa = ta.join(" ");
    let fb = tb.join(" ");
    if fa == fb {
        return true;
    }
    if let (Some(ga), Some(gb)) = (alias_group_id(&fa), alias_group_id(&fb)) {
        if ga == gb {
            return true;
        }
    }
    let (short, long) = if ta.len() <= tb.len() { (&ta, &tb) } else { (&tb, &ta) };
    if short.len() >= 2 && is_token_subsequence(short, long) {
        return true;
    }
    if short.len() == 1 && short[0].chars().count() >= 5 && long.iter().any(|t| t == &short[0]) {
        return true;
    }
    false
}

fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk)
}

fn is_more_complete_name(candidate: &str, existing: &str) -> bool {
    // Keep CJK / bilingual primary credit unless both share script or candidate is an explicit Latin full-name expansion.
    if has_cjk(existing) && !has_cjk(candidate) {
        return false;
    }
    if !has_cjk(existing) && has_cjk(candidate) {
        return false;
    }
    let tc = creator_tokens(candidate);
    let te = creator_tokens(existing);
    if tc.len() != te.len() {
        return tc.len() > te.len();
    }
    let cc = candidate.chars().filter(|c| !c.is_whitespace()).count();
    let ce = existing.chars().filter(|c| !c.is_whitespace()).count();
    cc > ce
}

/// Whether the whole line is CJK, which decides whether the renderer may break
/// between characters instead of only at spaces.
pub fn line_breaks_between_chars(text: &str) -> bool {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk > 0 && other == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: i64, end: i64, text: &str, words: Vec<Word>) -> LyricLine {
        LyricLine {
            start: Some(start),
            end: Some(end),
            text: text.to_string(),
            words,
            ..Default::default()
        }
    }

    fn word(start: i64, end: i64, text: &str) -> Word {
        Word {
            start: Some(start),
            end: Some(end),
            text: text.to_string(),
        }
    }

    #[test]
    fn synthesises_spaces_only_when_the_document_dropped_them() {
        let dropped = LyricDoc {
            lines: vec![line(
                0,
                2000,
                "alpha line one",
                vec![word(0, 500, "alpha"), word(500, 1000, "line"), word(1000, 2000, "one")],
            )],
            ..Default::default()
        };
        assert!(detect_space_convention(&dropped));
        let lines = doc_to_lines(&dropped, LineDisplayOptions::default());
        let words: Vec<&str> = lines[0].words.iter().map(|w| w.word.as_str()).collect();
        assert_eq!(words, ["alpha ", "line ", "one"]);

        let native = LyricDoc {
            lines: vec![line(
                0,
                2000,
                "alpha line one",
                vec![word(0, 500, "alpha "), word(500, 1000, "line "), word(1000, 2000, "one")],
            )],
            ..Default::default()
        };
        assert!(!detect_space_convention(&native));
        let lines = doc_to_lines(&native, LineDisplayOptions::default());
        let words: Vec<&str> = lines[0].words.iter().map(|w| w.word.as_str()).collect();
        assert_eq!(words, ["alpha ", "line ", "one"]);
    }

    #[test]
    fn line_timing_collapses_to_one_word() {
        let doc = LyricDoc {
            lines: vec![line(1000, 3000, "solo", vec![word(1000, 3000, "solo")])],
            ..Default::default()
        };
        let lines = doc_to_lines(&doc, LineDisplayOptions::default());
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].word, "solo");
        assert_eq!(lines[0].words[0].end_time, 3000);
    }

    #[test]
    fn missing_line_end_becomes_four_seconds() {
        let doc = LyricDoc {
            lines: vec![LyricLine {
                start: Some(500),
                end: Some(0),
                text: "x".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let lines = doc_to_lines(&doc, LineDisplayOptions::default());
        assert_eq!(lines[0].end_time, 4500.0);
    }

    #[test]
    fn zero_length_word_keeps_its_own_end() {
        // The reference substitutes `start + 1` only when `end` is *missing*:
        // `numOr(word.end, wordStart + 1)`. A word that explicitly ends where
        // it starts keeps end == start, and the golden
        // `doc-to-lines/synthetic-zero-length-word.json` records exactly that
        // (1000, not 1001). The synthetic case's note describes the intent, the
        // code and the frozen output are the contract.
        let doc = LyricDoc {
            lines: vec![line(
                1000,
                2000,
                "ab",
                vec![word(1000, 1000, "a"), word(1000, 2000, "b")],
            )],
            ..Default::default()
        };
        let lines = doc_to_lines(&doc, LineDisplayOptions::default());
        assert_eq!(lines[0].words[0].end_time, 1000);
    }

    #[test]
    fn a_word_without_an_end_gets_start_plus_one() {
        // Two words on purpose: a single word entry collapses to line timing
        // (`input.length <= 1`) and its own `end` is never consulted, so the
        // missing-end path only exists on multi-word lines.
        let doc = LyricDoc {
            lines: vec![line(
                1000,
                2000,
                "ab",
                vec![
                    Word { start: Some(1000), end: None, text: "a".into() },
                    word(1000, 2000, "b"),
                ],
            )],
            ..Default::default()
        };
        let lines = doc_to_lines(&doc, LineDisplayOptions::default());
        assert_eq!(lines[0].words[0].end_time, 1001);
    }

    #[test]
    fn harmony_emits_a_second_line_after_the_lead() {
        let doc = LyricDoc {
            lines: vec![LyricLine {
                start: Some(1000),
                end: Some(3000),
                text: "lead words".into(),
                words: vec![word(1000, 2000, "lead "), word(2000, 3000, "words")],
                bg: Some(BgField::Text("harmony words".into())),
                bg_words: vec![word(1000, 2000, "harmony "), word(2000, 3000, "words")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let lines = doc_to_lines(&doc, LineDisplayOptions::default());
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].is_bg);
        assert!(lines[1].is_bg);
        assert_eq!(lines[0].src_index, 0);
        assert_eq!(lines[1].src_index, 0);
    }

    #[test]
    fn harmony_alone_is_not_background() {
        let doc = LyricDoc {
            lines: vec![LyricLine {
                start: Some(1000),
                end: Some(3000),
                text: String::new(),
                bg: Some(BgField::Text("harmony words".into())),
                bg_words: vec![word(1000, 2000, "harmony "), word(2000, 3000, "words")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let lines = doc_to_lines(&doc, LineDisplayOptions::default());
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].is_bg);
        assert_eq!(lines[0].words.len(), 2);
    }

    #[test]
    fn duet_sides_follow_first_appearance() {
        let doc = LyricDoc {
            duet: true,
            lines: vec![
                LyricLine { singer: Some("A".into()), ..line(0, 1000, "one", vec![]) },
                LyricLine { singer: Some("B".into()), ..line(1000, 2000, "two", vec![]) },
                LyricLine { singer: Some("A".into()), ..line(2000, 3000, "three", vec![]) },
                LyricLine { singer: Some("C".into()), ..line(3000, 4000, "four", vec![]) },
            ],
            ..Default::default()
        };
        let lines = doc_to_lines(&doc, LineDisplayOptions::default());
        let sides: Vec<bool> = lines.iter().map(|l| l.is_duet).collect();
        assert_eq!(sides, [false, true, false, true]);
    }

    #[test]
    fn credits_deduplicate_case_insensitively() {
        let doc = LyricDoc {
            credits: vec!["Rick Nowels".into(), "rick nowels".into(), "X".into()],
            artists: vec!["Abel Tesfaye".into(), "Rick Nowels".into()],
            ..Default::default()
        };
        assert_eq!(credit_names(&doc), "创作者：Rick Nowels、Abel Tesfaye");
    }

    #[test]
    fn credits_deduplicate_aliases_full_names_and_variants() {
        let growing_pains = LyricDoc {
            credits: vec![
                "Sam Romans".into(),
                "Jasmine Van Den Bogaerde".into(),
            ],
            artists: vec!["Birdy".into()],
            ..Default::default()
        };
        assert_eq!(
            credit_names(&growing_pains),
            "创作者：Sam Romans、Jasmine Van Den Bogaerde"
        );

        let birds = LyricDoc {
            credits: vec![
                "Billie Eilish O’Connell".into(),
                "Finneas O'Connell".into(),
            ],
            artists: vec!["Billie Eilish".into()],
            ..Default::default()
        };
        assert_eq!(
            credit_names(&birds),
            "创作者：Billie Eilish O’Connell、Finneas O'Connell"
        );

        let upgrade_to_fuller = LyricDoc {
            credits: vec![
                "Billie Eilish".into(),
                "FINNEAS".into(),
                "Billie Eilish O'Connell".into(),
                "Finneas Baird O'Connell".into(),
            ],
            artists: vec!["Billie Eilish".into()],
            ..Default::default()
        };
        assert_eq!(
            credit_names(&upgrade_to_fuller),
            "创作者：Billie Eilish O'Connell、Finneas Baird O'Connell"
        );

        let paren_and_role = LyricDoc {
            credits: vec![
                "作词：Jasmine Van Den Bogaerde (Birdy)".into(),
                "Sam Romans (Piano)".into(),
            ],
            artists: vec!["Birdy".into()],
            ..Default::default()
        };
        assert_eq!(
            credit_names(&paren_and_role),
            "创作者：Jasmine Van Den Bogaerde、Sam Romans"
        );
    }

    #[test]
    fn gaps_ignore_interior_spaces_and_never_emit_a_trailing_one() {
        // The reference rejects a space whose position is not a token boundary
        // (`cumulative.indexOf(seen) < 0`), so the space inside "al ways" is
        // skipped and only the real boundary after "always" produces a gap:
        // the frozen reference returns [true, false] here, not null.
        let tokens = vec!["always".to_string(), "here".to_string()];
        assert_eq!(
            gaps_from_text("al ways here", &tokens),
            Some(vec![true, false])
        );
        // A trailing space is never emitted: the last token needs nothing after it.
        assert_eq!(
            gaps_from_text("alpha ", &["alpha".to_string()]),
            Some(vec![false])
        );
    }
}
