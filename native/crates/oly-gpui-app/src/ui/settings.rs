//! The lyric-layout drawer: per-shape profile stores, effective-value
//! resolution, and the 120 ms slider settle.
//!
//! Legacy `DrawerWindow.tsx` semantics, kept exactly:
//!
//! * Three shape stores. `profileKeyFor(mode, showCover)` is the legacy
//!   `modeKeyOf` rule: everything that is not the wide window shares the
//!   `mini` store; the wide window splits into `cover` / `nocover`.
//! * Every control writes one key into `display.typeProfiles[<shape>]`. The
//!   lyric window merges that diff over `display` when it reads its config,
//!   so the drawer never owns a second copy of the settings.
//! * Effective values resolve profile store first, then the base display
//!   section, then the caller's fallback; a value of the wrong type falls
//!   back too (the legacy `readNum`/`readStr`/`readBool` type checks).
//! * `playingLineTopOffset` is special: its read bypasses the base section
//!   entirely. Until the profile store sets it, the row shows 自动 and the
//!   fallback is the per-shape 42 / 50 / 42 (`PTO_FALLBACK`).
//! * Sliders settle: the drag is a local draft and the last value commits
//!   120 ms after the last move (the drawer is meant to be dragged while
//!   watching the lyrics; the round trip must stay under the eye's threshold
//!   for "not live"). One legacy quirk is preserved: when a config broadcast
//!   changes a slider's value mid-drag, the pending commit is hijacked to the
//!   new value, because the sync effect overwrites `latest.current` too.
//! * Switches, segments and selects write immediately; the fold state is
//!   session-only and every fold starts open.
//!
//! The widget is a pure state machine; GPUI event wiring lives with the app
//! shell (Task 10).

use std::collections::BTreeMap;

use super::controls::Mode;

/// The three shape stores (`ProfileKey`). `cover`/`nocover` only exist under
/// the wide shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProfileKey {
    Cover,
    Nocover,
    Mini,
}

impl ProfileKey {
    /// The store key inside `display.typeProfiles`.
    pub fn key(self) -> &'static str {
        match self {
            ProfileKey::Cover => "cover",
            ProfileKey::Nocover => "nocover",
            ProfileKey::Mini => "mini",
        }
    }

    /// The legacy `PROFILE_LABEL` string shown in the drawer subtitle.
    pub fn label(self) -> &'static str {
        match self {
            ProfileKey::Cover => "窗口 / 全屏",
            ProfileKey::Nocover => "无封面",
            ProfileKey::Mini => "迷你歌词",
        }
    }
}

/// Legacy `modeKeyOf`: not `window` is always the `mini` store — the three
/// strips share one layout, and the reference build wrote them to a single
/// store for exactly that reason.
pub fn profile_key_for(mode: Mode, show_cover: bool) -> ProfileKey {
    if mode != Mode::Window {
        return ProfileKey::Mini;
    }
    if show_cover {
        ProfileKey::Cover
    } else {
        ProfileKey::Nocover
    }
}

/// The value the reference build shows when `playingLineTopOffset` was never
/// set, per shape store.
pub fn pto_fallback(key: ProfileKey) -> f64 {
    match key {
        ProfileKey::Cover => 42.0,
        ProfileKey::Nocover => 50.0,
        ProfileKey::Mini => 42.0,
    }
}

/// The drawer's two panes (`pane === 'type' | 'fx'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// 歌词设置 — layout, sizes and fonts.
    Type,
    /// 歌词效果 — word brightening, line layering and motion.
    Fx,
}

impl Pane {
    pub fn label(self) -> &'static str {
        match self {
            Pane::Type => "歌词设置",
            Pane::Fx => "歌词效果",
        }
    }
}

/// The fold sections and their pane, in legacy order.
pub const FOLDS: &[(&str, Pane)] = &[
    ("layout", Pane::Type),
    ("type", Pane::Type),
    ("font", Pane::Type),
    ("word", Pane::Fx),
    ("layer", Pane::Fx),
    ("motion", Pane::Fx),
];

/// A slider row's static bounds (`min`/`max`/`step`/`unit`/`digits` and the
/// `readNum` fallback). `playingLineTopOffset` ignores `fallback` — its value
/// comes from [`pto_fallback`] while the store has not set it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliderSpec {
    pub pane: Pane,
    pub fold: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub unit: &'static str,
    pub digits: u8,
    pub fallback: f64,
}

impl SliderSpec {
    /// The number of decimal places in the step, so snapped values can be
    /// rebuilt without binary noise (`0.02 * 45 = 0.9000000000000001`).
    fn step_digits(&self) -> i32 {
        let mut digits = 0;
        let mut s = self.step;
        while digits < 8 && (s - s.round()).abs() > 1e-9 {
            s *= 10.0;
            digits += 1;
        }
        digits
    }
}

const fn slider(
    pane: Pane,
    fold: &'static str,
    key: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    step: f64,
    unit: &'static str,
    digits: u8,
    fallback: f64,
) -> SliderSpec {
    SliderSpec {
        pane,
        fold,
        key,
        label,
        min,
        max,
        step,
        unit,
        digits,
        fallback,
    }
}

/// Every slider row, in legacy order. The bounds are the reference build's
/// contract: tests assert each one.
pub const SLIDERS: &[SliderSpec] = &[
    slider(Pane::Type, "type", "playingLineTopOffset", "当前行位置", 15.0, 85.0, 1.0, "%", 0, 0.0),
    slider(Pane::Type, "type", "fontSize", "原文字号", 50.0, 200.0, 1.0, "%", 0, 100.0),
    slider(Pane::Type, "type", "transFontSize", "译文字号", 50.0, 200.0, 1.0, "%", 0, 100.0),
    slider(Pane::Type, "type", "lineGap", "行间距", -0.6, 1.5, 0.02, "em", 2, 0.0),
    slider(Pane::Type, "type", "innerGap", "原文与译文间距", 0.0, 1.2, 0.02, "em", 2, 0.3),
    slider(Pane::Type, "type", "romanFontScale", "注音字号比例", 20.0, 150.0, 5.0, "%", 0, 50.0),
    slider(Pane::Type, "font", "fontWeight", "原文字重", 300.0, 900.0, 10.0, "", 0, 800.0),
    slider(Pane::Type, "font", "transFontWeight", "译文字重", 300.0, 900.0, 50.0, "", 0, 500.0),
    slider(Pane::Type, "font", "letterSpacing", "字距", -0.02, 0.1, 0.005, "em", 3, 0.005),
    slider(Pane::Fx, "word", "unplayedOriginalOpacity", "未唱部分亮度", 5.0, 100.0, 5.0, "%", 0, 60.0),
    slider(Pane::Fx, "word", "brightAlpha", "已唱部分亮度", 30.0, 100.0, 5.0, "%", 0, 100.0),
    slider(Pane::Fx, "layer", "translatedLyricsOpacity", "非当前行译文亮度", 10.0, 100.0, 5.0, "%", 0, 60.0),
    slider(Pane::Fx, "layer", "transCurrentAlpha", "当前行译文亮度", 20.0, 100.0, 5.0, "%", 0, 85.0),
    slider(Pane::Fx, "layer", "bgLineScale", "和声行缩放", 30.0, 150.0, 5.0, "%", 0, 70.0),
    slider(Pane::Fx, "layer", "bgLineOpacity", "和声行亮度", 0.0, 100.0, 5.0, "%", 0, 40.0),
    slider(Pane::Fx, "layer", "idleLineOpacity", "非当前行亮度", 5.0, 100.0, 5.0, "%", 0, 20.0),
];

/// The switch rows: `(pane, fold, key, legacy default)`.
pub const SWITCHES: &[(Pane, &str, &str, bool)] = &[
    (Pane::Type, "layout", "showTranslation", true),
    (Pane::Type, "layout", "showRomanization", false),
    (Pane::Fx, "layer", "blurMax", false),
    (Pane::Fx, "layer", "enableScale", true),
    (Pane::Fx, "layer", "hidePassedLines", false),
    (Pane::Fx, "motion", "enableSpring", true),
];

/// The segment rows: `(pane, fold, key, default, [(value, label)])`.
pub const SEGMENTS: &[(Pane, &str, &str, &str, &[(&str, &str)])] = &[
    (
        Pane::Type,
        "layout",
        "align",
        "left",
        &[("left", "左对齐"), ("right", "右对齐")],
    ),
    (
        Pane::Type,
        "layout",
        "lineBreakMode",
        "natural",
        &[("natural", "自然折行"), ("balanced", "均衡分行")],
    ),
    (
        Pane::Fx,
        "motion",
        "springFeel",
        "standard",
        &[("lively", "灵动"), ("standard", "标准"), ("calm", "沉稳")],
    ),
];

/// 歌词字体 options (the legacy `FONT_OPTIONS` array).
pub const FONT_OPTIONS: &[&str] = &[
    "Pretendard",
    "檎风黑体 CHS",
    "微软雅黑",
    "宋体",
    "黑体",
    "楷体",
    "等线",
    "Consolas",
];

/// 译文字体 options; the empty string renders as 继承 (the legacy
/// `SelectRow` maps `option||'继承'`).
pub const TRANS_FONT_OPTIONS: &[&str] = &[
    "",
    "檎风黑体 CHS",
    "微软雅黑",
    "宋体",
    "黑体",
    "楷体",
    "仿宋",
    "等线",
];

/// The display placeholder for the empty translation font.
pub const TRANS_FONT_INHERIT_LABEL: &str = "继承";

/// One config value, as the drawer can read it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Num(f64),
    Str(String),
}

/// The slice of config the drawer reads and writes: the base display section
/// and the per-shape profile stores.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayConfig {
    /// `display.<key>` — the base section.
    pub base: BTreeMap<String, Value>,
    /// `display.typeProfiles[<shape>][<key>]`, keyed by the store name.
    pub profiles: BTreeMap<String, BTreeMap<String, Value>>,
}

/// Effective value: the shape store wins, then the base section.
pub fn pick(config: &DisplayConfig, profile: ProfileKey, key: &str) -> Option<Value> {
    if let Some(store) = config.profiles.get(profile.key()) {
        if let Some(value) = store.get(key) {
            return Some(value.clone());
        }
    }
    config.base.get(key).cloned()
}

/// The legacy `readNum`: a finite number or the fallback.
pub fn read_num(config: &DisplayConfig, profile: ProfileKey, key: &str, fallback: f64) -> f64 {
    match pick(config, profile, key) {
        Some(Value::Num(value)) if value.is_finite() => value,
        _ => fallback,
    }
}

/// The legacy `readStr`: a string or the fallback.
pub fn read_str<'a>(config: &DisplayConfig, profile: ProfileKey, key: &str, fallback: &'a str) -> String {
    match pick(config, profile, key) {
        Some(Value::Str(value)) => value,
        _ => fallback.to_string(),
    }
}

/// The legacy `readBool`: a boolean or the fallback.
pub fn read_bool(config: &DisplayConfig, profile: ProfileKey, key: &str, fallback: bool) -> bool {
    match pick(config, profile, key) {
        Some(Value::Bool(value)) => value,
        _ => fallback,
    }
}

/// `playingLineTopOffset` reads the profile store only: `(value, auto)`.
/// `auto` is true until the store sets the key, and the value is then the
/// per-shape fallback — never the base display section.
pub fn playing_line_top_offset(config: &DisplayConfig, profile: ProfileKey) -> (f64, bool) {
    let own = config
        .profiles
        .get(profile.key())
        .and_then(|store| store.get("playingLineTopOffset"));
    match own {
        Some(Value::Num(value)) if value.is_finite() => (*value, false),
        _ => (pto_fallback(profile), true),
    }
}

/// A write the caller must deliver with `ConfigStore::patch`.
#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    pub profile: ProfileKey,
    pub key: String,
    pub value: Value,
}

impl Write {
    /// Lands the write in the profile store, leaving the base section alone.
    pub fn apply(&self, config: &mut DisplayConfig) {
        config
            .profiles
            .entry(self.profile.key().to_string())
            .or_default()
            .insert(self.profile_key_path(), self.value.clone());
    }

    fn profile_key_path(&self) -> String {
        self.key.clone()
    }
}

/// The numeric readout: integer sliders round, digitised sliders keep fixed
/// decimals, and the unit always follows (`Math.round` / `toFixed`).
pub fn format_value(value: f64, digits: u8, unit: &str) -> String {
    if digits == 0 {
        format!("{}{}", value.round() as i64, unit)
    } else {
        format!("{:.*}{}", digits as usize, value, unit)
    }
}

#[derive(Debug)]
struct SliderDraft {
    /// The value the slider currently shows.
    draft: f64,
    /// The value scheduled for a settle commit, if any.
    pending: Option<f64>,
    /// When the pending commit fires (last move + settle delay).
    pending_at_ms: Option<f64>,
}

/// The settle delay for one slider drag, ms. 120 rather than the settings
/// window's 220: the drawer is meant to be dragged while watching the lyrics.
pub const SETTLE_MS: f64 = 120.0;

/// One drawer window.
#[derive(Debug)]
pub struct DrawerState {
    pub profile: ProfileKey,
    pub pane: Pane,
    pub loaded: bool,
    folds: BTreeMap<String, bool>,
    drafts: BTreeMap<String, SliderDraft>,
}

impl DrawerState {
    /// Opens the drawer. The host caches the shape, so a drawer that mounts
    /// after the switch still reads the right store; `win:mode` updates arrive
    /// through [`DrawerState::set_profile`].
    pub fn open(profile: ProfileKey) -> Self {
        let mut folds = BTreeMap::new();
        for (id, _) in FOLDS {
            folds.insert((*id).to_string(), true);
        }
        Self {
            profile,
            pane: Pane::Type,
            loaded: false,
            folds,
            drafts: BTreeMap::new(),
        }
    }

    /// The host broadcast a mode (or show-cover) change: the store switches.
    pub fn set_profile(&mut self, profile: ProfileKey) {
        self.profile = profile;
    }

    pub fn set_pane(&mut self, pane: Pane) {
        self.pane = pane;
    }

    /// The config read resolved (`load()` or the first `config:changed`).
    pub fn set_loaded(&mut self) {
        self.loaded = true;
    }

    pub fn fold_open(&self, id: &str) -> bool {
        self.folds.get(id).copied().unwrap_or(true)
    }

    /// Session-only fold toggle.
    pub fn toggle_fold(&mut self, id: &str) {
        let open = self.fold_open(id);
        self.folds.insert(id.to_string(), !open);
    }

    /// The value the slider shows: the in-flight draft if one exists, else
    /// the resolved config value.
    pub fn slider_draft(&self, config: &DisplayConfig, spec: &SliderSpec) -> f64 {
        if spec.key == "playingLineTopOffset" {
            let (value, auto) = playing_line_top_offset(config, self.profile);
            if auto {
                if let Some(d) = self.drafts.get(spec.key) {
                    return d.draft;
                }
                return value;
            }
        }
        if let Some(d) = self.drafts.get(spec.key) {
            return d.draft;
        }
        if spec.key == "playingLineTopOffset" {
            playing_line_top_offset(config, self.profile).0
        } else {
            read_num(config, self.profile, spec.key, spec.fallback)
        }
    }

    /// Whether the read-out says 自动 (the profile store has not set the key).
    pub fn slider_auto(&self, config: &DisplayConfig, spec: &SliderSpec) -> bool {
        spec.key == "playingLineTopOffset" && playing_line_top_offset(config, self.profile).1
    }

    /// A slider move: local draft, commit scheduled after the settle delay.
    /// Like the volume panel, a slide never writes directly.
    pub fn on_slide(&mut self, spec: &SliderSpec, value: f64, now_ms: f64) {
        let value = snap(value, spec);
        let entry = self.drafts.entry(spec.key.to_string()).or_insert(SliderDraft {
            draft: value,
            pending: None,
            pending_at_ms: None,
        });
        entry.draft = value;
        entry.pending = Some(value);
        entry.pending_at_ms = Some(now_ms + SETTLE_MS);
    }

    /// A config broadcast for one key. The legacy sync effect resets both the
    /// draft and `latest.current`, so an external change mid-drag hijacks the
    /// pending commit to the new value. Preserved verbatim.
    pub fn sync_value(&mut self, key: &str, value: f64) {
        if let Some(entry) = self.drafts.get_mut(key) {
            entry.draft = value;
            if entry.pending.is_some() {
                entry.pending = Some(value);
            }
        }
    }

    /// Fires the settle commits whose time has come.
    pub fn tick(&mut self, now_ms: f64) -> Vec<Write> {
        let mut writes = Vec::new();
        let keys: Vec<String> = self.drafts.keys().cloned().collect();
        for key in keys {
            let due = {
                let entry = &self.drafts[&key];
                match (entry.pending, entry.pending_at_ms) {
                    (Some(_), Some(at)) => now_ms + 1e-9 >= at,
                    _ => false,
                }
            };
            if due {
                let entry = self.drafts.get_mut(&key).unwrap();
                let value = entry.pending.take().unwrap();
                entry.pending_at_ms = None;
                writes.push(Write {
                    profile: self.profile,
                    key,
                    value: Value::Num(value),
                });
            }
        }
        writes
    }

    /// A switch flip: immediate write.
    pub fn set_switch(&mut self, key: &str, checked: bool) -> Write {
        Write {
            profile: self.profile,
            key: key.to_string(),
            value: Value::Bool(checked),
        }
    }

    /// A segment pick: immediate write.
    pub fn set_segment(&mut self, key: &str, value: &str) -> Write {
        Write {
            profile: self.profile,
            key: key.to_string(),
            value: Value::Str(value.to_string()),
        }
    }

    /// A font select: immediate write (the empty translation font stores
    /// an empty string, which the lyric window inherits).
    pub fn set_select(&mut self, key: &str, value: &str) -> Write {
        Write {
            profile: self.profile,
            key: key.to_string(),
            value: Value::Str(value.to_string()),
        }
    }
}

/// Snap a raw slider position to the row's step and clamp it into range
/// (what the DOM range input does natively).
///
/// The DOM input snaps the step *count* in exact decimal arithmetic: 0.31 on a
/// 0.02-step row from -0.6 counts (0.31+0.6)/0.02 = 45.5 steps and rounds to
/// 46 → 0.32, not 0.30. Naive f64 division gives 45.499999… and lands one
/// step low, so the arithmetic runs on the row's fixed-point scale where the
/// count is integer-exact.
fn snap(value: f64, spec: &SliderSpec) -> f64 {
    let value = value.clamp(spec.min, spec.max);
    if spec.step <= 0.0 || (spec.max - spec.min) <= spec.step {
        return value;
    }
    let scale = 10f64.powi(i32::from(spec.step_digits()));
    let scaled = (value * scale).round();
    let min = (spec.min * scale).round();
    let step = (spec.step * scale).round();
    let count = ((scaled - min) / step).round();
    let snapped = (min + count * step) / scale;
    snapped.clamp(spec.min, spec.max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DisplayConfig {
        let mut base = BTreeMap::new();
        base.insert("fontSize".to_string(), Value::Num(120.0));
        base.insert("showTranslation".to_string(), Value::Bool(false));
        base.insert("fontFamily".to_string(), Value::Str("黑体".to_string()));
        let mut cover = BTreeMap::new();
        cover.insert("fontSize".to_string(), Value::Num(95.0));
        let mut profiles = BTreeMap::new();
        profiles.insert("cover".to_string(), cover);
        DisplayConfig {
            base,
            profiles,
        }
    }

    #[test]
    fn the_profile_key_follows_the_host_mode() {
        assert_eq!(profile_key_for(Mode::Mini, true), ProfileKey::Mini);
        assert_eq!(profile_key_for(Mode::Album, true), ProfileKey::Mini);
        assert_eq!(profile_key_for(Mode::Lyrics, false), ProfileKey::Mini);
        assert_eq!(profile_key_for(Mode::Window, true), ProfileKey::Cover);
        assert_eq!(profile_key_for(Mode::Window, false), ProfileKey::Nocover);
    }

    #[test]
    fn the_profile_labels_match_the_legacy_strings() {
        assert_eq!(ProfileKey::Cover.label(), "窗口 / 全屏");
        assert_eq!(ProfileKey::Nocover.label(), "无封面");
        assert_eq!(ProfileKey::Mini.label(), "迷你歌词");
    }

    #[test]
    fn effective_values_prefer_the_profile_store_then_the_base_section() {
        let config = config();
        // The cover store wins.
        assert_eq!(read_num(&config, ProfileKey::Cover, "fontSize", 100.0), 95.0);
        // Mini has no store entry: the base section answers.
        assert_eq!(read_num(&config, ProfileKey::Mini, "fontSize", 100.0), 120.0);
        // Neither has it: the caller's fallback.
        assert_eq!(read_num(&config, ProfileKey::Cover, "lineGap", 0.0), 0.0);
        // A value of the wrong type falls back too (the legacy type checks).
        assert_eq!(read_bool(&config, ProfileKey::Cover, "fontSize", true), true);
        assert_eq!(
            read_str(&config, ProfileKey::Cover, "fontSize", "Pretendard"),
            "Pretendard"
        );
        assert_eq!(read_bool(&config, ProfileKey::Mini, "showTranslation", true), false);
        assert_eq!(read_str(&config, ProfileKey::Mini, "fontFamily", "Pretendard"), "黑体");
    }

    #[test]
    fn playing_line_top_offset_is_auto_until_written() {
        let config = config();
        assert_eq!(playing_line_top_offset(&config, ProfileKey::Cover), (42.0, true));
        assert_eq!(playing_line_top_offset(&config, ProfileKey::Nocover), (50.0, true));
        assert_eq!(playing_line_top_offset(&config, ProfileKey::Mini), (42.0, true));
        // The base section is bypassed: even with `playingLineTopOffset` in
        // `base`, the row stays 自动 until the profile store sets it.
        let mut with_base = config.clone();
        with_base
            .base
            .insert("playingLineTopOffset".to_string(), Value::Num(60.0));
        assert_eq!(playing_line_top_offset(&with_base, ProfileKey::Mini), (42.0, true));
        // One write flips the row off 自动 for that shape only.
        let write = Write {
            profile: ProfileKey::Cover,
            key: "playingLineTopOffset".to_string(),
            value: Value::Num(33.0),
        };
        write.apply(&mut with_base);
        assert_eq!(playing_line_top_offset(&with_base, ProfileKey::Cover), (33.0, false));
        assert_eq!(playing_line_top_offset(&with_base, ProfileKey::Mini), (42.0, true));
    }

    #[test]
    fn slider_writes_settle_after_120ms_and_coalesce() {
        let config = config();
        let mut drawer = DrawerState::open(ProfileKey::Cover);
        let spec = SLIDERS.iter().find(|s| s.key == "fontSize").unwrap();
        // The draft starts at the effective value.
        assert_eq!(drawer.slider_draft(&config, spec), 95.0);
        drawer.on_slide(spec, 130.0, 0.0);
        assert_eq!(drawer.slider_draft(&config, spec), 130.0);
        assert!(drawer.tick(50.0).is_empty(), "inside the settle window");
        drawer.on_slide(spec, 118.0, 60.0);
        // The last value wins and commits 120 ms after it.
        let writes = drawer.tick(180.0 + 0.1);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].key, "fontSize");
        assert_eq!(writes[0].value, Value::Num(118.0));
        assert_eq!(writes[0].profile, ProfileKey::Cover);
        assert!(drawer.tick(1000.0).is_empty(), "the queue drains");
    }

    #[test]
    fn sliders_snap_to_the_step_and_clamp() {
        let config = config();
        let mut drawer = DrawerState::open(ProfileKey::Mini);
        let line_gap = SLIDERS.iter().find(|s| s.key == "lineGap").unwrap();
        drawer.on_slide(line_gap, 0.31, 0.0);
        assert_eq!(drawer.slider_draft(&config, line_gap), 0.32, "snaps to 0.02");
        drawer.on_slide(line_gap, 9.0, 0.0);
        assert_eq!(drawer.slider_draft(&config, line_gap), 1.5, "clamped high");
        drawer.on_slide(line_gap, -9.0, 0.0);
        assert_eq!(drawer.slider_draft(&config, line_gap), -0.6, "clamped low");
    }

    #[test]
    fn an_external_value_change_hijacks_the_pending_commit() {
        // The legacy sync effect overwrites `latest.current` as well as the
        // draft, so a config broadcast mid-drag redirects the settle commit.
        let config = config();
        let mut drawer = DrawerState::open(ProfileKey::Cover);
        let spec = SLIDERS.iter().find(|s| s.key == "fontSize").unwrap();
        drawer.on_slide(spec, 150.0, 0.0);
        drawer.sync_value("fontSize", 80.0);
        assert_eq!(drawer.slider_draft(&config, spec), 80.0);
        let writes = drawer.tick(200.0);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].value, Value::Num(80.0), "the broadcast value commits");
    }

    #[test]
    fn switches_and_segments_write_immediately() {
        let mut drawer = DrawerState::open(ProfileKey::Mini);
        let write = drawer.set_switch("showTranslation", false);
        assert_eq!(write.key, "showTranslation");
        assert_eq!(write.value, Value::Bool(false));
        let write = drawer.set_segment("align", "right");
        assert_eq!(write.key, "align");
        assert_eq!(write.value, Value::Str("right".to_string()));
        let write = drawer.set_select("transFontFamily", "");
        assert_eq!(write.value, Value::Str(String::new()), "inherit stores the empty string");
    }

    #[test]
    fn writes_land_in_the_profile_store_only() {
        let mut config = config();
        Write {
            profile: ProfileKey::Cover,
            key: "transFontSize".to_string(),
            value: Value::Num(85.0),
        }
        .apply(&mut config);
        assert_eq!(read_num(&config, ProfileKey::Cover, "transFontSize", 100.0), 85.0);
        // The base section is untouched.
        assert_eq!(config.base.get("fontSize"), Some(&Value::Num(120.0)));
        // The mini store is untouched.
        assert_eq!(
            read_num(&config, ProfileKey::Mini, "transFontSize", 100.0),
            100.0
        );
    }

    #[test]
    fn the_option_tables_match_the_legacy_arrays() {
        assert_eq!(
            FONT_OPTIONS,
            &[
                "Pretendard",
                "檎风黑体 CHS",
                "微软雅黑",
                "宋体",
                "黑体",
                "楷体",
                "等线",
                "Consolas"
            ]
        );
        assert_eq!(
            TRANS_FONT_OPTIONS,
            &["", "檎风黑体 CHS", "微软雅黑", "宋体", "黑体", "楷体", "仿宋", "等线"]
        );
        assert_eq!(TRANS_FONT_INHERIT_LABEL, "继承");
    }

    #[test]
    fn the_slider_specs_match_the_legacy_bounds() {
        let expect: &[(&str, f64, f64, f64, u8, f64)] = &[
            ("playingLineTopOffset", 15.0, 85.0, 1.0, 0, 0.0),
            ("fontSize", 50.0, 200.0, 1.0, 0, 100.0),
            ("transFontSize", 50.0, 200.0, 1.0, 0, 100.0),
            ("lineGap", -0.6, 1.5, 0.02, 2, 0.0),
            ("innerGap", 0.0, 1.2, 0.02, 2, 0.3),
            ("romanFontScale", 20.0, 150.0, 5.0, 0, 50.0),
            ("fontWeight", 300.0, 900.0, 10.0, 0, 800.0),
            ("transFontWeight", 300.0, 900.0, 50.0, 0, 500.0),
            ("letterSpacing", -0.02, 0.1, 0.005, 3, 0.005),
            ("unplayedOriginalOpacity", 5.0, 100.0, 5.0, 0, 60.0),
            ("brightAlpha", 30.0, 100.0, 5.0, 0, 100.0),
            ("translatedLyricsOpacity", 10.0, 100.0, 5.0, 0, 60.0),
            ("transCurrentAlpha", 20.0, 100.0, 5.0, 0, 85.0),
            ("bgLineScale", 30.0, 150.0, 5.0, 0, 70.0),
            ("bgLineOpacity", 0.0, 100.0, 5.0, 0, 40.0),
            ("idleLineOpacity", 5.0, 100.0, 5.0, 0, 20.0),
        ];
        assert_eq!(SLIDERS.len(), expect.len(), "every legacy slider row exists");
        for (spec, (key, min, max, step, digits, fallback)) in SLIDERS.iter().zip(expect) {
            assert_eq!(spec.key, *key, "row order");
            assert_eq!(spec.min, *min, "{key} min");
            assert_eq!(spec.max, *max, "{key} max");
            assert_eq!(spec.step, *step, "{key} step");
            assert_eq!(spec.digits, *digits, "{key} digits");
            assert_eq!(spec.fallback, *fallback, "{key} fallback");
        }
    }

    #[test]
    fn folds_default_open_and_toggle() {
        let mut drawer = DrawerState::open(ProfileKey::Cover);
        for (id, _) in FOLDS {
            assert!(drawer.fold_open(id), "{id} starts open");
        }
        drawer.toggle_fold("layout");
        assert!(!drawer.fold_open("layout"));
        drawer.toggle_fold("layout");
        assert!(drawer.fold_open("layout"));
    }

    #[test]
    fn the_pane_labels_match_the_legacy_tabs() {
        assert_eq!(Pane::Type.label(), "歌词设置");
        assert_eq!(Pane::Fx.label(), "歌词效果");
        let mut drawer = DrawerState::open(ProfileKey::Mini);
        assert_eq!(drawer.pane, Pane::Type);
        drawer.set_pane(Pane::Fx);
        assert_eq!(drawer.pane, Pane::Fx);
    }

    #[test]
    fn the_readout_formats_like_the_reference() {
        assert_eq!(format_value(33.4, 0, "%"), "33%", "Math.round(33.4) is 33");
        assert_eq!(format_value(100.0, 0, "%"), "100%");
        assert_eq!(format_value(0.32, 2, "em"), "0.32em");
        assert_eq!(format_value(0.005, 3, "em"), "0.005em");
        assert_eq!(format_value(650.0, 0, ""), "650", "weight rows carry no unit");
    }

    #[test]
    fn the_drawer_tracks_the_host_mode_after_open() {
        let mut drawer = DrawerState::open(ProfileKey::Cover);
        assert!(!drawer.loaded);
        drawer.set_loaded();
        assert!(drawer.loaded);
        drawer.set_profile(ProfileKey::Mini);
        assert_eq!(drawer.profile, ProfileKey::Mini);
        // New writes follow the new store.
        let write = drawer.set_switch("enableSpring", false);
        assert_eq!(write.profile, ProfileKey::Mini);
    }
}
