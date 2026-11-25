//! The lyric-version picker: the single ordering authority plus the busy/
//! error/close choreography of the card.
//!
//! Ported from `src/shared/picker-order.ts` (the ordering), `picker-present.ts`
//! (row identity and "is this the live document") and `PickerOverlay.tsx` (the
//! card's own behaviour: busy timeout, error surface, self-close after a
//! successful pick).
//!
//! Ordering rules, kept exactly (the header comment of the legacy file is the
//! contract: 「自动挑的那份」永远等于「面板里排第一」):
//!
//! * Privileged double-full candidates first (AMLL > QQ), then group
//!   (逐字+译文 > 仅译文 > 仅逐字 > 其他), then source privilege, then —
//!   *same source and same submission signature* — first vocal line (percent
//!   is metadata noise within one submission), then percent, then firstMs.
//! * `plain` candidates are demoted to 30 before the threshold filter, except
//!   instrumental ones.
//! * The per-source benchmark exemption: a same-submission candidate below the
//!   threshold stays, unless it is `plain` (which must not resurrect itself
//!   through its own benchmark).
//! * Fused rows are built from verified word×trans pairs from *different*
//!   sources, deduplicated by source pair, at most 4.

/// One candidate row, the runtime shape after probing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Candidate {
    pub source: String,
    pub id: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_sec: Option<f64>,
    pub percent: Option<f64>,
    /// Static capability before probing.
    pub cap_word: bool,
    pub cap_trans: bool,
    pub probe_failed: bool,
    pub verified: Option<Verified>,
    pub fused: bool,
    /// Fused rows only: their two halves.
    pub word: Option<Box<Candidate>>,
    pub trans: Option<Box<Candidate>>,
}

/// Probe result; the presence of a `Verified` means the probe ran.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Verified {
    pub word: bool,
    pub trans: bool,
    pub bg: bool,
    pub lines: u32,
    pub timeline: bool,
    pub first_ms: Option<f64>,
    pub instrumental: bool,
}

impl Candidate {
    fn artists_set(&self) -> Vec<String> {
        (self.artist.as_deref().unwrap_or(""))
            .split('/')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .into_iter()
            .collect()
    }

    /// Same-submission signature: title + artist set (order-insensitive),
    /// lowercased.
    pub fn submission_sig(&self) -> String {
        let mut artists = self.artists_set();
        artists.sort();
        format!(
            "{}|{}",
            self.title.as_deref().unwrap_or("").trim().to_lowercase(),
            artists.join("/")
        )
    }

    /// The first vocal line when known; unknown sorts last.
    fn first_ms(&self) -> f64 {
        self.verified
            .as_ref()
            .and_then(|v| v.first_ms)
            .unwrap_or(f64::INFINITY)
    }
}

/// Classification: both_bg, both, trans, word, line or plain.
pub fn kind_of(c: &Candidate) -> &'static str {
    if c.fused {
        return if c.verified.as_ref().is_some_and(|v| v.bg) {
            "both_bg"
        } else {
            "both"
        };
    }
    match &c.verified {
        Some(v) => {
            if v.word && v.trans && v.bg {
                "both_bg"
            } else if v.word && v.trans {
                "both"
            } else if v.trans {
                "trans"
            } else if v.word {
                "word"
            } else if v.timeline {
                "line"
            } else {
                "plain"
            }
        }
        None => {
            if c.cap_word && c.cap_trans {
                "both"
            } else if c.cap_trans {
                "trans"
            } else if c.cap_word {
                "word"
            } else {
                "plain"
            }
        }
    }
}

/// Source privilege: AMLL > QQ > Kugou > Netease > LRCLIB.
fn src_privilege(c: &Candidate) -> f64 {
    if c.fused {
        return 0.0;
    }
    match c.source.as_str() {
        "amlldb" | "amllb" | "amll" => 5.0,
        "qq" => 4.0,
        "kugou" => 3.0,
        "netease" => 2.0,
        "lrclib" => 1.0,
        _ => 0.0,
    }
}

/// The single ordering authority (legacy `candSort`).
pub fn cand_sort(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let ga = group_rank(kind_of(a));
    let gb = group_rank(kind_of(b));
    if ga != gb {
        return ga.cmp(&gb);
    }
    let sa = src_privilege(a);
    let sb = src_privilege(b);
    if sa != sb {
        return sb.partial_cmp(&sa).unwrap_or(Ordering::Equal);
    }
    if a.source == b.source && a.submission_sig() == b.submission_sig() {
        return a
            .first_ms()
            .partial_cmp(&b.first_ms())
            .unwrap_or(Ordering::Equal);
    }
    let p = b.percent.unwrap_or(0.0) - a.percent.unwrap_or(0.0);
    if p != 0.0 {
        return p.partial_cmp(&0.0).unwrap_or(Ordering::Equal);
    }
    a.first_ms().partial_cmp(&b.first_ms()).unwrap_or(Ordering::Equal)
}

fn group_rank(kind: &str) -> u8 {
    match kind {
        "both_bg" => 0,
        "both" => 1,
        "trans" => 2,
        "word" => 3,
        "line" => 4,
        _ => 5,
    }
}

/// Can this row be shown at all? Probe failures and timelines without lines
/// are dropped (legacy `usable`).
pub fn usable(c: &Candidate) -> bool {
    if c.probe_failed {
        return false;
    }
    match &c.verified {
        Some(v) => v.timeline && v.lines > 0,
        None => true,
    }
}

/// The search-to-panel pipeline (legacy `buildCands`): threshold filter with
/// the same-submission exemption, same-song narrowing, per-source best + alt,
/// then verified fusion pairs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CandsResult {
    pub cands: Vec<Candidate>,
    pub fused: Vec<Candidate>,
    pub source_count: usize,
}

pub fn build_cands(raw: &[Candidate], threshold: f64, duration_ms: f64) -> CandsResult {
    // Demote plain to 30 unless instrumental, before the threshold filter.
    let list: Vec<Candidate> = raw
        .iter()
        .map(|c| {
            let demote = kind_of(c) == "plain"
                && !c.verified.as_ref().is_some_and(|v| v.instrumental);
            if demote {
                let mut c2 = c.clone();
                c2.percent = Some(30.0);
                c2
            } else {
                c.clone()
            }
        })
        .collect();

    // Per-source benchmark = highest-percent candidate's submission signature.
    let mut best_sig: std::collections::HashMap<&str, (f64, String)> = Default::default();
    for c in &list {
        match best_sig.get(c.source.as_str()) {
            Some((p, _)) if *p >= c.percent.unwrap_or(0.0) => {}
            _ => {
                best_sig.insert(
                    c.source.as_str(),
                    (c.percent.unwrap_or(0.0), c.submission_sig()),
                );
            }
        }
    }

    let mut passed: Vec<Candidate> = list
        .iter()
        .filter(|c| {
            usable(c)
                && (c.percent.unwrap_or(0.0) >= threshold
                    || (kind_of(c) != "plain"
                        && best_sig
                            .get(c.source.as_str())
                            .is_some_and(|(_, s)| *s == c.submission_sig())))
        })
        .cloned()
        .collect();

    let same_song = |c: &Candidate| -> bool {
        if duration_ms > 0.0 && c.duration_sec.unwrap_or(0.0) > 0.0 {
            return (c.duration_sec.unwrap() * 1000.0 - duration_ms).abs() <= 8000.0;
        }
        best_sig
            .get(c.source.as_str())
            .is_some_and(|(_, s)| *s == c.submission_sig())
            || c.percent.unwrap_or(0.0) >= 95.0
    };
    if passed.iter().any(same_song) {
        passed.retain(same_song);
    }

    let mut by_source: std::collections::BTreeMap<String, Vec<Candidate>> = Default::default();
    for c in passed {
        by_source.entry(c.source.clone()).or_default().push(c);
    }
    let source_count = by_source.len();
    let mut cands: Vec<Candidate> = Vec::new();
    for (_, mut arr) in by_source {
        arr.sort_by(cand_sort);
        let best = arr.remove(0);
        cands.push(best.clone());
        if best.duration_sec.unwrap_or(0.0) > 0.0 {
            let alt = arr.into_iter().find(|x| {
                x.percent.unwrap_or(0.0) >= 90.0
                    && (x.duration_sec.unwrap_or(0.0) - best.duration_sec.unwrap()).abs() > 5.0
            });
            if let Some(alt) = alt {
                cands.push(alt);
            }
        } else {
            for x in arr.into_iter().take(2) {
                if x.percent.unwrap_or(0.0) >= 90.0
                    || best_sig
                        .get(x.source.as_str())
                        .is_some_and(|(_, s)| *s == x.submission_sig())
                {
                    cands.push(x);
                }
            }
        }
    }

    // Fusion pairs: verified word × verified trans from different sources.
    let words: Vec<&Candidate> = cands
        .iter()
        .filter(|c| kind_of(c) == "word" && c.verified.is_some())
        .collect();
    let transs: Vec<&Candidate> = cands
        .iter()
        .filter(|c| kind_of(c) == "trans" && c.verified.is_some())
        .collect();
    let mut fused: Vec<Candidate> = Vec::new();
    for w in &words {
        for t in &transs {
            if w.source == t.source {
                continue;
            }
            fused.push(fuse(w, t));
        }
    }
    fused.sort_by(|a, b| {
        b.percent
            .unwrap_or(0.0)
            .partial_cmp(&a.percent.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut seen = std::collections::HashSet::new();
    let uniq: Vec<Candidate> = fused
        .into_iter()
        .filter(|f| {
            let key = format!(
                "{}\u{1}{}",
                f.word.as_ref().unwrap().source,
                f.trans.as_ref().unwrap().source
            );
            seen.insert(key)
        })
        .collect();

    CandsResult {
        cands,
        fused: uniq.into_iter().take(4).collect(),
        source_count,
    }
}

fn fuse(w: &Candidate, t: &Candidate) -> Candidate {
    let lines = w
        .verified
        .as_ref()
        .map(|v| v.lines)
        .or_else(|| t.verified.as_ref().map(|v| v.lines))
        .unwrap_or(0);
    Candidate {
        source: "fused".into(),
        id: Some(format!("{}+{}", w.source, t.source)),
        title: w.title.clone(),
        artist: w.artist.clone(),
        duration_sec: w.duration_sec,
        percent: Some(
            w.percent
                .unwrap_or(0.0)
                .min(t.percent.unwrap_or(0.0)),
        ),
        cap_word: true,
        cap_trans: true,
        verified: Some(Verified {
            word: true,
            trans: true,
            bg: w.verified.as_ref().is_some_and(|v| v.bg),
            lines,
            timeline: true,
            first_ms: None,
            instrumental: false,
        }),
        fused: true,
        word: Some(Box::new(w.clone())),
        trans: Some(Box::new(t.clone())),
        ..Default::default()
    }
}

/// The live document the panel checks rows against. `composed_from` is set on
/// fused documents at runtime (not in the TS type, hence the loose shape).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveDoc {
    pub id: String,
    pub provider: String,
    pub composed_from: Vec<String>,
}

/// Stable row identity across snapshots (legacy `rowKey`).
pub fn row_key(c: &Candidate) -> String {
    format!("{}\u{1}{}", c.source, c.id.as_deref().unwrap_or(""))
}

/// Is this row the one on screen? Fused rows match by their two sources; plain
/// rows by provider+id, and never when the live doc is itself a composition.
pub fn is_current(c: &Candidate, doc: &LiveDoc) -> bool {
    if doc.composed_from.len() > 1 {
        if !c.fused {
            return false;
        }
        let (Some(w), Some(t)) = (&c.word, &c.trans) else {
            return false;
        };
        return doc.composed_from.contains(&w.source) && doc.composed_from.contains(&t.source);
    }
    if c.fused {
        return false;
    }
    c.source == doc.provider && c.id.as_deref().unwrap_or("") == doc.id
}

/// The card's own state machine: busy row, error surface, closing timer.
/// Legacy constants: an 8 s busy timeout, a 220 ms exit before unmount.
pub const BUSY_TIMEOUT_MS: f64 = 8000.0;
pub const EXIT_MS: f64 = 220.0;

#[derive(Debug, Default)]
pub struct PickerCard {
    busy: Option<String>,
    error: Option<String>,
    closing: bool,
}

impl PickerCard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn busy(&self) -> Option<&str> {
        self.busy.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_closing(&self) -> bool {
        self.closing
    }

    /// A new snapshot (new track or the doc a pick produced) clears busy,
    /// including one that lands while the card is already closing.
    pub fn on_snapshot(&mut self) {
        self.busy = None;
    }

    /// The user chose a row. Returns false when the click is ignored (busy,
    /// closing, or the row is already live — the last case requests a close
    /// instead).
    pub fn choose(&mut self, row: &Candidate, doc: &LiveDoc) -> Choice {
        if self.busy.is_some() || self.closing {
            return Choice::Ignored;
        }
        if is_current(row, doc) {
            self.request_close();
            return Choice::Closed;
        }
        self.error = None;
        self.busy = Some(row_key(row));
        Choice::Picked
    }

    /// The pick resolved. `ok` closes the card on its own exit; a failure
    /// surfaces the error and unlocks.
    pub fn on_pick_result(&mut self, ok: bool, error: Option<&str>) -> Choice {
        self.busy = None;
        if ok {
            self.request_close();
            Choice::Closed
        } else {
            self.error = Some(error.unwrap_or("切换失败").to_string());
            Choice::Ignored
        }
    }

    pub fn request_close(&mut self) {
        self.closing = true;
    }

    /// The busy timeout elapsed (8 s without a verdict) — unlock the card.
    pub fn on_busy_timeout(&mut self) {
        self.busy = None;
    }
}

/// What a user gesture on the card led to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Picked,
    Closed,
    Ignored,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(source: &str, id: &str, percent: f64) -> Candidate {
        Candidate {
            source: source.into(),
            id: Some(id.into()),
            percent: Some(percent),
            ..Default::default()
        }
    }

    fn verified_word(source: &str, id: &str, lines: u32, first_ms: f64) -> Candidate {
        Candidate {
            source: source.into(),
            id: Some(id.into()),
            verified: Some(Verified {
                word: true,
                lines,
                timeline: true,
                first_ms: Some(first_ms),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn verified_trans(source: &str, id: &str, lines: u32) -> Candidate {
        Candidate {
            source: source.into(),
            id: Some(id.into()),
            verified: Some(Verified {
                trans: true,
                lines,
                timeline: true,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn privileged_amll_both_beats_qq_both_beats_the_rest() {
        let amll = Candidate {
            source: "amllb".into(),
            verified: Some(Verified { word: true, trans: true, ..Default::default() }),
            ..cand("amllb", "a", 80.0)
        };
        let qq = Candidate {
            source: "qq".into(),
            verified: Some(Verified { word: true, trans: true, ..Default::default() }),
            ..cand("qq", "q", 100.0)
        };
        let kugou = cand("kugou", "k", 100.0);
        let mut rows = vec![kugou, qq, amll];
        rows.sort_by(cand_sort);
        assert_eq!(rows[0].source, "amllb");
        assert_eq!(rows[1].source, "qq");
        assert_eq!(rows[2].source, "kugou");
    }

    #[test]
    fn same_submission_prefers_the_earliest_first_line_over_percent() {
        // The exile case: a truncated 95% submission loses to a full 30% one
        // with the same signature but an earlier vocal start.
        let truncated = Candidate {
            title: Some("exile".into()),
            artist: Some("Bon Iver / Taylor Swift".into()),
            percent: Some(95.0),
            verified: Some(Verified { word: true, timeline: true, first_ms: Some(20_000.0), ..Default::default() }),
            ..cand("amllb", "short", 95.0)
        };
        let full = Candidate {
            title: Some("exile".into()),
            artist: Some("Taylor Swift / Bon Iver".into()),
            percent: Some(30.0),
            verified: Some(Verified { word: true, timeline: true, first_ms: Some(5_000.0), ..Default::default() }),
            ..cand("amllb", "full", 30.0)
        };
        let mut rows = vec![truncated, full];
        rows.sort_by(cand_sort);
        assert_eq!(rows[0].id.as_deref(), Some("full"));
    }

    #[test]
    fn plain_candidates_are_demoted_below_the_threshold() {
        let raw = vec![
            cand("lrclib", "plain", 100.0), // no caps, no verification: plain
            Candidate {
                source: "qq".into(),
                id: Some("q".into()),
                percent: Some(75.0),
                // usable() requires a timeline and at least one line, exactly
                // like the legacy `(!v.timeline || !v.lines)` guard.
                verified: Some(Verified { word: true, timeline: true, lines: 50, ..Default::default() }),
                ..Default::default()
            },
        ];
        let out = build_cands(&raw, 70.0, 0.0);
        // The demoted plain (30) misses the threshold and has no benchmark
        // exemption (plain never self-exempts); the word candidate passes.
        assert_eq!(out.cands.len(), 1);
        assert_eq!(out.cands[0].source, "qq");
    }

    #[test]
    fn same_submission_below_the_threshold_is_exempted() {
        let full = Candidate {
            title: Some("exile".into()),
            artist: Some("Taylor Swift / Bon Iver".into()),
            percent: Some(30.0),
            verified: Some(Verified { word: true, timeline: true, lines: 50, ..Default::default() }),
            ..cand("amllb", "full", 30.0)
        };
        let truncated = Candidate {
            title: Some("exile".into()),
            artist: Some("Bon Iver / Taylor Swift".into()),
            percent: Some(95.0),
            verified: Some(Verified { word: true, timeline: true, lines: 40, ..Default::default() }),
            ..cand("amllb", "short", 95.0)
        };
        let out = build_cands(&[truncated, full], 70.0, 0.0);
        let ids: Vec<&str> = out.cands.iter().filter_map(|c| c.id.as_deref()).collect();
        assert!(ids.contains(&"full"), "the same-submission 30% row survives");
    }

    #[test]
    fn fusion_pairs_use_verified_halves_from_different_sources() {
        let w = verified_word("amllb", "a", 50, 0.0);
        let t = verified_trans("qq", "q", 50);
        let out = build_cands(&[w.clone(), t.clone()], 70.0, 0.0);
        assert_eq!(out.fused.len(), 1, "one verified pair across sources");
        let f = &out.fused[0];
        assert_eq!(f.word.as_ref().unwrap().source, "amllb");
        assert_eq!(f.trans.as_ref().unwrap().source, "qq");
        // A same-source pair must not fuse.
        let t2 = verified_trans("amllb", "a2", 50);
        let out2 = build_cands(&[w, t2], 70.0, 0.0);
        assert_eq!(out2.fused.len(), 0, "same-source halves do not fuse");
    }

    #[test]
    fn is_current_matches_fused_rows_by_source_pair() {
        let w = verified_word("amllb", "a", 50, 0.0);
        let t = verified_trans("qq", "q", 50);
        let fused = fuse(&w, &t);
        let live = LiveDoc {
            id: "x".into(),
            provider: "fused".into(),
            composed_from: vec!["qq".into(), "amllb".into()],
        };
        assert!(is_current(&fused, &live));
        // A plain row never lights while a composition is live.
        assert!(!is_current(&w, &live));
    }

    #[test]
    fn the_card_locks_during_a_pick_and_unlocks_on_failure() {
        let mut card = PickerCard::new();
        let w = verified_word("amllb", "a", 50, 0.0);
        let doc = LiveDoc::default();
        assert_eq!(card.choose(&w, &doc), Choice::Picked);
        assert!(card.busy().is_some());
        // A second click while busy is ignored.
        assert_eq!(card.choose(&w, &doc), Choice::Ignored);
        assert_eq!(card.on_pick_result(false, None), Choice::Ignored);
        assert_eq!(card.error(), Some("切换失败"));
        assert!(card.busy().is_none(), "failure unlocks");
    }

    #[test]
    fn a_successful_pick_closes_the_card_after_its_exit() {
        let mut card = PickerCard::new();
        let w = verified_word("amllb", "a", 50, 0.0);
        assert_eq!(card.choose(&w, &LiveDoc::default()), Choice::Picked);
        assert_eq!(card.on_pick_result(true, None), Choice::Closed);
        assert!(card.is_closing());
    }

    #[test]
    fn clicking_the_live_row_just_closes() {
        let mut card = PickerCard::new();
        let w = verified_word("amllb", "a", 50, 0.0);
        let doc = LiveDoc { id: "a".into(), provider: "amllb".into(), composed_from: vec![] };
        assert_eq!(card.choose(&w, &doc), Choice::Closed);
        assert!(card.busy().is_none(), "no pick was issued");
    }
}
