//! Lyrics sources, translation, matching, and configuration.
//!
//! This crate is a 1:1 port of the legacy TypeScript engine
//! (`src/engine/*.ts`). The goldens under
//! `native/tests/fixtures/oracle/engine/` were exported from the shipped
//! JavaScript oracle and pin every branch, so the port is verified against the
//! behaviour that actually shipped rather than against a re-reading of it.
//!
//! Module map (each mirrors one reference file):
//!
//! | module | reference |
//! | --- | --- |
//! | [`parsers`] | `parsers.ts` |
//! | [`decrypt`] | `krc.ts`, `qrc.ts` |
//! | [`match`] | `match.ts` |
//! | [`identity`] | `index.ts` (identity guards) |
//! | [`picker`] | `picker-order.ts` |
//! | [`arbiter`] | `arbiter.ts` |
//! | [`doc`] | `index.ts` (buildDoc/compose) |
//! | [`strings`] | `index.ts` (trackKey/primaryArtist) |
//! | [`crosslang`] | `crosslang.ts` |
//! | [`sources`] | `sources/index.ts` (pure helpers) |
//! | [`translate`] | `translate/index.ts` (predicates) |
//! | [`zhconv`], [`title_bridge`] | `zhconv.ts`, `title-bridge.ts` |

#![forbid(unsafe_code)]

pub mod arbiter;
pub mod crosslang;
pub mod custom_lyrics;
pub mod data;
pub mod decrypt;
pub mod doc;
pub mod hd_cover;
pub mod identity;
pub mod local_folder;
pub mod matching;
pub mod motion_cover;
pub mod net;
pub mod parsers;
pub mod picker;
pub mod providers;
pub mod service;
pub mod sources;
pub mod strings;
pub mod title_bridge;
pub mod translate;
pub mod translate_net;
pub mod types;
pub mod zhconv;

pub use arbiter::{
    GRADE_BOTH, GRADE_PLAIN, GRADE_TRANS, GRADE_WORD, Judgement, grade_of, grade_of_doc, judge,
    judge_with_threshold, should_upgrade, should_upgrade_doc,
};
pub use crosslang::{artist_name_variants, consensus_chinese_artist, norm_txt, parse_chinese_title};
pub use decrypt::{decrypt_krc, krc_translations, triple_des_decrypt};
pub use doc::{
    INSTRUMENTAL_TEXT, RawLyrics, ScoredCandidate, attach_field, attach_words, build_doc, compose,
    instrumental_only, merge_lrc_field, should_fill_gaps, strip_meta_lines, trans_gaps,
};
pub use identity::{artist_ok, duration_ok, identity_gate, identity_plausible, title_ok};
pub use matching::{
    HIGH, LOW, MEDIUM, NO_MATCH, PERFECT, VERY_HIGH, TrackParts, TrackScore, compare_artist,
    compare_duration, compare_name, compare_track, normalize, score_candidate, to_simplified,
};
pub use parsers::{is_placeholder, parse_krc, parse_lrc, parse_lyrics, parse_plain, parse_qrc};
pub use picker::{
    PickerOptions, PickerResult, build_cands, cand_sort, group_label, group_order, kind_of, usable,
};
pub use sources::{LrclibEntry, amlldb_title_related, lrclib_map_entries};
pub use strings::{primary_artist, title_variants, track_key};
pub use translate::{has_foreign_lyrics, needs_translation};
pub use types::{
    BuiltDoc, Candidate, DocLine, ParsedLine, ParsedLyrics, Track, Verified, Word,
};
