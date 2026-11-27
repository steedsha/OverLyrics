//! On-disk configuration with explicit data root.
//!
//! This is a faithful port of the shipped Tauri store
//! (`src-tauri/src/store/mod.rs`): the config is a `serde_json::Value` rather
//! than a struct, so a schema mismatch can never prevent startup; unknown keys
//! survive a round trip; defaults are deep-merged underneath the saved file.
//!
//! Two deliberate changes from the reference implementation, both required by
//! the migration plan:
//!
//! * the data root is an explicit [`DataRoot`] value - nothing here reads
//!   `%APPDATA%` or any other ambient location, so two installs can never
//!   observe each other's state;
//! * [`ConfigStore::patch`] and [`ConfigStore::reset`] are transactional: the
//!   candidate document is persisted first and only then swapped into memory.
//!   A write that cannot land returns [`ConfigError::Write`] and leaves the
//!   in-memory document exactly as it was, so a caller can never believe a
//!   setting was stored when it was not.
//!
//! Every migration below is keyed on a marker field, not on value heuristics:
//! the marker records that the cut has already been made, so a value the user
//! later sets back to the old default is respected instead of being rewritten
//! on every launch. Marker fields ship in `config_defaults.json` with `false`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value};

/// Defaults shipped verbatim from the reference implementation.
const DEFAULTS: &str = include_str!("config_defaults.json");

/// Parsed default configuration document (cached once per process).
pub fn default_config() -> Value {
    static PARSED: OnceLock<Value> = OnceLock::new();
    PARSED
        .get_or_init(|| serde_json::from_str(DEFAULTS).unwrap_or(Value::Null))
        .clone()
}

/// Accent colour used when `display.uiAccentColor` is absent or malformed.
/// Mirrors `DEFAULT_COLOR` in the reference `shared/ui-theme.js`.
const DEFAULT_ACCENT: &str = "#0a84ff";

/// Value recorded in `config.json` after a legacy import.
const LEGACY_TAG: &str = "overlyric@1";

/// Error from a config operation. Every variant means "nothing was applied".
#[derive(Debug)]
pub enum ConfigError {
    /// The directory or file could not be created/written.
    Write(std::io::Error),
    /// The document could not be serialised (should not happen for JSON data).
    Encode(serde_json::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(err) => write!(f, "config write failed: {err}"),
            Self::Encode(err) => write!(f, "config encode failed: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Write(err) => Some(err),
            Self::Encode(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(err: std::io::Error) -> Self {
        Self::Write(err)
    }
}

impl From<serde_json::Error> for ConfigError {
    fn from(err: serde_json::Error) -> Self {
        Self::Encode(err)
    }
}

pub type Result<T> = std::result::Result<T, ConfigError>;

/// An explicit configuration directory.
///
/// The type exists so that "where the config lives" is a value the caller
/// passes in, never a lookup this crate performs. The production app supplies
/// its experiment data root; tests supply a temporary directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataRoot(PathBuf);

impl DataRoot {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl From<PathBuf> for DataRoot {
    fn from(path: PathBuf) -> Self {
        Self(path)
    }
}

/// The loaded configuration document plus its on-disk home.
pub struct ConfigStore {
    root: DataRoot,
    data: Mutex<Value>,
}

impl ConfigStore {
    /// Read `<root>/config.json` (a missing or corrupt file means "fresh
    /// install"), apply defaults and migrations, then write the result back.
    ///
    /// A corrupt file is replaced, but nothing else in the directory is ever
    /// touched - offsets and source memory are separate files owned elsewhere.
    pub fn load(root: &DataRoot) -> Result<Self> {
        let path = root.join("config.json");
        let saved = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or(Value::Null);

        let data = compose(&saved);
        persist(root, &data)?;
        Ok(Self {
            root: root.clone(),
            data: Mutex::new(data),
        })
    }

    pub fn data_dir(&self) -> &Path {
        self.root.as_path()
    }

    pub fn root(&self) -> &DataRoot {
        &self.root
    }

    pub fn get(&self) -> Value {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Value> {
        match self.data.lock() {
            Ok(guard) => guard,
            // A panic while holding the lock cannot leave a half-merged
            // document: every mutation happens on a clone and is swapped in
            // only after the file write succeeded.
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Deep-merge `patch` into the config and persist it.
    ///
    /// Returns the new full document. On a write failure the previous document
    /// is left in place and the error is reported.
    pub fn patch(&self, patch: Value) -> Result<Value> {
        let mut guard = self.lock();
        let mut candidate = guard.clone();
        // A patch that turns a section into a scalar would poison every later
        // read; only object patches for object sections are accepted.
        merge(&mut candidate, sanitize(&patch));
        // Re-run the value normalisations (not the one-shot cuts, whose markers
        // are already spent) so the document never holds a shape the UI cannot
        // render: retired backdrop names, unknown glass renderers, malformed
        // accent colours and ratio-scale match thresholds all land on their
        // shipped value immediately.
        let snapshot = candidate.clone();
        migrate(&mut candidate, &snapshot);
        candidate = scrub(candidate);

        persist(&self.root, &candidate)?;
        *guard = candidate.clone();
        Ok(candidate)
    }

    /// Every business key back to its default, but one-shot migration markers
    /// keep their current value. Wiping the markers would replay them on the
    /// next start and silently undo whatever the user just changed.
    pub fn reset(&self) -> Result<Value> {
        let prev = self.get();
        let mut fresh = compose(&Value::Object(Map::new()));

        for (section, key) in [
            ("window", "topDefaultMigrated"),
            ("display", "oddRowsMigrated"),
            ("display", "anchor42Migrated"),
            ("display", "fontPretendardMigrated"),
            ("display", "feather56Migrated"),
            ("display", "tuning68Migrated"),
            ("display", "empMeasuredMigrated"),
            ("display", "lyricGapUnitMigrated"),
            ("display", "innerGapAnchorMigrated"),
            ("lyrics", "sourcesPruned"),
        ] {
            if let Some(value) = prev.get(section).and_then(|s| s.get(key)) {
                if value.as_bool() == Some(true) || value.as_u64().is_some() {
                    if let Some(target) = fresh.get_mut(section).and_then(|s| s.as_object_mut()) {
                        target.insert(key.to_string(), value.clone());
                    }
                }
            }
        }

        persist(&self.root, &fresh)?;
        *self.lock() = fresh.clone();
        Ok(fresh)
    }
}

/// Write `data` to `<root>/config.json` atomically.
///
/// Write-then-rename: a crash mid-write must not truncate the config. Any
/// failure is returned; the caller decides whether to keep the old document.
fn persist(root: &DataRoot, data: &Value) -> Result<()> {
    std::fs::create_dir_all(root.as_path())?;
    let path = root.join("config.json");
    let tmp = root.join("config.json.tmp");
    let text = serde_json::to_string_pretty(data)?;
    std::fs::write(&tmp, text)?;
    if let Err(err) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConfigError::Write(err));
    }
    Ok(())
}

/// One-way import of the legacy Electron build's `config.json`.
///
/// Reads the legacy file, never writes it, and never shares the directory:
/// both versions may be installed side by side and must keep independent
/// state. Returns `true` when a file was imported.
pub fn import_legacy(dir: &Path, legacy_dir: &Path) -> bool {
    let target = dir.join("config.json");
    if target.exists() {
        return false;
    }
    let source = legacy_dir.join("config.json");
    let Ok(text) = std::fs::read_to_string(&source) else {
        return false;
    };
    let Ok(saved) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    // The marker is part of the data contract: it records that this install
    // was migrated, so a later launch with the same legacy directory still
    // around does not try to import again.
    let mut data = compose(&saved);
    if let Some(object) = data.as_object_mut() {
        object.insert("migratedFrom".into(), Value::String(LEGACY_TAG.into()));
    }
    let Ok(pretty) = serde_json::to_string_pretty(&data) else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    std::fs::write(&target, pretty).is_ok()
}

/// One-way copy of the legacy build's `lyric-offsets.json`.
///
/// The formats are identical and the numbers are real tuning the user did, so
/// this is a byte copy rather than a parse-and-rewrite: re-serialising would
/// round-trip through floats and silently move a 50 ms nudge to 49.999.
pub fn import_legacy_offsets(dir: &Path, legacy_dir: &Path) -> bool {
    let target = dir.join("lyric-offsets.json");
    if target.exists() {
        return false;
    }
    let source = legacy_dir.join("lyric-offsets.json");
    let Ok(text) = std::fs::read_to_string(&source) else {
        return false;
    };
    if serde_json::from_str::<Value>(&text).is_err() {
        return false;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    std::fs::write(&target, text).is_ok()
}

/// Defaults <- saved patch <- migrations. Pure, so tests can drive it directly.
fn compose(saved: &Value) -> Value {
    let defaults: Value = serde_json::from_str(DEFAULTS).unwrap_or(Value::Null);
    let saved = if saved.is_object() {
        saved.clone()
    } else {
        Value::Object(Map::new())
    };

    let mut data = defaults;
    merge(&mut data, sanitize(&saved));
    adopt_type_profiles(&mut data, &saved);
    migrate(&mut data, &saved);
    // `adopt_type_profiles` copies the saved profiles verbatim, so `merge` is
    // not the only door into the document; one scrub at the end closes the rest.
    scrub(data)
}

/// Drop any top-level key that the defaults declare as a section but the saved
/// file turned into a scalar/array. Hand-edited configs otherwise make every
/// later `config.window.x` read throw and the app "silently fails to start".
fn sanitize(saved: &Value) -> Value {
    let defaults: Value = serde_json::from_str(DEFAULTS).unwrap_or(Value::Null);
    let mut out = saved.clone();
    if let (Some(out), Some(defaults)) = (out.as_object_mut(), defaults.as_object()) {
        for (key, value) in defaults {
            if value.is_object() {
                if let Some(v) = out.get(key) {
                    if !v.is_object() {
                        out.remove(key);
                    }
                }
            }
        }
    }
    out
}

/// Type profiles are *diffs over the global display values*, so an existing
/// config keeps its own sparse set and missing profile keys fall back to the
/// user's display section. Only a fresh install gets the full defaults.
fn adopt_type_profiles(data: &mut Value, saved: &Value) {
    let Some(saved_display) = saved.get("display").filter(|d| d.is_object()) else {
        return;
    };
    let profiles = match saved_display.get("typeProfiles") {
        Some(v) if v.is_object() => v.clone(),
        _ => Value::Object(Map::new()),
    };
    // This value is copied straight into the document instead of travelling
    // through `merge`, so it needs the same key scrub.
    let profiles = scrub(profiles);
    if let Some(display) = data.get_mut("display").and_then(|d| d.as_object_mut()) {
        display.insert("typeProfiles".into(), profiles);
    }
}

/// Keys that must never travel through a merge.
///
/// The patch may carry an own `__proto__` member; the reference implementation
/// skips exactly these three keys for the same reason. `serde_json`'s map is
/// not a JavaScript object, but the document is handed back to UI code where
/// `{...section}` and `Object.assign` would hit the prototype setter.
const FORBIDDEN_KEYS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// Deep-merge `src` into `dst`. Objects merge key-wise; arrays and scalars
/// replace. The forbidden key list above is enforced at every level.
pub fn merge(dst: &mut Value, src: Value) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                if FORBIDDEN_KEYS.contains(&k.as_str()) {
                    continue;
                }
                merge(d.entry(k).or_insert(Value::Null), v);
            }
        }
        (d, s) => *d = s,
    }
}

/// Recursively drop the forbidden keys from an owned document.
///
/// `merge` already filters, but a section can also arrive by being *cloned*
/// into the document (type profiles) or by being built by a migration.
fn scrub(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| !FORBIDDEN_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k, scrub(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(scrub).collect()),
        other => other,
    }
}

fn remove_keys(section: Option<&mut Map<String, Value>>, keys: &[&str]) {
    let Some(section) = section else { return };
    for key in keys {
        section.remove(*key);
    }
}

fn as_num(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

fn set_num(section: &mut Map<String, Value>, key: &str, value: f64) {
    section.insert(key.to_string(), Value::from(value));
}

/// `#abc` / `#aabbcc` / `abc` -> lowercase `#aabbcc`; anything else -> `None`.
fn normalize_color(value: &Value) -> Option<String> {
    let text = value.as_str()?.trim();
    let hex = text.strip_prefix('#').unwrap_or(text);
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        3 => {
            let mut out = String::from("#");
            for c in hex.chars() {
                let lower = c.to_ascii_lowercase();
                out.push(lower);
                out.push(lower);
            }
            Some(out)
        }
        6 => Some(format!("#{}", hex.to_ascii_lowercase())),
        _ => None,
    }
}

/// The retired keys that have no reader in the new build.
const DEAD_DISPLAY_KEYS: &[&str] = &[
    // Older naming of the translation / romanization scale knobs.
    "translationScale",
    "romanizationScale",
    "romaScale",
    // Pre-v0.4 layout switches.
    "showTrackInfo",
    "showControls",
    "dimNeighbors",
    "neighborBlur",
    "glow",
    "panelBlur",
    "textAlign",
    "fadeOutEffect",
    "outOfSightEffect",
    "playedOriginalOpacity",
    // Switches replaced by pure sliders.
    "blurEffect",
    "edgeFeatherEffect",
    "glowEffect",
    "glowStyle",
    // Retired backdrop knobs.
    "backdropPulse",
    "backdropFromCover",
    "ambientSpeed",
    "ambientGlow",
    "ambientGrain",
    "panelOpacity",
    "backdropBlur",
    "showBackground",
    "themeFromCover",
    // Retired with the painted backdrops: the library owns the material now.
    "amllCornerGlow",
    "amllSpeed",
    "amllSaturation",
    "amllBrightness",
    "amllContrast",
    "backdropBrightness",
    "backdropSaturate",
    "backdropContrast",
    "backdropSpeed",
    "backdropFps15",
    "backdropAppleMigrated",
    // Fixed at build time / moved into shared constants.
    "dynamicFontSize",
    "originalFontSize",
    "fontScale",
    "lineOverallSpacingFactor",
    "lineInnerSpacingFactor",
    "compactLineOverallSpacingFactor",
    "compactLineInnerSpacingFactor",
    "currentUnplayedOpacity",
    "floatEffect",
    "floatDuration",
    "scaleEffect",
    "outOfSightScale",
    "fullscreenHideCover",
    "compactLeft",
    "rowsInWindow",
    "rowsInFullscreen",
    "coverScale",
    "coverSize",
    // Free-layout era.
    "freeLayout",
    "layoutEdit",
    "metaScale",
    "lyricsModeFontSize",
    "layoutRebuild",
    // Handed to the AMLL renderer.
    "glowStrength",
    "longSyllableScale",
    "pushStrength",
    "empRiseMs",
    "empHoldMs",
    "empFallMs",
    "empGlowRadius",
    "empFlash",
    "pastFadeMs",
    "scrollDuration",
    "scrollEasing",
    "distanceDim",
    "distanceBlur",
    "layoutMode",
];

/// The retired `lyrics` keys.
const DEAD_LYRICS_KEYS: &[&str] = &[
    "enabledProviders",
    "minScore",
    "localLyricsDir",
    "amllEnabled",
    "tlsStrict",
    "autoSearch",
    "offsetMs",
];

/// Config migrations, keyed on marker fields rather than on value heuristics.
///
/// `saved` is the raw on-disk value (before defaults are merged in) on load,
/// and the merged document on a patch. Several cuts must inspect it, because
/// DEFAULTS would otherwise mask "the key was absent".
fn migrate(doc: &mut Value, saved: &Value) {
    let Some(root) = doc.as_object_mut() else {
        return;
    };

    let saved_pruned = saved
        .get("lyrics")
        .and_then(|l| l.get("sourcesPruned"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // ---- M1/M2/M2b - backdrop style and glass renderer --------------------
    if let Some(display) = root.get_mut("display").and_then(|d| d.as_object_mut()) {
        let style = display
            .get("backdropStyle")
            .and_then(Value::as_str)
            .map(str::to_string);
        match style.as_deref() {
            Some("ambient") => {
                display.insert("backdropStyle".into(), "neutral".into());
            }
            Some("acrylic") | Some("amll") | Some("fluid") => {
                display.insert("backdropStyle".into(), "none".into());
            }
            _ => {}
        }

        // M2b - the renderer switch arrived with the library. A missing or
        // unrecognized value takes the shipped css default; 'svg' is only ever
        // written by us and is gated behind the G1 svg checks.
        if !matches!(
            display.get("glassRenderer").and_then(Value::as_str),
            Some("css") | Some("svg")
        ) {
            display.insert("glassRenderer".into(), "css".into());
        }
    }

    // ---- M3..M7 - lyrics section ------------------------------------------
    if let Some(lyrics) = root.get_mut("lyrics").and_then(|l| l.as_object_mut()) {
        // M3 - the Apple source no longer exists.
        if let Some(sources) = lyrics.get_mut("enabledSources").and_then(|s| s.as_object_mut()) {
            sources.remove("apple");
        }

        // M4 - QQ shipped disabled while its pipeline was being fixed, then was
        // re-enabled once it was fixed. Anything older than the second cut gets it.
        if saved_pruned < 3 {
            if let Some(sources) = lyrics.get_mut("enabledSources").and_then(|s| s.as_object_mut()) {
                sources.insert("qq".into(), Value::Bool(true));
            }
            lyrics.insert("sourcesPruned".into(), Value::from(3));
        }

        // M5/M6 - `matchThreshold` used to be a 0..1 ratio. That scale never
        // took effect (0.5 read as 0.5%), so the value is not rescaled: it lands
        // on the current default, which is also one of the dropdown's options.
        if let Some(v) = as_num(lyrics.get("matchThreshold")) {
            if v > 0.0 && v <= 1.0 {
                lyrics.insert("matchThreshold".into(), Value::from(70));
            }
        }
        // M6b - the settings dropdown once wrote the value as a string.
        if let Some(text) = lyrics.get("matchThreshold").and_then(Value::as_str) {
            if let Ok(v) = text.parse::<f64>() {
                if v.is_finite() {
                    lyrics.insert("matchThreshold".into(), Value::from(v.round()));
                }
            }
        }

        remove_keys(Some(lyrics), DEAD_LYRICS_KEYS);
    }

    // ---- dead keys --------------------------------------------------------
    if let Some(display) = root.get_mut("display").and_then(|d| d.as_object_mut()) {
        remove_keys(Some(display), DEAD_DISPLAY_KEYS);

        // Retired per-profile key: cover size is a shared constant now.
        if let Some(profiles) = display.get_mut("typeProfiles").and_then(|p| p.as_object_mut()) {
            for (_, profile) in profiles.iter_mut() {
                if let Some(profile) = profile.as_object_mut() {
                    profile.remove("coverSize");
                }
            }
        }
    }

    // ---- window section ---------------------------------------------------
    if let Some(window) = root.get_mut("window").and_then(|w| w.as_object_mut()) {
        remove_keys(Some(window), &["scale", "clickThrough", "hideWhenPaused"]);
        // M7 - fullscreen never survives a restart; it is a runtime mode.
        if window.get("fullscreen") == Some(&Value::Bool(true)) {
            window.insert("fullscreen".into(), Value::Bool(false));
        }
        // Always-on-top became off by default. Only a saved `true` that the user
        // never revisited is rewritten; the marker keeps the cut one-shot so a
        // later deliberate `true` survives.
        if window.get("topDefaultMigrated") != Some(&Value::Bool(true)) {
            let saved_top = saved
                .get("window")
                .and_then(|w| w.get("alwaysOnTop"))
                .and_then(Value::as_bool);
            if saved_top == Some(true) {
                window.insert("alwaysOnTop".into(), Value::Bool(false));
            }
            window.insert("topDefaultMigrated".into(), Value::Bool(true));
        }
    }

    // A `hotkeys` section has no reader since global shortcuts were retired.
    root.remove("hotkeys");

    // ---- aiLadderOn -------------------------------------------------------
    // Only an explicit saved value may disable the ladder.
    let saved_ai = saved
        .get("lyrics")
        .and_then(|l| l.get("aiLadderOn"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if saved_ai == 0 {
        if let Some(lyrics) = root.get_mut("lyrics").and_then(|l| l.as_object_mut()) {
            lyrics.insert("aiLadderOn".into(), Value::from(1));
        }
    }

    // ---- marker migrations ------------------------------------------------
    // Each cut reads `saved` and fires at most once. Afterwards the user's own
    // choice wins, even when it equals the old default.
    let anchor_default = {
        let d: Value = serde_json::from_str(DEFAULTS).unwrap_or(Value::Null);
        as_num(d.get("display").and_then(|x| x.get("playingLineTopOffset"))).unwrap_or(37.0)
    };

    if let Some(display) = root.get_mut("display").and_then(|d| d.as_object_mut()) {
        let saved_display = saved.get("display").filter(|d| d.is_object());

        // Scroll anchor: the old 50% default was written back by the settings
        // page on every save, so a stored 50 is not a user choice.
        if display.get("anchor42Migrated") != Some(&Value::Bool(true)) {
            if as_num(saved_display.and_then(|d| d.get("playingLineTopOffset"))) == Some(50.0) {
                set_num(display, "playingLineTopOffset", anchor_default);
            }
            display.insert("anchor42Migrated".into(), Value::Bool(true));
        }

        // Embedded font became the default; '' was the old default, not a choice.
        if display.get("fontPretendardMigrated") != Some(&Value::Bool(true)) {
            if saved_display.and_then(|d| d.get("fontFamily")).and_then(Value::as_str) == Some("") {
                display.insert("fontFamily".into(), "Pretendard".into());
            }
            display.insert("fontPretendardMigrated".into(), Value::Bool(true));
        }

        // Edge feather: 16 was the rectangular-era default and is a hard cut at
        // today's font size. 0 stays 0 - that is a deliberate "off".
        if display.get("feather56Migrated") != Some(&Value::Bool(true)) {
            if as_num(saved_display.and_then(|d| d.get("edgeFeather"))) == Some(16.0) {
                set_num(display, "edgeFeather", 56.0);
            }
            display.insert("feather56Migrated".into(), Value::Bool(true));
        }

        // Typography retune: fontSize 90 -> 68, lineGap .2 -> .16, innerGap .2 -> .08.
        if display.get("tuning68Migrated") != Some(&Value::Bool(true)) {
            if let Some(d) = saved_display {
                if as_num(d.get("fontSize")) == Some(90.0) {
                    set_num(display, "fontSize", 68.0);
                }
                if as_num(d.get("lineGap")) == Some(0.2) {
                    set_num(display, "lineGap", 0.16);
                }
                if as_num(d.get("innerGap")) == Some(0.2) {
                    set_num(display, "innerGap", 0.08);
                }
            }
            display.insert("tuning68Migrated".into(), Value::Bool(true));
        }

        // The gap knobs changed unit from "x line height" to absolute em. This
        // cut reads the *merged* value, because the previous cut just rewrote it
        // - reading the raw file would leave every pre-cut config on 0.16/0.08.
        if display.get("lyricGapUnitMigrated") != Some(&Value::Bool(true)) {
            if as_num(display.get("lineGap")) == Some(0.16) {
                set_num(display, "lineGap", 0.0);
            }
            if as_num(display.get("innerGap")) == Some(0.08) {
                set_num(display, "innerGap", 0.3);
            }
            display.insert("lyricGapUnitMigrated".into(), Value::Bool(true));
        }

        // innerGap moved from the (never filled) bottom-line box to the
        // translation element's own margin. Only the 0.3 left by the previous
        // cut is rewritten.
        if display.get("innerGapAnchorMigrated") != Some(&Value::Bool(true)) {
            if display.get("lyricGapUnitMigrated") == Some(&Value::Bool(true))
                && as_num(display.get("innerGap")) == Some(0.3)
            {
                set_num(display, "innerGap", 0.0);
            }
            display.insert("innerGapAnchorMigrated".into(), Value::Bool(true));
        }

        // `empMeasured` (longSyllableScale 50->65 / pushStrength 40->25). Both
        // keys sit in the dead-key list above, but the reference store runs this
        // cut after that cleanup, so a legacy file that still carried the old
        // defaults gets the keys written back with the measured values.
        if display.get("empMeasuredMigrated") != Some(&Value::Bool(true)) {
            if let Some(d) = saved_display {
                if as_num(d.get("longSyllableScale")) == Some(50.0) {
                    set_num(display, "longSyllableScale", 65.0);
                }
                if as_num(d.get("pushStrength")) == Some(40.0) {
                    set_num(display, "pushStrength", 25.0);
                }
            }
            display.insert("empMeasuredMigrated".into(), Value::Bool(true));
        }

        // Accent colour written by hand can be `#ABC`, `abc` or garbage; the CSS
        // variables need one canonical form.
        let normalized =
            normalize_color(display.get("uiAccentColor").unwrap_or(&Value::Null))
                .unwrap_or_else(|| DEFAULT_ACCENT.to_string());
        if display.get("uiAccentColor").and_then(Value::as_str) != Some(normalized.as_str()) {
            display.insert("uiAccentColor".into(), Value::String(normalized));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_normalisation_accepts_only_hex() {
        assert_eq!(normalize_color(&Value::from("#ABC")), Some("#aabbcc".into()));
        assert_eq!(normalize_color(&Value::from("12ef90")), Some("#12ef90".into()));
        assert_eq!(normalize_color(&Value::from("#12345")), None);
        assert_eq!(normalize_color(&Value::from("chartreuse")), None);
        assert_eq!(normalize_color(&Value::Null), None);
    }

    #[test]
    fn merge_replaces_scalars_and_arrays() {
        let mut dst = serde_json::json!({ "a": { "b": 1, "c": 2 }, "list": [1, 2] });
        merge(&mut dst, serde_json::json!({ "a": { "c": 3 }, "list": [9] }));
        assert_eq!(dst["a"]["b"], 1);
        assert_eq!(dst["a"]["c"], 3);
        assert_eq!(dst["list"], serde_json::json!([9]));
    }
}
