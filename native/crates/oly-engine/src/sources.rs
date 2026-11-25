//! Pure helpers shared by the provider adapters.
//!
//! The HTTP clients themselves land with the Task 12 wiring; what is testable
//! without a network is the candidate shaping, and that is what the oracle
//! pins here.

use std::sync::LazyLock;

use regex::Regex;

/// `amlldbTitleRelated` — the AMLL community library matches on a title
/// substring, so searching a multi-word query returns noise: `21` hits 883
/// unrelated submissions, `Enter` matches everything by `carpenter`. Every
/// query token must therefore reappear in the candidate title, where a Latin
/// token must be a standalone word (`21` must not match `武家坡2021`) and a CJK
/// token may be a substring (Chinese has no spaces).
pub fn amlldb_title_related(query: &str, title: &str) -> bool {
    fn norm(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.to_lowercase().chars() {
            if c.is_alphanumeric() || c.is_whitespace() {
                out.push(c);
            } else {
                out.push(' ');
            }
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    static CJK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[\u{4e00}-\u{9fff}\u{3040}-\u{30ff}\u{3400}-\u{4dbf}]").expect("cjk")
    });

    let query = norm(query);
    let title_norm = norm(title);
    if query.is_empty() || title_norm.is_empty() {
        return false;
    }
    if query == title_norm {
        return true;
    }
    let title_tokens: Vec<&str> = title_norm.split(' ').collect();
    query.split(' ').all(|token| {
        if CJK.is_match(token) {
            title_norm.contains(token)
        } else {
            title_tokens.contains(&token)
        }
    })
}

/// One search-result row from LRCLIB, before it becomes a candidate.
#[derive(Clone, Debug, Default)]
pub struct LrclibEntry {
    pub id: i64,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: String,
    pub duration_sec: f64,
    pub synced_lyrics: Option<String>,
    pub plain_lyrics: Option<String>,
}

/// Map LRCLIB search results onto candidates.
///
/// By default only entries with synced lyrics qualify (a plain-text row has no
/// timeline and would be dropped by the probe anyway). `keep_plain` keeps them
/// so the rescue chain can split a `中文 - English` title; those rows carry
/// `syncedLyrics = null` and must be filtered again before delivery.
pub fn lrclib_map_entries(entries: &[LrclibEntry], keep_plain: bool) -> Vec<crate::types::Candidate> {
    entries
        .iter()
        .filter(|e| {
            if keep_plain {
                !e.track_name.is_empty()
            } else {
                e.synced_lyrics.is_some()
            }
        })
        .map(|e| crate::types::Candidate {
            source: "lrclib".to_string(),
            id: e.id.to_string(),
            title: e.track_name.clone(),
            artists: Some(
                e.artist_name
                    .split(['\u{3001}', ',', '/', '&'])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            ),
            album: Some(e.album_name.clone()),
            duration_sec: Some(e.duration_sec),
            extra: Some(serde_json::json!({ "syncedLyrics": e.synced_lyrics })),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_related_matches_words_and_cjk_substrings() {
        assert!(amlldb_title_related("晴天", "晴天 (Live)"));
        assert!(amlldb_title_related("Green Light", "Green Light (Live)"));
        assert!(amlldb_title_related("Hush", "Hush My Heart"));
        assert!(!amlldb_title_related("21", "武家坡2021"));
    }
}
