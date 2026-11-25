//! Task 11 — configuration compatibility with the shipped Tauri store.
//!
//! `src-tauri/src/store/mod.rs` is the contract: same defaults, same deep-merge
//! semantics, same one-shot marker migrations. These tests drive the port
//! through its public API only.
//!
//! Two rules the plan calls out explicitly are proven here rather than assumed:
//!
//! * the data root is **explicit** — no implicit `%APPDATA%` lookup, and two
//!   roots never see each other's state;
//! * `persist` **reports failure**. A write that could not land must not leave
//!   the in-memory document ahead of the file and must not claim success.

use std::path::PathBuf;

use oly_core::config::{ConfigStore, DataRoot};
use serde_json::{Value, json};

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oly-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn load(dir: &PathBuf) -> ConfigStore {
    ConfigStore::load(&DataRoot::new(dir.clone())).expect("config store should load")
}

/// Put a raw config on disk, as an older build would have left it.
fn write_config(dir: &PathBuf, doc: Value) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_string_pretty(&doc).unwrap(),
    )
    .unwrap();
}

fn num(doc: &Value, section: &str, key: &str) -> f64 {
    doc.get(section)
        .and_then(|s| s.get(key))
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("{section}.{key} is not a number: {doc}"))
}

fn text<'a>(doc: &'a Value, section: &str, key: &str) -> &'a str {
    doc.get(section)
        .and_then(|s| s.get(key))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{section}.{key} is not a string: {doc}"))
}

// ---------------------------------------------------------------- defaults --

#[test]
fn fresh_install_lands_on_the_shipped_business_values() {
    let dir = temp_root("fresh");
    let store = load(&dir);
    let doc = store.get();

    assert_eq!(num(&doc, "window", "width"), 1280.0);
    assert_eq!(num(&doc, "window", "height"), 840.0);
    assert_eq!(num(&doc, "display", "fontSize"), 68.0);
    assert_eq!(num(&doc, "display", "edgeFeather"), 64.0);
    assert_eq!(text(&doc, "display", "fontFamily"), "Pretendard");
    assert_eq!(text(&doc, "display", "uiAccentColor"), "#0a84ff");
    assert_eq!(text(&doc, "display", "backdropStyle"), "neutral");
    assert_eq!(text(&doc, "display", "glassRenderer"), "css");
    assert_eq!(num(&doc, "lyrics", "matchThreshold"), 70.0);
    assert_eq!(num(&doc, "lyrics", "sourcesPruned"), 3.0);
    assert_eq!(num(&doc, "lyrics", "aiLadderOn"), 1.0);

    // One-shot markers are spent on the first pass, so a later deliberate value
    // that happens to equal an old default is never rewritten again.
    for key in [
        "anchor42Migrated",
        "fontPretendardMigrated",
        "feather56Migrated",
        "tuning68Migrated",
        "lyricGapUnitMigrated",
        "innerGapAnchorMigrated",
        "empMeasuredMigrated",
    ] {
        assert_eq!(
            doc.get("display").and_then(|d| d.get(key)),
            Some(&Value::Bool(true)),
            "display.{key} should be spent on first run"
        );
    }
    assert_eq!(
        doc.get("window").and_then(|w| w.get("topDefaultMigrated")),
        Some(&Value::Bool(true))
    );

    // A fresh install keeps the shipped sparse profiles.
    let profiles = doc
        .get("display")
        .and_then(|d| d.get("typeProfiles"))
        .and_then(Value::as_object)
        .expect("typeProfiles section");
    for name in ["cover", "nocover", "mini"] {
        assert!(profiles.contains_key(name), "missing profile {name}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_data_root_is_explicit_and_never_shared() {
    let alpha = temp_root("alpha");
    let beta = temp_root("beta");

    let a = load(&alpha);
    assert_eq!(a.data_dir(), alpha.as_path());
    a.patch(json!({ "display": { "fontSize": 91 } })).unwrap();

    // A second root is a second install: it must not observe alpha's patch.
    let b = load(&beta);
    assert_eq!(num(&b.get(), "display", "fontSize"), 68.0);
    assert!(alpha.join("config.json").is_file());
    assert!(beta.join("config.json").is_file());

    let _ = std::fs::remove_dir_all(&alpha);
    let _ = std::fs::remove_dir_all(&beta);
}

#[test]
fn deep_merge_keeps_untouched_sections() {
    let dir = temp_root("merge");
    let store = load(&dir);
    let doc = store
        .patch(json!({ "display": { "fontSize": 80 } }))
        .unwrap();
    assert_eq!(num(&doc, "display", "fontSize"), 80.0);
    assert_eq!(num(&doc, "display", "edgeFeather"), 64.0);
    assert_eq!(num(&doc, "window", "width"), 1280.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_scalar_where_a_section_lives_falls_back_to_defaults() {
    let dir = temp_root("scalar");
    let store = load(&dir);
    let doc = store
        .patch(json!({ "window": "broken", "display": 7 }))
        .unwrap();
    assert_eq!(num(&doc, "window", "width"), 1280.0);
    assert_eq!(num(&doc, "display", "fontSize"), 68.0);

    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------- resilience --

#[test]
fn corrupt_config_recovers_defaults_without_touching_other_files() {
    let dir = temp_root("corrupt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), "{not json at all").unwrap();
    std::fs::write(dir.join("lyric-offsets.json"), r#"{"version":1,"offsets":{"k":-150}}"#)
        .unwrap();
    std::fs::write(dir.join("source-memory.json"), r#"{"kugou":1}"#).unwrap();

    let store = load(&dir);
    assert_eq!(num(&store.get(), "display", "fontSize"), 68.0);

    // Only the damaged file is replaced; the user's other state is untouched.
    assert_eq!(
        std::fs::read_to_string(dir.join("lyric-offsets.json")).unwrap(),
        r#"{"version":1,"offsets":{"k":-150}}"#
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("source-memory.json")).unwrap(),
        r#"{"kugou":1}"#
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keys_the_build_does_not_know_survive_a_roundtrip() {
    let dir = temp_root("unknown");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_string_pretty(&json!({
            "display": { "fontSize": 72, "customKnob": true },
            "futureSection": { "a": 1 },
        }))
        .unwrap(),
    )
    .unwrap();

    {
        let store = load(&dir);
        let doc = store.get();
        assert_eq!(num(&doc, "display", "fontSize"), 72.0);
        assert_eq!(
            doc.get("display").and_then(|d| d.get("customKnob")),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            doc.get("futureSection").and_then(|s| s.get("a")),
            Some(&Value::from(1))
        );
    }

    let again = load(&dir).get();
    assert_eq!(
        again.get("display").and_then(|d| d.get("customKnob")),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        again.get("futureSection").and_then(|s| s.get("a")),
        Some(&Value::from(1))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------- migrations --

#[test]
fn retired_backdrop_styles_land_on_the_new_surface() {
    let dir = temp_root("backdrop");
    let store = load(&dir);

    let ambient = store
        .patch(json!({ "display": { "backdropStyle": "ambient" } }))
        .unwrap();
    assert_eq!(text(&ambient, "display", "backdropStyle"), "neutral");

    for retired in ["acrylic", "amll", "fluid"] {
        let doc = store
            .patch(json!({ "display": { "backdropStyle": retired } }))
            .unwrap();
        assert_eq!(
            text(&doc, "display", "backdropStyle"),
            "none",
            "{retired} should retire to none"
        );
    }

    for live in ["neutral", "none"] {
        let doc = store
            .patch(json!({ "display": { "backdropStyle": live } }))
            .unwrap();
        assert_eq!(text(&doc, "display", "backdropStyle"), live);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn glass_renderer_keeps_known_values_and_defaults_the_rest() {
    let dir = temp_root("glass");
    let store = load(&dir);
    assert_eq!(text(&store.get(), "display", "glassRenderer"), "css");

    let svg = store
        .patch(json!({ "display": { "glassRenderer": "svg" } }))
        .unwrap();
    assert_eq!(text(&svg, "display", "glassRenderer"), "svg");

    let bogus = store
        .patch(json!({ "display": { "glassRenderer": "webgl" } }))
        .unwrap();
    assert_eq!(text(&bogus, "display", "glassRenderer"), "css");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apple_source_is_dropped_and_qq_is_reenabled_once() {
    let dir = temp_root("sources");
    let store = load(&dir);
    let doc = store
        .patch(json!({
            "lyrics": {
                "enabledSources": { "kugou": true, "apple": true, "qq": false },
                "sourcesPruned": 1
            }
        }))
        .unwrap();
    let sources = doc
        .get("lyrics")
        .and_then(|l| l.get("enabledSources"))
        .and_then(Value::as_object)
        .unwrap();
    assert!(!sources.contains_key("apple"));
    assert_eq!(sources.get("qq"), Some(&Value::Bool(true)));
    assert_eq!(num(&doc, "lyrics", "sourcesPruned"), 3.0);

    // A config that already went through the cut keeps the user's own choice.
    let kept = store
        .patch(json!({
            "lyrics": { "enabledSources": { "qq": false }, "sourcesPruned": 3 }
        }))
        .unwrap();
    assert_eq!(
        kept.get("lyrics")
            .and_then(|l| l.get("enabledSources"))
            .and_then(|s| s.get("qq")),
        Some(&Value::Bool(false))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn match_threshold_ratio_lands_on_a_selectable_value() {
    let dir = temp_root("threshold");
    let store = load(&dir);

    for ratio in [0.5, 1.0, 0.8] {
        let doc = store
            .patch(json!({ "lyrics": { "matchThreshold": ratio } }))
            .unwrap();
        assert_eq!(
            num(&doc, "lyrics", "matchThreshold"),
            70.0,
            "ratio {ratio} should land on the default dropdown option"
        );
    }

    // Percent-scale numbers and numeric strings are kept as numbers.
    let percent = store
        .patch(json!({ "lyrics": { "matchThreshold": 60 } }))
        .unwrap();
    assert_eq!(num(&percent, "lyrics", "matchThreshold"), 60.0);

    let stringy = store
        .patch(json!({ "lyrics": { "matchThreshold": "85" } }))
        .unwrap();
    assert_eq!(num(&stringy, "lyrics", "matchThreshold"), 85.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dead_keys_are_removed_but_live_ones_survive() {
    let dir = temp_root("deadkeys");
    let store = load(&dir);
    let doc = store
        .patch(json!({
            "display": {
                "fontSize": 72,
                "translationScale": 1.2,
                "romanizationScale": 1.1,
                "blurEffect": true,
                "coverScale": 1.1,
                "longSyllableThreshold": 800
            },
            "lyrics": { "enabledProviders": ["a"], "minScore": 3, "localLyricsDir": "x" },
            "window": { "scale": 1.5, "clickThrough": true }
        }))
        .unwrap();

    let display = doc.get("display").and_then(Value::as_object).unwrap();
    for dead in ["translationScale", "romanizationScale", "blurEffect", "coverScale"] {
        assert!(!display.contains_key(dead), "{dead} should be gone");
    }
    assert_eq!(num(&doc, "display", "fontSize"), 72.0);
    assert_eq!(num(&doc, "display", "longSyllableThreshold"), 800.0);

    let lyrics = doc.get("lyrics").and_then(Value::as_object).unwrap();
    for dead in ["enabledProviders", "minScore", "localLyricsDir", "offsetMs"] {
        assert!(!lyrics.contains_key(dead), "lyrics.{dead} should be gone");
    }

    let window = doc.get("window").and_then(Value::as_object).unwrap();
    assert!(!window.contains_key("scale"));
    assert!(!window.contains_key("clickThrough"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fullscreen_is_never_restored_and_hotkeys_section_is_gone() {
    let dir = temp_root("fullscreen");
    let store = load(&dir);
    let doc = store
        .patch(json!({
            "window": { "fullscreen": true },
            "hotkeys": { "toggle": "Ctrl+Alt+L" }
        }))
        .unwrap();
    assert_eq!(
        doc.get("window").and_then(|w| w.get("fullscreen")),
        Some(&Value::Bool(false))
    );
    assert!(!doc.as_object().unwrap().contains_key("hotkeys"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_ladder_is_forced_on_for_every_saved_config() {
    // The reference store reads the saved flag with `if (!savedAiFlag)`, so even
    // a deliberate 0 lands on 1 on the next launch. Reproduce it.
    let dir = temp_root("ailadder");
    let store = load(&dir);
    for saved in [json!({}), json!({ "lyrics": { "aiLadderOn": 0 } })] {
        let doc = store.patch(saved).unwrap();
        assert_eq!(num(&doc, "lyrics", "aiLadderOn"), 1.0);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn marker_migrations_fire_once_and_then_respect_the_user() {
    // A config old enough to carry the retired defaults. The cuts read the raw
    // file, so this must be written to disk before the store loads it.
    let dir = temp_root("markers");
    write_config(
        &dir,
        json!({
            "display": {
                "playingLineTopOffset": 50,
                "fontFamily": "",
                "edgeFeather": 16
            }
        }),
    );
    let doc = load(&dir).get();

    assert_eq!(num(&doc, "display", "playingLineTopOffset"), 37.0);
    assert_eq!(text(&doc, "display", "fontFamily"), "Pretendard");
    assert_eq!(num(&doc, "display", "edgeFeather"), 56.0);
    for key in ["anchor42Migrated", "fontPretendardMigrated", "feather56Migrated"] {
        assert_eq!(
            doc.get("display").and_then(|d| d.get(key)),
            Some(&Value::Bool(true)),
            "display.{key} should be spent after the cut"
        );
    }

    // Once spent, a deliberate choice that equals the old default survives.
    let store = load(&dir);
    let user_choice = store
        .patch(json!({ "display": { "playingLineTopOffset": 50 } }))
        .unwrap();
    assert_eq!(num(&user_choice, "display", "playingLineTopOffset"), 50.0);

    let kept = store
        .patch(json!({ "display": { "fontFamily": "Segoe UI", "edgeFeather": 0 } }))
        .unwrap();
    assert_eq!(text(&kept, "display", "fontFamily"), "Segoe UI");
    assert_eq!(num(&kept, "display", "edgeFeather"), 0.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_gap_unit_cut_reads_what_the_previous_cut_wrote() {
    // ⑫ rewrites .2 -> .16/.08 and ⑯ then reads the *merged* value, so a config
    // carrying the old pair must reach 0/0. Only a raw file exercises the chain.
    let dir = temp_root("gaps");
    write_config(&dir, json!({ "display": { "lineGap": 0.2, "innerGap": 0.2 } }));
    let doc = load(&dir).get();
    assert_eq!(num(&doc, "display", "lineGap"), 0.0);
    assert_eq!(num(&doc, "display", "innerGap"), 0.0);

    // A user who later picks the old-marked 0.3 keeps it.
    let store = load(&dir);
    let kept = store
        .patch(json!({ "display": { "innerGap": 0.3 } }))
        .unwrap();
    assert_eq!(num(&kept, "display", "innerGap"), 0.3);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn accent_colour_is_normalized_or_replaced() {
    let dir = temp_root("accent");
    let store = load(&dir);
    let cases = [
        ("#ABC", "#aabbcc"),
        ("12ef90", "#12ef90"),
        ("chartreuse", "#0a84ff"),
    ];
    for (written, expected) in cases {
        let doc = store
            .patch(json!({ "display": { "uiAccentColor": written } }))
            .unwrap();
        assert_eq!(text(&doc, "display", "uiAccentColor"), expected);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ persistence --

#[test]
fn patch_persists_and_reloads() {
    let dir = temp_root("patch");
    {
        let store = load(&dir);
        store
            .patch(json!({ "display": { "fontSize": 81 }, "lyrics": { "matchThreshold": 90 } }))
            .unwrap();
    }
    let again = load(&dir);
    assert_eq!(num(&again.get(), "display", "fontSize"), 81.0);
    assert_eq!(num(&again.get(), "lyrics", "matchThreshold"), 90.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reset_restores_defaults_but_keeps_the_markers() {
    let dir = temp_root("reset");
    let store = load(&dir);
    store
        .patch(json!({
            "display": { "fontSize": 90, "anchor42Migrated": true, "feather56Migrated": true },
            "lyrics": { "aiLadderOn": 1, "sourcesPruned": 3 }
        }))
        .unwrap();

    let fresh = store.reset().unwrap();
    assert_eq!(num(&fresh, "display", "fontSize"), 68.0);
    assert_eq!(
        fresh.get("display").and_then(|d| d.get("anchor42Migrated")),
        Some(&Value::Bool(true))
    );
    assert_eq!(num(&fresh, "lyrics", "sourcesPruned"), 3.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn patches_drop_forbidden_keys_at_every_level() {
    let dir = temp_root("proto");
    let store = load(&dir);
    let out = store
        .patch(json!({
            "__proto__": { "polluted": 1 },
            "constructor": { "prototype": { "polluted": 1 } },
            "prototype": { "polluted": 1 },
            "display": {
                "fontSize": 91,
                "typeProfiles": { "mini": { "__proto__": { "polluted": 1 }, "fontSize": 60 } }
            }
        }))
        .unwrap();

    assert_eq!(num(&out, "display", "fontSize"), 91.0);
    for key in ["__proto__", "constructor", "prototype"] {
        assert!(!out.as_object().unwrap().contains_key(key), "root kept {key}");
    }
    let mini = out
        .get("display")
        .and_then(|d| d.get("typeProfiles"))
        .and_then(|p| p.get("mini"))
        .and_then(Value::as_object)
        .unwrap();
    assert!(!mini.contains_key("__proto__"));
    assert_eq!(mini.get("fontSize").and_then(Value::as_f64), Some(60.0));

    // …and the file on disk is clean too.
    drop(store);
    let reloaded = load(&dir).get();
    assert!(!reloaded.as_object().unwrap().contains_key("__proto__"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_write_is_reported_and_memory_never_runs_ahead_of_disk() {
    let dir = temp_root("persist-fail");
    let store = load(&dir);

    // Occupy the temp path with a directory: the atomic write cannot land.
    let blocker = dir.join("config.json.tmp");
    std::fs::create_dir_all(&blocker).unwrap();

    let err = store.patch(json!({ "display": { "fontSize": 99 } }));
    assert!(err.is_err(), "a write that cannot land must not report success");
    assert_eq!(
        num(&store.get(), "display", "fontSize"),
        68.0,
        "rejected patch must not be visible in memory"
    );

    std::fs::remove_dir_all(&blocker).unwrap();
    store.patch(json!({ "display": { "fontSize": 99 } })).unwrap();
    assert_eq!(num(&store.get(), "display", "fontSize"), 99.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editing_one_type_profile_does_not_overwrite_the_others() {
    let dir = temp_root("profiles");
    let store = load(&dir);

    store
        .patch(json!({ "display": { "typeProfiles": { "cover": { "fontSize": 95 } } } }))
        .unwrap();
    let doc = store
        .patch(json!({ "display": { "typeProfiles": { "mini": { "fontSize": 60 } } } }))
        .unwrap();

    let profiles = doc
        .get("display")
        .and_then(|d| d.get("typeProfiles"))
        .and_then(Value::as_object)
        .unwrap();
    assert_eq!(
        profiles.get("cover").and_then(|p| p.get("fontSize")).and_then(Value::as_f64),
        Some(95.0)
    );
    assert_eq!(
        profiles.get("mini").and_then(|p| p.get("fontSize")).and_then(Value::as_f64),
        Some(60.0)
    );
    assert!(
        profiles.contains_key("nocover"),
        "an untouched profile must survive the other two edits"
    );

    let reloaded = load(&dir).get();
    let profiles = reloaded
        .get("display")
        .and_then(|d| d.get("typeProfiles"))
        .and_then(Value::as_object)
        .unwrap();
    assert_eq!(
        profiles.get("cover").and_then(|p| p.get("fontSize")).and_then(Value::as_f64),
        Some(95.0)
    );
    assert_eq!(
        profiles.get("mini").and_then(|p| p.get("fontSize")).and_then(Value::as_f64),
        Some(60.0)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// --------------------------------------------------------- legacy import --

#[test]
fn legacy_import_reads_the_old_file_and_never_writes_it() {
    let base = temp_root("legacy");
    let legacy = base.join("OverLyrics");
    let fresh = base.join("OverLyricsLite");
    std::fs::create_dir_all(&legacy).unwrap();
    let original = json!({
        "display": { "backdropStyle": "acrylic", "fontSize": 90 },
        "lyrics": {
            "matchThreshold": "0.8",
            "enabledSources": { "apple": true, "qq": true }
        }
    });
    let text = serde_json::to_string_pretty(&original).unwrap();
    std::fs::write(legacy.join("config.json"), &text).unwrap();

    assert!(oly_core::config::import_legacy(&fresh, &legacy));
    let imported = load(&fresh).get();
    assert_eq!(text_of(&imported, "display", "backdropStyle"), "none");
    assert_eq!(num(&imported, "display", "fontSize"), 68.0);
    assert_eq!(num(&imported, "lyrics", "matchThreshold"), 70.0);
    assert!(
        !imported
            .get("lyrics")
            .and_then(|l| l.get("enabledSources"))
            .and_then(Value::as_object)
            .unwrap()
            .contains_key("apple")
    );
    assert_eq!(
        imported.get("migratedFrom"),
        Some(&Value::String("overlyric@1".into()))
    );

    // The legacy file is untouched and a second import is a no-op.
    assert_eq!(std::fs::read_to_string(legacy.join("config.json")).unwrap(), text);
    assert!(!oly_core::config::import_legacy(&fresh, &legacy));

    let _ = std::fs::remove_dir_all(&base);
}

fn text_of<'a>(doc: &'a Value, section: &str, key: &str) -> &'a str {
    doc.get(section)
        .and_then(|s| s.get(key))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{section}.{key} missing"))
}

#[test]
fn legacy_import_ignores_a_missing_or_broken_source() {
    let base = temp_root("nolegacy");
    let legacy = base.join("OverLyrics");
    let fresh = base.join("OverLyricsLite");

    assert!(!oly_core::config::import_legacy(&fresh, &legacy));
    assert!(!fresh.join("config.json").exists());

    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("config.json"), "{not json").unwrap();
    assert!(!oly_core::config::import_legacy(&fresh, &legacy));
    assert!(!fresh.join("config.json").exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn legacy_offsets_are_copied_byte_for_byte_and_never_overwrite() {
    let base = temp_root("offsets");
    let legacy = base.join("OverLyrics");
    let fresh = base.join("OverLyricsLite");
    std::fs::create_dir_all(&legacy).unwrap();

    // Written exactly as `offsets.ts` serialises it: the scope key is
    // `JSON.stringify([...])`, so quotes are escaped and \u0001 stays literal.
    let text =
        r#"{"version":1,"offsets":{"[\"song\",\"lustforlife\u0001lanadelrey\",\"kugou\"]":-150}}"#;
    std::fs::write(legacy.join("lyric-offsets.json"), text).unwrap();

    assert!(oly_core::config::import_legacy_offsets(&fresh, &legacy));
    assert_eq!(
        std::fs::read_to_string(fresh.join("lyric-offsets.json")).unwrap(),
        text
    );
    assert_eq!(
        std::fs::read_to_string(legacy.join("lyric-offsets.json")).unwrap(),
        text,
        "the import may only read the source"
    );

    // A later user edit in the new build must not be resurrected over.
    std::fs::write(fresh.join("lyric-offsets.json"), r#"{"offsets":{"kept":1}}"#).unwrap();
    assert!(!oly_core::config::import_legacy_offsets(&fresh, &legacy));
    assert_eq!(
        std::fs::read_to_string(fresh.join("lyric-offsets.json")).unwrap(),
        r#"{"offsets":{"kept":1}}"#
    );

    let _ = std::fs::remove_dir_all(&base);
}
