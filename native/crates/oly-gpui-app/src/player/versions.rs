//! Presentation of lyric versions without losing the engine candidate identity.
use oly_engine::types::Candidate;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRow {
    pub source: String,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub detail: String,
    pub timing_badge: Option<&'static str>,
    pub has_translation: bool,
    /// Index in the original engine list, before threshold filtering.
    pub candidate_index: usize,
    pub selected: bool,
}

/// Prefer the scored shape. An invalid scored percentage must not silently
/// fall back to an unrelated legacy percentage (or the internal raw score).
pub fn candidate_percent(candidate: &Candidate) -> Option<f64> {
    let percent = candidate.match_info.map(|m| m.percent).or(candidate.percent)?;
    percent.is_finite().then_some(percent)
}

pub fn match_threshold(config: &Value) -> f64 {
    config.pointer("/lyrics/matchThreshold")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(70.0)
}

fn row_unusable(candidate: &Candidate) -> bool {
    candidate.probe_failed == Some(true)
        || candidate.verified.as_ref().is_some_and(|v| v.lines == Some(0))
}

/// Selected identity is (source, id), never the filtered row position.
/// Filtered rows share `oly_engine::picker::cand_sort_with` so the panel top
/// item always matches automatic selection.
///
/// Scheme A visibility:
/// - Verified usable rows always sort ahead of unverified/pending rows.
/// - When a source already has at least one verified usable candidate, drop
///   unprobed secondary entries (`verified.is_none()`) from that same source
///   unless currently selected.
/// - When a source has no verified usable candidate, keep at most one
///   unverified/timed-out/soft-failed placeholder row for that source (sunk
///   below verified candidates).
pub fn version_rows(
    candidates: &[Candidate],
    config: &Value,
    selected: Option<(&str, &str)>,
) -> Vec<VersionRow> {
    let threshold = match_threshold(config);
    let policy = oly_engine::picker::SortPolicy::from_config(config);
    let mut visible: Vec<(usize, &Candidate, f64, bool)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(candidate_index, candidate)| {
            let is_selected =
                selected.is_some_and(|(source, id)| source == candidate.source && id == candidate.id);
            let percent = candidate_percent(candidate).or_else(|| is_selected.then_some(0.0))?;
            (percent >= threshold || is_selected)
                .then_some((candidate_index, candidate, percent, is_selected))
        })
        .collect();
    visible.sort_by(|a, b| {
        row_unusable(a.1)
            .cmp(&row_unusable(b.1))
            .then_with(|| oly_engine::picker::cand_sort_with(a.1, b.1, &policy))
    });

    let mut verified_sources = std::collections::HashSet::new();
    for (_, cand, _, _) in &visible {
        if oly_engine::picker::is_verified_usable(cand) {
            verified_sources.insert(cand.source.as_str());
        }
    }
    let mut unverified_placeholder_kept = std::collections::HashSet::new();
    visible.retain(|(_, cand, _, is_selected)| {
        if *is_selected || cand.verified.is_some() {
            return true;
        }
        if verified_sources.contains(cand.source.as_str()) {
            return false;
        }
        unverified_placeholder_kept.insert(cand.source.as_str())
    });

    visible
        .into_iter()
        .map(|(candidate_index, candidate, percent, is_selected)| {
            let title = source_name(candidate);
            let artist = candidate.artist_text();
            let pct = if (percent - percent.round()).abs() < 0.05 {
                format!("匹配 {:.0}%", percent.round())
            } else {
                format!("匹配 {:.1}%", percent)
            };
            let mut meta = Vec::new();
            if !artist.trim().is_empty() { meta.push(artist); }
            if let Some(duration) = duration_text(candidate) { meta.push(duration); }
            if let Some(lines) = candidate.verified.as_ref().and_then(|v| v.lines) {
                if lines >= 0 { meta.push(format!("{} 行", lines)); }
            }
            if candidate.probe_timed_out == Some(true) {
                meta.push("验证超时".to_string());
            } else if candidate.probe_failed == Some(true) || candidate.probe_error.is_some() {
                meta.push("验证未完成".to_string());
            }
            let timing_badge = timing_badge(candidate);
            let has_translation = candidate.verified.as_ref().and_then(|v| v.trans) == Some(true);
            VersionRow {
                source: candidate.source.clone(),
                id: candidate.id.clone(),
                title,
                subtitle: meta.join(" · "),
                detail: pct,
                timing_badge,
                has_translation,
                candidate_index,
                selected: is_selected,
            }
        })
        .collect()
}

/// Recover the original candidate index for an identity that is still visible.
pub fn selected_candidate_index(rows: &[VersionRow], source: &str, id: &str) -> Option<usize> {
    rows.iter().find(|row| row.source == source && row.id == id).map(|row| row.candidate_index)
}

fn source_name(candidate: &Candidate) -> String {
    candidate.source_name.as_deref().filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| match candidate.source.as_str() {
            "qq" => "QQ 音乐",
            "netease" => "网易云音乐",
            "kugou" => "酷狗音乐",
            "amlldb" => "AMLL 歌词库",
            source => source,
        }).to_string()
}

fn timing_badge(candidate: &Candidate) -> Option<&'static str> {
    let verified = candidate.verified.as_ref();
    let timeline = verified.and_then(|v| v.timeline);
    let word = verified.and_then(|v| v.word);
    match (timeline, word) {
        (Some(false), _) => Some("纯文本"),
        (Some(true), Some(true)) => Some("逐字"),
        (Some(true), Some(false)) | (Some(true), None) => Some("逐行"),
        (None, Some(true)) => Some("逐字"),
        (None, None) if candidate.cap_word == Some(true) => Some("逐字"),
        _ => None,
    }
}

fn duration_text(candidate: &Candidate) -> Option<String> {
    let seconds = candidate.duration_sec.filter(|n| n.is_finite() && *n >= 0.0)
        .or_else(|| candidate.duration_ms.filter(|n| n.is_finite() && *n >= 0.0).map(|n| n / 1000.0))?;
    // Avoid saturating an arbitrary provider duration into an enormous label.
    if seconds > 86_400.0 { return None; }
    let seconds = seconds.round() as u64;
    Some(format!("{}:{:02}", seconds / 60, seconds % 60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oly_engine::types::{MatchInfo, Verified};
    use serde_json::json;

    fn candidate(source: &str, id: &str, percent: Option<f64>) -> Candidate {
        Candidate { source: source.into(), id: id.into(), title: "Song".into(), percent, ..Default::default() }
    }

    #[test]
    fn threshold_is_inclusive_and_unknown_is_never_zero() {
        let candidates = vec![candidate("qq", "below", Some(69.999)), candidate("qq", "edge", Some(70.0)), candidate("kugou", "above", Some(70.001)), candidate("qq", "unknown", None)];
        let rows = version_rows(&candidates, &json!({"lyrics":{"matchThreshold":70}}), None);
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["edge", "above"]);
        assert!(version_rows(&[candidate("qq", "unknown", None)], &json!({"lyrics":{"matchThreshold":0}}), None).is_empty());
        assert_eq!(version_rows(&[candidate("qq", "zero", Some(0.0))], &json!({"lyrics":{"matchThreshold":0}}), None).len(), 1);
    }

    #[test]
    fn scored_percentage_wins_and_raw_score_is_not_displayed() {
        let mut c = candidate("qq", "1", Some(99.0));
        c.match_info = Some(MatchInfo { score: 9876.0, percent: 72.5 });
        let rows = version_rows(&[c.clone()], &json!({}), None);
        assert!(rows[0].detail.contains("72.5%"));
        assert!(!rows[0].detail.contains("9876"));
        assert!(version_rows(&[c.clone()], &json!({"lyrics":{"matchThreshold":80}}), None).is_empty());
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            c.match_info.as_mut().unwrap().percent = invalid;
            assert!(candidate_percent(&c).is_none());
            assert!(version_rows(&[c.clone()], &json!({"lyrics":{"matchThreshold":0}}), None).is_empty());
            assert!(candidate_percent(&candidate("qq", "2", Some(invalid))).is_none());
        }
    }

    #[test]
    fn selection_and_click_mapping_use_identity_after_filtering() {
        let candidates = vec![candidate("qq", "x", Some(20.0)), candidate("netease", "same", Some(90.0)), candidate("qq", "same", Some(85.0))];
        let rows = version_rows(&candidates, &json!({}), Some(("qq", "same")));
        // QQ outranks Netease under equal content tier, and preserves candidate_index 2.
        assert!(rows[0].selected);
        assert!(!rows[1].selected);
        assert_eq!(selected_candidate_index(&rows, "qq", "same"), Some(2));
        assert_eq!(selected_candidate_index(&rows, "qq", "x"), None);
    }

    #[test]
    fn orders_content_tiers_and_sources_consistently() {
        let make = |source: &str, id: &str, word: bool, trans: bool, bg: bool, timeline: bool| Candidate {
            source: source.into(),
            id: id.into(),
            title: "Song".into(),
            percent: Some(90.0),
            verified: Some(Verified {
                word: Some(word),
                trans: Some(trans),
                bg: Some(bg),
                timeline: Some(timeline),
                lines: Some(30),
                ..Default::default()
            }),
            ..Default::default()
        };
        let candidates = vec![
            make("amlldb", "plain", false, false, false, false),
            make("amlldb", "line", false, false, false, true),
            make("amlldb", "word", true, false, false, true),
            make("lrclib", "trans", false, true, false, true),
            make("netease", "both", true, true, false, true),
            make("kugou", "both", true, true, false, true),
            make("qq", "both", true, true, false, true),
            make("amlldb", "both", true, true, false, true),
            make("lrclib", "both_bg", true, true, true, true),
        ];
        let rows = version_rows(&candidates, &json!({}), None);
        let keys: Vec<(String, String)> = rows.into_iter().map(|r| (r.source, r.id)).collect();
        assert_eq!(
            keys,
            vec![
                ("lrclib".into(), "both_bg".into()),
                ("amlldb".into(), "both".into()),
                ("qq".into(), "both".into()),
                ("kugou".into(), "both".into()),
                ("netease".into(), "both".into()),
                ("lrclib".into(), "trans".into()),
                ("amlldb".into(), "word".into()),
                ("amlldb".into(), "line".into()),
                ("amlldb".into(), "plain".into()),
            ]
        );
    }

    #[test]
    fn timed_out_probe_remains_unknown_and_verified_beats_hints() {
        let mut c = candidate("qq", "1", Some(90.0));
        c.probe_timed_out = Some(true);
        c.cap_word = Some(true);
        c.cap_trans = Some(true);
        let row = &version_rows(&[c.clone()], &json!({}), None)[0];
        assert!(row.subtitle.contains("验证超时"));
        assert_eq!(row.timing_badge, Some("逐字"));
        assert!(!row.has_translation);
        c.verified = Some(Verified { timeline: Some(true), word: Some(false), trans: Some(false), lines: Some(42), ..Default::default() });
        let row = &version_rows(&[c.clone()], &json!({}), None)[0];
        assert_eq!(row.timing_badge, Some("逐行"));
        assert!(!row.has_translation);
        assert!(row.subtitle.contains("42 行"));
        c.verified.as_mut().unwrap().timeline = Some(false);
        assert_eq!(version_rows(&[c.clone()], &json!({}), None)[0].timing_badge, Some("纯文本"));
        c.verified.as_mut().unwrap().timeline = Some(true);
        c.verified.as_mut().unwrap().word = Some(true);
        c.verified.as_mut().unwrap().trans = Some(true);
        let row = &version_rows(&[c], &json!({}), None)[0];
        assert_eq!(row.timing_badge, Some("逐字"));
        assert!(row.has_translation);
    }

    #[test]
    fn subtitle_uses_provider_title_artist_and_optional_duration() {
        let mut c = candidate("qq", "1", Some(100.0));
        c.source_name = Some("Provider".into());
        c.artists = Some(vec!["A".into(), "B".into()]);
        c.duration_ms = Some(125_000.0);
        let row = &version_rows(&[c], &json!({}), None)[0];
        assert_eq!(row.title, "Provider");
        assert_eq!(row.subtitle, "A / B · 2:05");
    }

    #[test]
    fn active_selected_candidate_remains_visible_below_threshold() {
        let candidates = vec![
            candidate("amlldb", "starboy-amll", Some(30.0)),
            candidate("qq", "starboy-qq", Some(95.0)),
        ];
        let rows = version_rows(
            &candidates,
            &json!({"lyrics":{"matchThreshold":70}}),
            Some(("amlldb", "starboy-amll")),
        );
        assert!(rows.iter().any(|r| r.source == "amlldb" && r.id == "starboy-amll" && r.selected));
        assert!(rows.iter().any(|r| r.source == "qq" && r.id == "starboy-qq" && !r.selected));
    }

    #[test]
    fn scheme_a_sinks_unverified_and_respects_source_order_and_tier_preference() {
        let mut qq_unverified = candidate("qq", "qq-u", Some(90.0));
        qq_unverified.cap_word = Some(true);
        qq_unverified.cap_trans = Some(true);
        qq_unverified.probe_error = Some("soft fail".into());

        let mut qq_unverified_2 = candidate("qq", "qq-u2", Some(90.0));
        qq_unverified_2.cap_word = Some(true);
        qq_unverified_2.cap_trans = Some(true);

        let mut netease_trans = candidate("netease", "ne-1", Some(90.0));
        netease_trans.verified = Some(Verified {
            timeline: Some(true),
            word: Some(false),
            trans: Some(true),
            lines: Some(24),
            ..Default::default()
        });

        let mut kugou_word = candidate("kugou", "kg-1", Some(90.0));
        kugou_word.verified = Some(Verified {
            timeline: Some(true),
            word: Some(true),
            trans: Some(false),
            lines: Some(55),
            ..Default::default()
        });

        let cfg_trans = json!({"lyrics": {"matchThreshold": 70, "tierPreference": "trans"}});
        let rows = version_rows(
            &[qq_unverified.clone(), qq_unverified_2.clone(), netease_trans.clone(), kugou_word.clone()],
            &cfg_trans,
            None,
        );
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["ne-1", "kg-1", "qq-u"]);

        let cfg_word = json!({"lyrics": {"matchThreshold": 70, "tierPreference": "word"}});
        let rows_word = version_rows(
            &[qq_unverified, qq_unverified_2, netease_trans, kugou_word],
            &cfg_word,
            None,
        );
        let ids_word: Vec<&str> = rows_word.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids_word, vec!["kg-1", "ne-1", "qq-u"]);

        let mut qq_both = candidate("qq", "qq-v", Some(90.0));
        qq_both.verified = Some(Verified { timeline: Some(true), word: Some(true), trans: Some(true), lines: Some(50), ..Default::default() });
        let mut kg_both = candidate("kugou", "kg-v", Some(90.0));
        kg_both.verified = Some(Verified { timeline: Some(true), word: Some(true), trans: Some(true), lines: Some(50), ..Default::default() });
        let cfg_order = json!({"lyrics": {"matchThreshold": 70, "sourceOrder": ["kugou", "qq", "amlldb", "netease", "lrclib"]}});
        let rows_order = version_rows(&[qq_both, kg_both], &cfg_order, None);
        assert_eq!(rows_order[0].source, "kugou");
        assert_eq!(rows_order[1].source, "qq");
    }
}
