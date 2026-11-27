//! Candidate arbitration: `gradeOf`, `gradeOfDoc`, `judge`, `shouldUpgrade`.
//!
//! Ported 1:1 from `tests/oracle/arbiter.js`. Arbitration is deliberately
//! separate from evidence gathering: the search / probe / rescue stages only
//! submit candidates, and one arbiter decides once the collection window
//! closes. Late evidence may reopen the decision, but only upward, and a manual
//! user pick always wins.
//!
//! Goldens: `native/tests/oracle/engine/arbiter.json` and `arbiter-tables.json`.

use crate::identity::identity_gate;
use crate::picker::{SortPolicy, TierPreference, cand_sort_with, kind_of};
use crate::types::{BuiltDoc, Candidate, Track};

/// Quality rungs, higher is better, strictly aligned with `picker::group_order`.
pub const GRADE_BOTH_BG: u8 = 6;
pub const GRADE_BOTH: u8 = 5;
pub const GRADE_TRANS: u8 = 4;
pub const GRADE_WORD: u8 = 3;
pub const GRADE_LINE: u8 = 2;
pub const GRADE_PLAIN: u8 = 1;

/// What a candidate can actually deliver, respecting `TierPreference`.
pub fn grade_of_with(c: &Candidate, pref: TierPreference) -> u8 {
    match kind_of(c) {
        "both_bg" => GRADE_BOTH_BG,
        "both" => GRADE_BOTH,
        "trans" => match pref {
            TierPreference::TransFirst => GRADE_TRANS,
            TierPreference::WordFirst => GRADE_WORD,
        },
        "word" => match pref {
            TierPreference::TransFirst => GRADE_WORD,
            TierPreference::WordFirst => GRADE_TRANS,
        },
        "line" => GRADE_LINE,
        _ => GRADE_PLAIN,
    }
}

/// What a candidate can actually deliver.
pub fn grade_of(c: &Candidate) -> u8 {
    grade_of_with(c, TierPreference::TransFirst)
}

/// The rung of an already-delivered document (rescue products and cached docs
/// have no `verified` block, so `grade_of` cannot judge them).
pub fn grade_of_doc(doc: Option<&BuiltDoc>) -> u8 {
    let Some(doc) = doc else {
        return 0;
    };
    if !doc.ok {
        return 0;
    }
    match (doc.has_word_level, doc.has_translation, doc.has_background) {
        (true, true, true) => GRADE_BOTH_BG,
        (true, true, false) => GRADE_BOTH,
        (false, true, _) => GRADE_TRANS,
        (true, false, _) => GRADE_WORD,
        (false, false, _) => {
            if doc.timing.is_some() {
                GRADE_LINE
            } else {
                GRADE_PLAIN
            }
        }
    }
}

/// Human label for a candidate's rung.
pub fn grade_label(c: &Candidate) -> &'static str {
    crate::picker::group_label(c)
}

/// The arbiter's verdict.
#[derive(Clone, Debug, Default)]
pub struct Judgement {
    pub ranked: Vec<Candidate>,
    pub pick: Option<Candidate>,
    pub best_grade: u8,
}

fn candidate_percent(c: &Candidate) -> Option<f64> {
    c.match_info
        .map(|m| m.percent)
        .or(c.percent)
        .filter(|p| p.is_finite())
}

fn candidate_duration_ms(c: &Candidate) -> Option<f64> {
    c.duration_sec
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| d * 1000.0)
        .or_else(|| c.duration_ms.filter(|d| d.is_finite() && *d > 0.0))
}

/// Decide once, over every live candidate.
///
/// Input must be the *unfiltered* verified candidates: the threshold in
/// `build_cands` only filters the panel, and passing its output here would hide
/// the LRCLIB row-level document forever.
pub fn judge(cands: &[Candidate], song: &Track) -> Judgement {
    judge_impl(cands, song, None, &SortPolicy::default())
}

/// Arbitrate using the panel's visibility threshold and same-song duration guard
/// so automatic selection picks the same candidate shown at the top of the list
/// whenever any verified candidate qualifies.
pub fn judge_with_threshold(cands: &[Candidate], song: &Track, threshold: f64) -> Judgement {
    judge_impl(cands, song, Some(threshold), &SortPolicy::default())
}

/// Arbitrate using the configured `SortPolicy` (source order + tier preference).
pub fn judge_with_policy(
    cands: &[Candidate],
    song: &Track,
    threshold: f64,
    policy: &SortPolicy,
) -> Judgement {
    judge_impl(cands, song, Some(threshold), policy)
}

fn judge_impl(
    cands: &[Candidate],
    song: &Track,
    threshold: Option<f64>,
    policy: &SortPolicy,
) -> Judgement {
    let mut alive: Vec<Candidate> = cands
        .iter()
        .filter(|c| {
            let verified = match &c.verified {
                Some(v) => v,
                None => return false,
            };
            if c.probe_failed == Some(true) {
                return false;
            }
            if verified.timeline == Some(false) {
                return false;
            }
            if let Some(lines) = verified.lines {
                if lines <= 0 {
                    return false;
                }
            }
            identity_gate(c, song)
        })
        .cloned()
        .collect();

    if let Some(threshold) = threshold.filter(|t| t.is_finite()) {
        let meets_threshold = |c: &Candidate| {
            c.rescue == Some(true) || candidate_percent(c).is_some_and(|p| p >= threshold)
        };
        if alive.iter().any(meets_threshold) {
            alive.retain(meets_threshold);
        }
        if let Some(song_ms) = song.duration_ms.filter(|d| d.is_finite() && *d > 0.0) {
            let duration_matches = |c: &Candidate| {
                candidate_duration_ms(c).is_some_and(|d| (d - song_ms).abs() <= 8000.0)
            };
            if alive.iter().any(duration_matches) {
                alive.retain(|c| candidate_duration_ms(c).is_none() || duration_matches(c));
            }
        }
    }

    alive.sort_by(|a, b| cand_sort_with(a, b, policy));
    let pick = alive.first().cloned();
    let best_grade = pick
        .as_ref()
        .map(|c| grade_of_with(c, policy.tier_preference))
        .unwrap_or(0);
    Judgement {
        ranked: alive,
        pick,
        best_grade,
    }
}

/// Late-arriving **candidate** evidence may only raise the rung.
pub fn should_upgrade(current_grade: u8, incoming: &Candidate, user_picked: bool) -> bool {
    if user_picked {
        return false;
    }
    grade_of(incoming) > current_grade
}

/// Late-arriving **document** evidence may only raise the rung.
pub fn should_upgrade_doc(current_grade: u8, doc: Option<&BuiltDoc>, user_picked: bool) -> bool {
    if user_picked {
        return false;
    }
    grade_of_doc(doc) > current_grade
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Verified;

    fn cand(source: &str, word: bool, trans: bool, percent: f64) -> Candidate {
        Candidate {
            source: source.into(),
            id: source.into(),
            title: "Lust For Life".into(),
            artist: Some("Lana Del Rey".into()),
            artists: Some(vec!["Lana Del Rey".into()]),
            album: Some("Lust For Life".into()),
            duration_sec: Some(290.0),
            percent: Some(percent),
            verified: Some(Verified {
                word: Some(word),
                trans: Some(trans),
                bg: None,
                lines: Some(40),
                timeline: Some(true),
                first_ms: Some(1000.0),
                foreign: None,
            }),
            ..Default::default()
        }
    }

    fn song() -> Track {
        Track {
            title: "Lust For Life".into(),
            artist: Some("Lana Del Rey".into()),
            album: Some("Lust For Life".into()),
            duration_ms: Some(290_000.0),
            ..Default::default()
        }
    }

    #[test]
    fn rungs_follow_what_the_candidate_carries() {
        assert_eq!(grade_of(&cand("kugou", true, false, 95.0)), GRADE_WORD);
        assert_eq!(grade_of(&cand("netease", false, true, 90.0)), GRADE_TRANS);
        assert_eq!(grade_of(&cand("qq", false, false, 60.0)), GRADE_LINE);
        let mut plain = cand("qq", false, false, 60.0);
        plain.verified.as_mut().unwrap().timeline = Some(false);
        assert_eq!(grade_of(&plain), GRADE_PLAIN);
        let both = cand("amlldb", true, true, 88.0);
        assert_eq!(grade_of(&both), GRADE_BOTH);
        let mut both_bg = cand("amlldb", true, true, 88.0);
        both_bg.verified.as_mut().unwrap().bg = Some(true);
        assert_eq!(grade_of(&both_bg), GRADE_BOTH_BG);
    }

    #[test]
    fn judge_sorts_by_rung_and_source_order_and_returns_the_pick() {
        let cands = vec![
            cand("qq", false, false, 60.0),
            cand("kugou", true, false, 95.0),
            cand("netease", false, true, 90.0),
        ];
        // `逐行+翻译` (`trans`) outranks `仅逐字` (`word`).
        let verdict = judge(&cands, &song());
        assert_eq!(verdict.best_grade, GRADE_TRANS);
        assert_eq!(verdict.pick.as_ref().unwrap().source, "netease");

        // Equal tier follows AMLL > QQ > Kugou > Netease > LRCLIB.
        let same_tier = vec![
            cand("lrclib", true, true, 100.0),
            cand("netease", true, true, 99.0),
            cand("kugou", true, true, 95.0),
            cand("qq", true, true, 90.0),
            cand("amlldb", true, true, 85.0),
        ];
        let verdict = judge(&same_tier, &song());
        let order: Vec<&str> = verdict.ranked.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(order, vec!["amlldb", "qq", "kugou", "netease", "lrclib"]);
        assert_eq!(verdict.pick.as_ref().unwrap().source, "amlldb");
    }

    #[test]
    fn upgrade_only_goes_up_and_never_over_a_user_pick() {
        assert!(should_upgrade(GRADE_WORD, &cand("netease", false, true, 90.0), false));
        assert!(!should_upgrade(GRADE_TRANS, &cand("kugou", true, false, 90.0), false));
        assert!(!should_upgrade(GRADE_BOTH, &cand("kugou", true, false, 90.0), false));
        assert!(!should_upgrade(GRADE_WORD, &cand("netease", false, true, 90.0), true));
    }

    #[test]
    fn judge_with_threshold_keeps_pick_above_threshold_and_duration_matched() {
        let mut sub_threshold_both = cand("amlldb", true, true, 62.0);
        sub_threshold_both.id = "sub-both".into();
        let mut wrong_duration_both = cand("qq", true, true, 82.0);
        wrong_duration_both.id = "wrong-dur".into();
        wrong_duration_both.duration_sec = Some(140.0);
        let mut valid_line = cand("netease", false, false, 96.0);
        valid_line.id = "valid-line".into();

        let verdict = judge_with_threshold(
            &[sub_threshold_both, wrong_duration_both, valid_line],
            &song(),
            70.0,
        );
        assert_eq!(verdict.pick.as_ref().map(|c| c.id.as_str()), Some("valid-line"));
    }
}
