//! Document assembly: `buildDoc`, `stripMetaLines`, field attachment, `compose`.
//!
//! Ported 1:1 from the builder half of `src/engine/index.ts`. The order of the
//! steps is load-bearing, not stylistic:
//!
//! 1. parse the raw chart,
//! 2. paste row-indexed translations (KRC's `translationList`) **before**
//!    `stripMetaLines` removes the leading credit rows — otherwise the arrays no
//!    longer line up and the whole translation is dropped,
//! 3. strip metadata rows,
//! 4. detect the instrumental placeholder,
//! 5. paste independent translation/romanisation LRCs by time,
//! 6. drop rows without a start time (a plain-text chart must not become a
//!    zero-row `ok: true` document).
//!
//! Goldens: `native/tests/fixtures/oracle/engine/{build-doc,attach,compose}.json`.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::identity::{artist_ok, title_ok};
use crate::parsers::{is_placeholder, parse_lrc, parse_lyrics};
use crate::types::{AlignStats, BuiltDoc, Candidate, DocLine, ParsedLine, ParsedLyrics, Track};

/// The line shown for an instrumental track.
pub const INSTRUMENTAL_TEXT: &str = "纯音乐，请欣赏";

static META_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(作词|作詞|作曲|編曲|编曲|填詞|填词|改編|改编|監製|监制|製作人|制作人|母帶|母带|錄製|录制|弦樂|弦乐|(?:OP|SP)\s*[:：]|Written\s+by|Lyrics\s+by|Composed\s+by|Produced\s+by|Arranged\s+by|Mixed\s+by|Mastered\s+by|Recorded\s+by)",
    )
    .expect("meta line")
});

static META_SHORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(词|詞|曲|唱|编|編|奏|鼓|琴|吉他|貝斯|贝斯|鍵盤|键盘|混音|录音|錄音|制作|製作|出品|发行|發行|策划|策劃|统筹|統籌|和声|和聲|合声|合聲|和音|合音|伴唱|配唱|监制|監製|母带|母帶)(助理|编写|編寫|工程|处理|處理|师|師|指导|指導|团队|團隊|室|棚)?[:：]",
    )
    .expect("meta short")
});

static META_COPYRIGHT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(©|ⓒ|\bcopyright\b|all\s+rights\s+reserved|used\s+by\s+permission)")
        .expect("copyright")
});

static META_LIST_BY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9][A-Za-z0-9 ,&/·、（）()''’.;；-]{0,80}\b(?:by|for|at)\s*[：:]")
        .expect("list by")
});

static META_COURTESY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bappears\s+courtesy\s+of\b").expect("courtesy"));

static META_ROLE_EN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(synthesizers?|synths?|mellotrons?|programming|engineer(?:s|ed|ing)?|mix(?:ed|ing)|master(?:ed|ing)|record(?:ed|ing)|arrang(?:ed|ement)s?|produc(?:ers?|ed|tion)|strings|drums|percussion|guitars?|keyboards?|pianos?|violins?|cellos?|trumpets?|saxophones?|flutes?|horns?|mellotron|harmonium|vocal|vocals|backing|choir|conductor|orchestr(?:a|al)|courtesy|publishing|management|executive)\b",
    )
    .expect("role en")
});

static META_ROLE_LABEL_WORDS: &str = "assistant assistants additional associate executive co lead backing background acoustic electric electronic upright grand pedal steel slide solo rhythm vocal vocals voice memo memos tubular bell bells bass drum drums percussion guitar guitars keyboard keyboards piano pianos synthesizer synthesizers synth synths mellotron mellotrons violin violins cello cellos trumpet trumpets saxophone saxophones flute flutes horn horns harmonium strings choir conductor orchestra orchestral arrangement arrangements arranged arranging programming engineer engineers engineered engineering mix mixed mixing master mastered mastering record recorded recording producer producers produced production and by for at";

static CREDITS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(作词|作詞|作曲|填詞|填词|编曲|編曲|改編|改编|Lyrics\s+by|Composed\s+by|Written\s+by|Music\s+by)")
        .expect("credits")
});

static ROLE_LABEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([^:：]+)[:：]\s*\S").expect("role label"));

static ENGLISH_CREDIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Za-z][A-Za-z0-9\s/&,+().-]{0,100})[:：]\s*\S").expect("english credit")
});

static INSTRUMENTAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)纯音乐|纯音樂|没有填词|沒有填詞|无歌词|無歌詞|请欣赏|請欣賞|instrumental|no\s+lyrics")
        .expect("instrumental")
});

fn is_english_credit_label(text: &str) -> bool {
    let Some(m) = ENGLISH_CREDIT_RE.captures(text) else {
        return false;
    };
    let label = m[1].trim();
    if !META_ROLE_EN_RE.is_match(label)
        && !Regex::new(r"(?i)\b(?:voice\s+memos?|bass|tubular\s+bells)\b")
            .expect("role extra")
            .is_match(label)
    {
        return false;
    }
    label
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .all(|w| META_ROLE_LABEL_WORDS.split(' ').any(|k| k == w))
}

fn collect_credits(pool: &mut Vec<String>, seg: &str) {
    static SPLIT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\s*(?:&|、|,|，|/|／|\+|＋|;|；)\s*|\s+and\s+").expect("credit split")
    });
    static INLINE_ROLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:作词|作詞|作曲|填词|填詞|编曲|編曲|改編|改编|词|詞|曲|Lyrics\s+by|Composed\s+by|Written\s+by|Music\s+by)\s*[:：]\s*")
            .expect("inline credit role")
    });
    static TRIM_HEAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\s·・—–-]+|[\s·・]+$").expect("trim head"));
    static TRIM_TAIL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[。．.;；！!？?]+$").expect("trim tail"));

    for raw in SPLIT.split(seg) {
        let n = INLINE_ROLE.replace(raw.trim(), "");
        let n = TRIM_HEAD.replace_all(&n, "");
        let n = TRIM_TAIL.replace_all(&n, "");
        let n = n.trim();
        let len = n.chars().count();
        if (2..=40).contains(&len) && !pool.iter().any(|p| p.to_lowercase() == n.to_lowercase()) {
            pool.push(n.to_string());
        }
    }
}

/// Remove the leading metadata block and the duet prefixes (`女：`).
///
/// The block is the run of rows from index 0 that carry *any* signal; the first
/// zero-signal row is the first lyric and stops the scan. Weak signals (the
/// English role vocabulary) only fold when at least two of them cluster inside
/// that block — a single line mentioning a guitar must not delete "Guitar Man".
/// The density signal folds a group of rows sharing a timestamp within 50 ms,
/// which is how QQ ships its credits.
pub fn strip_meta_lines(doc: &mut ParsedLyrics, cand_title: &str, cand_artist: &str) {
    if doc.lines.is_empty() {
        return;
    }
    let title = cand_title.to_lowercase();
    let artist = cand_artist.to_lowercase();
    let mut credits: Vec<String> = Vec::new();
    if let Some(existing) = doc.credits.clone() {
        for name in existing {
            collect_credits(&mut credits, &name);
        }
    }

    struct Signal {
        blank: bool,
        strong: bool,
        weak: bool,
        is_credits: bool,
        text: String,
    }

    let signals: Vec<Signal> = doc
        .lines
        .iter()
        .map(|l| {
            let t = l.text.trim().to_string();
            if t.is_empty() {
                return Signal {
                    blank: true,
                    strong: false,
                    weak: false,
                    is_credits: false,
                    text: t,
                };
            }
            let is_credits = CREDITS_RE.is_match(&t);
            let lower = t.to_lowercase();
            let self_intro = !title.is_empty()
                && t.chars().count() < 90
                && (t.contains(" - ") || t.contains(" – ") || t.contains(" — "))
                && (lower.starts_with(&title)
                    || (!artist.is_empty() && (lower.contains(&title) || lower.contains(&artist))));
            let strong = is_credits
                || self_intro
                || META_LINE_RE.is_match(&t)
                || META_SHORT_RE.is_match(&t)
                || META_COPYRIGHT_RE.is_match(&t)
                || META_LIST_BY_RE.is_match(&t)
                || META_COURTESY_RE.is_match(&t)
                || is_english_credit_label(&t);
            let weak = !strong
                && ROLE_LABEL_RE
                    .captures(&t)
                    .is_some_and(|m| META_ROLE_EN_RE.is_match(&m[1]));
            Signal {
                text: t,
                blank: false,
                is_credits,
                strong,
                weak,
            }
        })
        .collect();

    let mut prefix_end = 0usize;
    while prefix_end < signals.len() {
        let s = &signals[prefix_end];
        if s.blank || s.strong || s.weak {
            prefix_end += 1;
        } else {
            break;
        }
    }

    let mut fold = vec![false; signals.len()];
    let mut weak_count = 0usize;
    for i in 0..prefix_end {
        let s = &signals[i];
        if s.blank {
            continue;
        }
        if s.is_credits {
            static AFTER_COLON: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"[:：]\s*(.+)$").expect("after colon"));
            if let Some(m) = AFTER_COLON.captures(&s.text) {
                collect_credits(&mut credits, &m[1]);
            }
        }
        if s.strong {
            fold[i] = true;
        } else if s.weak {
            weak_count += 1;
        }
    }
    if weak_count >= 2 {
        for i in 0..prefix_end {
            if signals[i].weak {
                fold[i] = true;
            }
        }
    }
    for i in 0..prefix_end {
        if signals[i].blank {
            continue;
        }
        let mut group = vec![i];
        let start_i = doc.lines[i].start.unwrap_or(0);
        let mut j = i + 1;
        while j < prefix_end && !signals[j].blank {
            let start_j = doc.lines[j].start.unwrap_or(0);
            if (start_j - start_i).abs() > 50 {
                break;
            }
            group.push(j);
            j += 1;
        }
        if group.len() >= 2 {
            for k in group {
                fold[k] = true;
            }
        }
    }

    let mut kept: Vec<ParsedLine> = Vec::new();
    for (i, line) in doc.lines.iter().enumerate() {
        if !fold[i] {
            kept.push(line.clone());
        }
    }
    static DUET_PREFIX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[男女合與与]：\s*").expect("duet prefix"));
    for line in &mut kept {
        line.text = DUET_PREFIX.replace(&line.text, "").to_string();
    }
    doc.lines = kept;
    if !credits.is_empty() {
        doc.credits = Some(credits);
    }
}

/// `instrumentalOnly` — the whole chart is one placeholder sentence.
pub fn instrumental_only(lines: &[ParsedLine]) -> bool {
    let texts: Vec<&str> = lines
        .iter()
        .map(|l| l.text.trim())
        .filter(|t| !t.is_empty())
        .collect();
    if texts.is_empty() {
        return false;
    }
    texts
        .iter()
        .all(|t| t.chars().count() <= 60 && INSTRUMENTAL_RE.is_match(t))
}

/// The raw payload a source returned for one candidate.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawLyrics {
    pub raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_list: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub romanization_list: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_lrc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roma_lrc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// What the source layer knows about the chosen provider + candidate.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScoredCandidate {
    pub id: String,
    pub best: Candidate,
}

/// `buildDoc(scored, ly)` — turn one source's raw payload into a document.
pub fn build_doc(scored: &ScoredCandidate, ly: &RawLyrics) -> Option<BuiltDoc> {
    let mut base = parse_lyrics(&ly.raw)?;
    if base.lines.is_empty() {
        return None;
    }

    if let Some(list) = ly.translation_list.as_ref().filter(|l| !l.is_empty()) {
        if apply_line_list(&mut base.lines, list, Field::Trans) {
            base.has_translation = base.lines.iter().any(|l| l.trans.is_some());
        }
    }
    if let Some(list) = ly.romanization_list.as_ref().filter(|l| !l.is_empty()) {
        if apply_line_list(&mut base.lines, list, Field::Roma) {
            base.has_romanization = Some(base.lines.iter().any(|l| l.roma.is_some()));
        }
    }

    let title = scored.best.title.clone();
    let artist = scored.best.artist_text();
    strip_meta_lines(&mut base, &title, &artist);

    let instrumental = instrumental_only(&base.lines);
    if instrumental {
        base.lines = vec![ParsedLine {
            start: Some(0),
            end: None,
            text: INSTRUMENTAL_TEXT.to_string(),
            words: Vec::new(),
            ..ParsedLine::default()
        }];
        base.has_word_level = false;
        base.has_translation = false;
        base.has_romanization = None;
        base.has_background = None;
    } else if base.lines.is_empty() {
        return None;
    }

    if let Some(lrc) = ly.translation_lrc.as_ref().filter(|_| !base.has_translation) {
        merge_lrc_field(&mut base, lrc, Field::Trans);
    }
    if let Some(lrc) = ly.roma_lrc.as_ref().filter(|_| base.has_romanization != Some(true)) {
        merge_lrc_field(&mut base, lrc, Field::Roma);
    }

    let out_lines: Vec<&ParsedLine> = base.lines.iter().filter(|l| l.start.is_some()).collect();
    if out_lines.is_empty() {
        return None;
    }

    let lines = out_lines
        .iter()
        .map(|l| DocLine {
            start: l.start.unwrap_or(0),
            end: l.end,
            text: l.text.clone(),
            bg: l.bg.clone(),
            trans: l.trans.clone(),
            roma: l.roma.clone(),
            singer: l.singer.clone(),
            bg_words: l.bg_words.clone(),
            bg_trans: l.bg_trans.clone(),
            words: if l.words.len() > 1 {
                Some(l.words.clone())
            } else {
                None
            },
        })
        .collect();

    // `scored.best.match?.score ?? 0` — the score the search stage recorded on
    // the candidate. `percent` is the picker's own field and is only a fallback
    // for callers that hand a picker result straight to `buildDoc`.
    let (match_score, match_percent) = match scored.best.match_info.as_ref() {
        Some(info) => (info.score, info.percent),
        None => {
            let percent = scored.best.percent.unwrap_or(0.0);
            (percent, percent)
        }
    };
    // `scored.best.durationMs || 0`; older call sites only carried seconds.
    let duration_ms = match (scored.best.duration_ms, scored.best.duration_sec) {
        (Some(ms), _) => ms.round() as i64,
        (None, Some(sec)) => (sec * 1000.0).round() as i64,
        (None, None) => 0,
    };
    Some(BuiltDoc {
        ok: true,
        provider: scored.id.clone(),
        provider_name: source_label(&scored.id).to_string(),
        id: scored.best.id.clone(),
        title: scored.best.title.clone(),
        artists: scored.best.artists.clone().unwrap_or_default(),
        album: scored.best.album.clone().unwrap_or_default(),
        duration_ms,
        match_score,
        match_percent,
        reference: ly.reference.clone().unwrap_or_default(),
        format: base.format.clone(),
        timing: base.timing.clone(),
        lang: base.lang.clone(),
        has_word_level: base.has_word_level,
        has_translation: base.has_translation,
        has_romanization: base.has_romanization.unwrap_or(false),
        has_background: base.has_background.unwrap_or(false),
        duet: base.duet.unwrap_or(false),
        instrumental,
        lines,
        credits: base.credits.clone(),
        ..BuiltDoc::default()
    })
}

/// `SOURCE_LABEL[scored.id] || scored.id` — providers the table does not know
/// (LRCLIB) display their own id rather than borrowing another source's name.
fn source_label(id: &str) -> String {
    let label = match id {
        "kugou" => "酷狗音乐",
        "netease" => "网易云音乐",
        "qq" => "QQ 音乐",
        other => other,
    };
    label.to_string()
}

/// Which field an attached LRC fills.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Trans,
    Roma,
}

impl Field {
    fn get(self, line: &ParsedLine) -> Option<&String> {
        match self {
            Self::Trans => line.trans.as_ref(),
            Self::Roma => line.roma.as_ref(),
        }
    }
    fn set(self, line: &mut ParsedLine, value: String) {
        match self {
            Self::Trans => line.trans = Some(value),
            Self::Roma => line.roma = Some(value),
        }
    }
}

fn apply_line_list(lines: &mut [ParsedLine], list: &[String], field: Field) -> bool {
    let slice = if list.len() == lines.len() {
        list
    } else if list.len() > lines.len() {
        let mut off = 0usize;
        while off < list.len() - lines.len() && list[off].trim().is_empty() {
            off += 1;
        }
        let s = &list[off..(off + lines.len()).min(list.len())];
        let usable = s
            .iter()
            .filter(|t| !t.trim().is_empty() && !is_placeholder(t))
            .count();
        if s.len() != lines.len() || usable * 2 < lines.len() {
            return false;
        }
        s
    } else {
        return false;
    };
    for (line, val) in lines.iter_mut().zip(slice.iter()) {
        let v = val.trim();
        if !v.is_empty() && !is_placeholder(v) && field.get(line).is_none() {
            field.set(line, v.to_string());
        }
    }
    true
}

fn norm_text(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

fn is_credit_text(text: &str) -> bool {
    static CREDIT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)作词|作詞|作曲|编曲|編曲|制作|製作|混音|母带|母帶|监制|監製|和声|和聲|弦乐|弦樂|producer|written by|composed by|lyrics by|music by|arranged by|produced by|mixed by|mastered by",
        )
        .expect("credit text")
    });
    CREDIT.is_match(text)
}

/// Paste another document's `trans`/`roma` onto matching rows.
///
/// Round one requires the source text to match (punctuation ignored); round two
/// accepts a near-identical timestamp, but never for a credit row or a blank
/// row — those have no lyrics text and would steal the first sung line's
/// translation. One donor row may fill at most one target row.
pub fn attach_field(base: &mut BuiltDoc, other: &BuiltDoc, field: Field) {
    let pool: Vec<&DocLine> = other
        .lines
        .iter()
        .filter(|l| match field {
            Field::Trans => l.trans.is_some(),
            Field::Roma => l.roma.is_some(),
        })
        .collect();
    if pool.is_empty() {
        return;
    }
    let mut used = vec![false; pool.len()];
    for line in &mut base.lines {
        let already = match field {
            Field::Trans => line.trans.is_some(),
            Field::Roma => line.roma.is_some(),
        };
        if already {
            continue;
        }
        let bt = norm_text(&line.text);
        let mut best: Option<usize> = None;
        let mut diff = f64::INFINITY;
        for (i, o) in pool.iter().enumerate() {
            if used[i] {
                continue;
            }
            let ot = norm_text(&o.text);
            if !bt.is_empty()
                && !ot.is_empty()
                && ot != bt
                && !ot.contains(&bt)
                && !bt.contains(&ot)
            {
                continue;
            }
            let d = (o.start - line.start).unsigned_abs() as f64;
            if d < diff {
                diff = d;
                best = Some(i);
            }
        }
        if let Some(i) = best {
            if diff <= 1500.0 {
                if let Some(value) = field.get_pair(pool[i]) {
                    field.set_doc(line, value);
                    used[i] = true;
                }
                continue;
            }
        }
        if bt.is_empty() || is_credit_text(&line.text) {
            continue;
        }
        for (i, o) in pool.iter().enumerate() {
            if used[i] {
                continue;
            }
            let d = (o.start - line.start).unsigned_abs() as f64;
            if d <= 400.0 && d < diff {
                diff = d;
                best = Some(i);
            }
        }
        if let Some(i) = best {
            if diff <= 400.0 {
                if let Some(value) = field.get_pair(pool[i]) {
                    field.set_doc(line, value);
                    used[i] = true;
                }
            }
        }
    }
}

impl Field {
    fn get_pair(self, line: &DocLine) -> Option<String> {
        match self {
            Self::Trans => line.trans.clone(),
            Self::Roma => line.roma.clone(),
        }
    }

    fn set_doc(self, line: &mut DocLine, value: String) {
        match self {
            Self::Trans => line.trans = Some(value),
            Self::Roma => line.roma = Some(value),
        }
    }
}

/// `mergeLrcField` — parse an independent LRC and attach it.
pub fn merge_lrc_field(base: &mut ParsedLyrics, lrc_text: &str, field: Field) -> usize {
    let parsed = parse_lrc(lrc_text).or_else(|| parse_lyrics(lrc_text));
    let Some(parsed) = parsed else {
        return 0;
    };
    let pool: Vec<ParsedLine> = parsed
        .lines
        .into_iter()
        .filter(|l| !l.text.is_empty() && !is_placeholder(&l.text))
        .collect();
    if pool.is_empty() {
        return 0;
    }
    let before = base.lines.iter().filter(|l| field.get(l).is_some()).count();
    // Build a throwaway document so the attachment logic stays in one place.
    let donor = BuiltDoc {
        lines: pool
            .iter()
            .map(|l| DocLine {
                start: l.start.unwrap_or(0),
                end: l.end,
                text: l.text.clone(),
                bg: None,
                trans: Some(l.text.clone()),
                roma: Some(l.text.clone()),
                singer: None,
                bg_words: None,
                bg_trans: None,
                words: None,
            })
            .collect(),
        ..BuiltDoc::default()
    };
    // attach_field works on BuiltDoc; base is ParsedLyrics. Convert by hand.
    let mut used = vec![false; donor.lines.len()];
    for line in &mut base.lines {
        if field.get(line).is_some() {
            continue;
        }
        let bt = norm_text(&line.text);
        let line_start = line.start.unwrap_or(0);
        let mut best: Option<usize> = None;
        let mut diff = f64::INFINITY;
        for (i, o) in donor.lines.iter().enumerate() {
            if used[i] {
                continue;
            }
            let ot = norm_text(&o.text);
            if !bt.is_empty() && !ot.is_empty() && ot != bt && !ot.contains(&bt) && !bt.contains(&ot) {
                continue;
            }
            let d = (o.start - line_start).unsigned_abs() as f64;
            if d < diff {
                diff = d;
                best = Some(i);
            }
        }
        if let Some(i) = best {
            if diff <= 1500.0 {
                field.set(line, donor.lines[i].text.clone());
                used[i] = true;
                continue;
            }
        }
        if bt.is_empty() || is_credit_text(&line.text) {
            continue;
        }
        for (i, o) in donor.lines.iter().enumerate() {
            if used[i] {
                continue;
            }
            let d = (o.start - line_start).unsigned_abs() as f64;
            if d <= 400.0 && d < diff {
                diff = d;
                best = Some(i);
            }
        }
        if let Some(i) = best {
            if diff <= 400.0 {
                field.set(line, donor.lines[i].text.clone());
                used[i] = true;
            }
        }
    }
    let after = base.lines.iter().filter(|l| field.get(l).is_some()).count();
    let n = after.saturating_sub(before);
    if n > 0 {
        match field {
            Field::Trans => base.has_translation = true,
            Field::Roma => base.has_romanization = Some(true),
        }
    }
    n
}

/// `attachWords` — move word timings onto rows with matching text.
pub fn attach_words(base: &mut BuiltDoc, other: &BuiltDoc) {
    let pool: Vec<&DocLine> = other
        .lines
        .iter()
        .filter(|l| l.words.as_ref().is_some_and(|w| w.len() > 1))
        .collect();
    if pool.is_empty() {
        return;
    }
    let mut used = vec![false; pool.len()];
    for line in &mut base.lines {
        if line.words.as_ref().is_some_and(|w| w.len() > 1) {
            continue;
        }
        let bt = norm_text(&line.text);
        if bt.is_empty() {
            continue;
        }
        let mut best: Option<usize> = None;
        let mut diff = f64::INFINITY;
        for (i, o) in pool.iter().enumerate() {
            if used[i] {
                continue;
            }
            let ot = norm_text(&o.text);
            if ot.is_empty() || (ot != bt && !ot.contains(&bt) && !bt.contains(&ot)) {
                continue;
            }
            let d = (o.start - line.start).unsigned_abs() as f64;
            if d < diff {
                diff = d;
                best = Some(i);
            }
        }
        let Some(i) = best else { continue };
        if diff > 1500.0 {
            continue;
        }
        used[i] = true;
        let donor = pool[i];
        let words = donor.words.clone().unwrap_or_default();
        let Some(first) = words.first() else { continue };
        let Some(last) = words.last() else { continue };
        let w0 = first.start.unwrap_or(0);
        let w1 = last.end.or(last.start).unwrap_or(w0);
        let span = (w1 - w0).max(1);
        let target = line.end.unwrap_or(line.start) - line.start;
        let k = if target > 0 && (span as f64) > target as f64 * 1.05 {
            (target as f64 / span as f64).max(0.5)
        } else {
            1.0
        };
        line.words = Some(
            words
                .iter()
                .map(|w| crate::types::Word {
                    text: w.text.clone(),
                    start: Some(line.start + (((w.start.unwrap_or(w0) - w0) as f64) * k) as i64),
                    end: Some(line.start + (((w.end.unwrap_or(w0) - w0) as f64) * k) as i64),
                })
                .collect(),
        );
    }
}

/// `transGaps` — row indices that have text but no translation.
pub fn trans_gaps(lines: &[DocLine]) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.trans.is_some() {
            continue;
        }
        if !crate::translate::needs_translation(&l.text) {
            continue;
        }
        out.push(i);
    }
    out
}

/// `shouldFillGaps`
pub fn should_fill_gaps(doc: &BuiltDoc, composed_from: bool, translated_by_us: bool) -> bool {
    if !doc.ok || doc.instrumental {
        return false;
    }
    composed_from || !doc.has_translation || translated_by_us
}

/// One composed document, in the exact shape `compose()` returns.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposedDoc {
    pub ok: bool,
    pub provider: String,
    pub provider_name: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_sec: f64,
    pub reference: String,
    pub format: String,
    pub timing: String,
    pub lang: String,
    pub match_score: f64,
    pub match_percent: f64,
    pub has_word_level: bool,
    pub has_translation: bool,
    pub translation_source: Option<String>,
    pub has_romanization: bool,
    pub has_background: bool,
    pub duet: bool,
    pub instrumental: bool,
    pub used_sources: Vec<String>,
    pub manual: bool,
    pub timing_shift_ms: i64,
    pub timing_nudged: i64,
    pub timing_shift_from: Option<String>,
    pub align_stats: Option<AlignStats>,
    pub lines: Vec<DocLine>,
    /// `credits` is assigned only when the builder collected names, so an
    /// absent key and an explicit `null` are different answers here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<Vec<String>>,
    pub needs_translation: bool,
    pub errors: Vec<String>,
}

fn coverage(doc: &BuiltDoc, track: &Track) -> f64 {
    let Some(duration) = track.duration_ms.filter(|d| *d > 0.0) else {
        return 0.0;
    };
    let Some(last) = doc.lines.last() else {
        return 0.0;
    };
    let end = last.end.unwrap_or(last.start) as f64;
    (end / duration).min(1.05)
}

fn cover_bonus(c: f64) -> f64 {
    if c >= 0.9 {
        1.5
    } else if c >= 0.75 {
        0.0
    } else {
        -2.0
    }
}

fn rung(doc: &BuiltDoc) -> i64 {
    if doc.has_word_level && doc.has_translation {
        if doc.provider == "amlldb" {
            6
        } else {
            5
        }
    } else if doc.has_word_level {
        4
    } else if doc.has_translation {
        3
    } else {
        1
    }
}

fn shift_line(line: &mut DocLine, d: i64) {
    line.start = (line.start + d).max(0);
    if let Some(end) = line.end {
        line.end = Some((end + d).max(line.start + 200));
    }
    if let Some(words) = line.words.as_mut() {
        for w in words {
            let start = (w.start.unwrap_or(0) + d).max(0);
            w.start = Some(start);
            w.end = Some((w.end.unwrap_or(start) + d).max(start + 30));
        }
    }
}

/// Match rows by normalized text and decide whether the offset is constant
/// enough to shift the whole chart, or has to be nudged row by row.
fn align_lines(base: &mut BuiltDoc, donor: &BuiltDoc) -> AlignStats {
    let pool: Vec<&DocLine> = donor
        .lines
        .iter()
        .filter(|l| !norm_text(&l.text).is_empty() && l.start >= 0)
        .collect();
    if pool.is_empty() || base.lines.is_empty() {
        return AlignStats::default();
    }

    let mut pairs: Vec<(usize, usize, bool)> = Vec::new();
    let mut j = 0usize;
    for i in 0..base.lines.len() {
        let t = norm_text(&base.lines[i].text);
        if t.is_empty() {
            continue;
        }
        for k in j..(j + 4).min(pool.len()) {
            let o = norm_text(&pool[k].text);
            if !o.is_empty() && (o == t || o.contains(&t) || t.contains(&o)) {
                pairs.push((i, k, o == t));
                j = k;
                break;
            }
        }
    }
    if pairs.len() < 4.max((base.lines.len() as f64 * 0.4) as usize) {
        return AlignStats {
            matched: pairs.len(),
            med: 0,
            mad: 0,
        };
    }

    let mut diffs: Vec<i64> = pairs
        .iter()
        .map(|(i, k, _)| pool[*k].start - base.lines[*i].start)
        .collect();
    diffs.sort_unstable();
    let med = diffs[diffs.len() / 2];
    let mad = diffs.iter().map(|d| (d - med).abs()).sum::<i64>() / diffs.len() as i64;

    if med.abs() >= 200 && mad <= 260 {
        for line in &mut base.lines {
            shift_line(line, med);
        }
        return AlignStats {
            matched: pairs.len(),
            med,
            mad,
        };
    }

    for (i, k, exact) in &pairs {
        if !exact {
            continue;
        }
        let d = pool[*k].start - base.lines[*i].start;
        if d.abs() < 200 || d.abs() > 3000 {
            continue;
        }
        let prev = if *i > 0 { base.lines.get(*i - 1).map(|l| l.start) } else { None };
        let next = base.lines.get(*i + 1).map(|l| l.start);
        let ns = base.lines[*i].start + d;
        if prev.is_some_and(|p| ns < p + 150) {
            continue;
        }
        if next.is_some_and(|n| n - ns < 150) {
            continue;
        }
        shift_line(&mut base.lines[*i], d);
    }
    AlignStats {
        matched: pairs.len(),
        med,
        mad,
    }
}

/// A donor may only contribute when it is the same song: title, artist and a
/// duration within eight seconds. Row-level text checks cannot stop a whole
/// document from another song being merged in (a BTS `SWIM` once leaked into a
/// Night Tapes chart this way).
fn donor_ok(base: &BuiltDoc, other: &BuiltDoc) -> bool {
    if !base.title.is_empty()
        && !other.title.is_empty()
        && !title_ok(&other.title, &base.title, None)
    {
        return false;
    }
    let artists = base.artists.join(" / ");
    if !artist_ok(&other.artists, &artists) {
        return false;
    }
    if base.duration_ms > 0
        && other.duration_ms > 0
        && (other.duration_ms - base.duration_ms).abs() > 8000
    {
        return false;
    }
    true
}

/// `compose(docs, track, errors, preferDoc)` — pick the main source and merge.
pub fn compose(
    docs: &[BuiltDoc],
    track: &Track,
    errors: Vec<String>,
    prefer: Option<usize>,
) -> serde_json::Value {
    if docs.is_empty() {
        return serde_json::json!({ "ok": false, "error": "无可用歌词", "errors": errors });
    }

    let mut order: Vec<usize> = (0..docs.len()).collect();
    order.sort_by(|a, b| {
        let ra = rung(&docs[*a]) as f64 * 1000.0
            + docs[*a].match_score
            + cover_bonus(coverage(&docs[*a], track));
        let rb = rung(&docs[*b]) as f64 * 1000.0
            + docs[*b].match_score
            + cover_bonus(coverage(&docs[*b], track));
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });

    let base_idx = match prefer {
        Some(p) if p < docs.len() => p,
        _ => order[0],
    };
    let base_real = docs[base_idx].clone();
    let mut base = base_real.clone();


    if base.has_word_level {
        let donor = order
            .iter()
            .map(|i| &docs[*i])
            .find(|d| {
                std::ptr::eq(*d, &docs[base_idx]) == false
                    && donor_ok(&base_real, d)
                    && !d.has_word_level
                    && coverage(d, track) >= 0.6
            });
        if let Some(donor) = donor {
            let stats = align_lines(&mut base, donor);
            if stats.med != 0 {
                base.timing_shift_ms = stats.med;
                base.timing_shift_from = Some(donor.provider_name.clone());
            } else if stats.matched > 0 {
                base.timing_nudged = stats.matched as i64;
                base.timing_shift_from = Some(donor.provider_name.clone());
            }
            if stats.matched > 0 {
                base.align_stats = Some(stats);
            }
        }
    }

    for idx in order.iter().skip(1) {
        if *idx == base_idx {
            continue;
        }
        let other = &docs[*idx];
        if !donor_ok(&base_real, other) {
            continue;
        }
        if !base.has_word_level && other.has_word_level {
            attach_words(&mut base, other);
            if base.lines.iter().any(|l| l.words.as_ref().is_some_and(|w| w.len() > 1)) {
                base.has_word_level = true;
            }
        }
        if !base.has_translation && other.has_translation {
            attach_field(&mut base, other, Field::Trans);
            if base.lines.iter().any(|l| l.trans.is_some()) {
                base.has_translation = true;
                base.translation_source = Some(other.provider.clone());
            }
        }
        if !base.has_romanization && other.has_romanization {
            attach_field(&mut base, other, Field::Roma);
            base.has_romanization = base.lines.iter().any(|l| l.roma.is_some());
        }
    }

    let used_sources: Vec<String> = order.iter().map(|i| docs[*i].provider_name.clone()).collect();
    let composed = ComposedDoc {
        ok: true,
        provider: base.provider.clone(),
        provider_name: base.provider_name.clone(),
        id: base.id.clone(),
        title: base.title.clone(),
        artist: base.artists.join(" / "),
        artists: base.artists.clone(),
        album: base.album.clone(),
        duration_sec: (base.duration_ms as f64 / 1000.0).round(),
        reference: base.reference.clone(),
        format: base.format.clone(),
        timing: base.timing.clone().unwrap_or_default(),
        lang: base.lang.clone().unwrap_or_default(),
        match_score: (base.match_score * 100.0).round() / 100.0,
        match_percent: base.match_percent,
        has_word_level: base.has_word_level,
        has_translation: base.has_translation,
        translation_source: base
            .translation_source
            .clone()
            .or_else(|| base.has_translation.then(|| base.provider.clone())),
        has_romanization: base.has_romanization,
        has_background: base.has_background,
        duet: base.duet,
        instrumental: base.instrumental,
        used_sources,
        manual: prefer.is_some(),
        timing_shift_ms: base.timing_shift_ms,
        timing_nudged: base.timing_nudged,
        timing_shift_from: base.timing_shift_from.clone(),
        align_stats: base.align_stats.clone(),
        lines: base.lines.clone(),
        credits: base.credits.clone(),
        needs_translation: !base.has_translation && !base.instrumental,
        errors,
    };
    serde_json::to_value(composed).expect("composed document serializes")
}
