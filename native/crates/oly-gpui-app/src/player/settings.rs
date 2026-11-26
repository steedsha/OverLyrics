//! Live display drafts with isolated persistence and legacy profile keys.
use super::model::{Box4, Display};
use crate::ui::settings::SLIDERS;
use oly_core::config::{merge, ConfigStore};
use serde_json::{json, Value};
use std::sync::Arc;
#[derive(Clone, Copy)]
pub enum Control {
    Slider { min: f64, max: f64, step: f64 },
    PlayerOffset(usize),
    Switch,
    Text { secret: bool },
    Segmented(&'static [(&'static str, &'static str)]),
    Choice(&'static [(&'static str, &'static str)]),
    Action { button: &'static str, danger: bool },
    AliasRow(usize),
    Info(&'static str),
}
impl Control {
    pub fn slider_bounds(self) -> Option<(f64, f64, f64)> {
        match self {
            Control::Slider { min, max, step } => Some((min, max, step)),
            Control::PlayerOffset(_) => Some((-2000.0, 2000.0, 50.0)),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsModal {
    CustomModel {
        provider: String,
        draft: String,
    },
    ArtistAlias {
        from: String,
        to: String,
        focus: usize,
    },
    PrivacyPolicy,
}
#[derive(Clone, Copy)]
pub struct Item {
    pub key: &'static str,
    pub label: &'static str,
    pub control: Control,
}
const fn slider(key: &'static str, label: &'static str, min: f64, max: f64, step: f64) -> Item {
    Item {
        key,
        label,
        control: Control::Slider { min, max, step },
    }
}
const fn toggle(key: &'static str, label: &'static str) -> Item {
    Item {
        key,
        label,
        control: Control::Switch,
    }
}
const fn text(key: &'static str, label: &'static str, secret: bool) -> Item {
    Item { key, label, control: Control::Text { secret } }
}
const fn choice(
    key: &'static str,
    label: &'static str,
    options: &'static [(&'static str, &'static str)],
) -> Item {
    Item {
        key,
        label,
        control: Control::Choice(options),
    }
}
const fn segmented(
    key: &'static str,
    label: &'static str,
    options: &'static [(&'static str, &'static str)],
) -> Item {
    Item {
        key,
        label,
        control: Control::Segmented(options),
    }
}
const fn action(key: &'static str, label: &'static str, button: &'static str, danger: bool) -> Item {
    Item {
        key,
        label,
        control: Control::Action { button, danger },
    }
}
const fn info(key: &'static str, label: &'static str, value: &'static str) -> Item {
    Item {
        key,
        label,
        control: Control::Info(value),
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsKind {
    Application,
    #[default]
    Lyrics,
}
pub const PAGES: &[&str] = &["排版", "字体", "亮度", "动效", "高级"];
pub const APPLICATION_PAGES: &[&str] = &["外观", "排版", "视觉", "词源", "翻译", "全局", "关于"];

const ACTIVE_PLAYER_OFFSET_KEY: &str = "lyrics.playerSyncOffsets._current";
const CUSTOM_PLAYER_OFFSET_KEY: &str = "lyrics.playerSyncOffsets._custom";

/// Display controls are shape-specific unless explicitly global; dotted keys
/// identify a path in the complete document, never a display profile field.
enum ConfigPath<'a> {
    Profile(&'a str),
    Display(&'a str),
    Document(&'a str),
}
fn config_path(key: &str) -> ConfigPath<'_> {
    if key.contains('.') { return ConfigPath::Document(key); }
    if matches!(key, "showCover" | "showProgress" | "uiAccentColor" | "coverRadius"
        | "cornerRadius" | "motionCover" | "motionCoverCacheMB" | "windowBackground"
        | "immersionCompact" | "beatFollow" | "beatFlashThreshold" | "beatFlashStrength"
        | "beatWobbleThreshold" | "beatWobbleStrength" | "backdropStyle" | "themeMode") {
        ConfigPath::Display(key)
    } else { ConfigPath::Profile(key) }
}
fn nested_patch(path: &str, value: Value) -> Value {
    path.split('.').rev().fold(value, |value, segment| {
        let mut map = serde_json::Map::new();
        map.insert(segment.to_owned(), value);
        Value::Object(map)
    })
}
fn ai_models_for(provider: &str) -> &'static [(&'static str, &'static str)] {
    static MODEL_LISTS: std::sync::LazyLock<Vec<(&'static str, Vec<(&'static str, &'static str)>)>> =
        std::sync::LazyLock::new(|| {
            oly_contracts::config::AI_PRESETS
                .iter()
                .map(|p| {
                    (
                        p.provider,
                        p.models.iter().map(|m| (m.value, m.label)).collect(),
                    )
                })
                .collect()
        });
    MODEL_LISTS
        .iter()
        .find(|(p, _)| *p == provider)
        .or_else(|| MODEL_LISTS.first())
        .map(|(_, m)| m.as_slice())
        .unwrap_or(&[])
}

fn application_items(page: usize) -> Vec<Item> {
    match page {
        0 => vec![
            segmented("themeMode", "主题配色", &[("glass", "液体玻璃"), ("light", "浅色"), ("dark", "深色")]),
            segmented(
                "uiAccentColor",
                "强调色",
                &[("auto", "跟随封面"), ("#0a84ff", "经典蓝"), ("#ff9f0a", "经典橙")],
            ),
            toggle("showCover", "显示封面"),
            toggle("motionCover", "动态封面"),
            slider("coverRadius", "封面圆角", 0., 48., 1.),
            slider("motionCoverCacheMB", "动态封面缓存上限", 100., 2000., 100.),
            segmented("windowBackground", "窗口背景", &[("immersion", "沉浸背景"), ("glass", "玻璃背景")]),
            toggle("immersionCompact", "迷你与歌词模式使用沉浸背景"),
            toggle("beatFollow", "沉浸背景跟随鼓点"),
            slider("cornerRadius", "窗口与面板圆角", 0., 36., 1.),
        ],
        1 => vec![
            slider("fontSize", "原文字号", 50., 200., 1.),
            slider("transFontSize", "译文字号", 50., 200., 1.),
            slider("lineGap", "行间距", -0.6, 1.5, 0.02),
            slider("innerGap", "原文与译文间距", 0., 1.2, 0.02),
            slider("playingLineTopOffset", "当前行位置", 15., 85., 1.),
            segmented("align", "歌词对齐", &[("left", "左对齐"), ("right", "右对齐")]),
            segmented("lineBreakMode", "折行方式", &[("natural", "自然折行"), ("balanced", "均衡分行")]),
            toggle("showTranslation", "显示译文"),
            toggle("showRomanization", "显示罗马音 / 注音"),
            slider("romanFontScale", "注音字号比例", 20., 150., 1.),
            slider("bgLineScale", "和声行缩放", 30., 150., 1.),
            choice(
                "fontFamily",
                "原文字体",
                &[
                    ("Pretendard", "Pretendard"),
                    ("檎风黑体 CHS", "檎风黑体 CHS"),
                    ("微软雅黑", "微软雅黑"),
                    ("黑体", "黑体"),
                    ("等线", "等线"),
                ],
            ),
            choice(
                "transFontFamily",
                "译文字体",
                &[
                    ("", "继承"),
                    ("檎风黑体 CHS", "檎风黑体 CHS"),
                    ("微软雅黑", "微软雅黑"),
                    ("黑体", "黑体"),
                    ("等线", "等线"),
                ],
            ),
        ],
        2 => vec![
            toggle("enableSpring", "歌词滚动弹簧"),
            segmented(
                "springFeel",
                "弹簧手感",
                &[("lively", "灵动"), ("standard", "标准"), ("calm", "沉稳")],
            ),
            toggle("enableScale", "当前行缩放"),
            toggle("hidePassedLines", "隐藏已唱歌词"),
            toggle("blurMax", "非当前行模糊"),
            slider("idleLineOpacity", "非当前行亮度", 5., 100., 1.),
            slider("unplayedOriginalOpacity", "未唱部分亮度", 5., 100., 1.),
            slider("translatedLyricsOpacity", "非当前行译文亮度", 10., 100., 1.),
            slider("edgeFeather", "上下羽化", 0., 160., 2.),
        ],
        3 => vec![
            toggle("lyrics.enabledSources.kugou", "酷狗音乐"),
            toggle("lyrics.enabledSources.netease", "网易云音乐"),
            toggle("lyrics.enabledSources.qq", "QQ 音乐"),
            toggle("lyrics.enabledSources.lrclib", "LRCLIB"),
            toggle("lyrics.enabledSources.amlldb", "AMLL 歌词库"),
            slider("lyrics.matchThreshold", "歌词匹配阈值", 0., 100., 1.),
            slider("lyrics.lyricsCacheMB", "本地歌词缓存上限", 10., 500., 10.),
            action("lyrics.addArtistAlias", "添加歌手名映射", "+ 添加", false),
        ],
        4 => vec![
            toggle("lyrics.autoTranslate", "自动翻译"),
            segmented("lyrics.translateEngine", "翻译引擎", &[("free", "免费翻译"), ("ai", "AI 翻译")]),
            choice("lyrics.translateTarget", "翻译目标语言", &[
                ("zh-CHS", "简体中文"), ("zh-CHT", "繁体中文"),
                ("en", "英语"), ("ja", "日语"), ("ko", "韩语"),
            ]),
        ],
        5 => vec![
            slider("lyrics.playerSyncOffsets.apple", "Apple Music 同步偏移", -2000., 2000., 50.),
            action("action.clearLyricsCache", "清理歌词缓存", "清理", false),
            action("action.clearCoverCache", "清理封面缓存", "清理", false),
            action("action.resetTrackOffsets", "重置单曲偏移", "重置", false),
            action("action.resetDefaults", "恢复默认设置", "恢复默认", true),
        ],
        6 => vec![
            info("about.author", "制作者", "steedsha"),
            info("about.version", "版本", concat!("v", env!("CARGO_PKG_VERSION"))),
            action("action.openWebsite", "官方网站", "查看", false),
            action("action.openRepo", "开源仓库", "查看", false),
            action("action.checkUpdate", "软件更新", "最新", false),
            action("action.privacyPolicy", "隐私条款", "查看", false),
        ],
        _ => vec![],
    }
}
fn lyric_items(page: usize) -> Vec<Item> {
    if page == 4 {
        return vec![
            slider("lineWidthAspect", "行宽比例", 0.3, 1., 0.01),
            slider("linePaddingX", "水平留白", 0., 4., 0.05),
            slider("letterSpacing", "字距", -0.02, 0.1, 0.005),
            slider("longSyllableThreshold", "长音阈值", 500., 4000., 100.),
            slider("wordFadeWidth", "逐字渐变宽度", 0., 2., 0.05),
            slider("edgeFeather", "上下羽化", 0., 160., 2.),
        ];
    }
    if page >= PAGES.len() { return vec![]; }
    let mut rows = items(page);
    rows.retain(|item| !matches!(item.key, "lineWidthAspect" | "linePaddingX" | "letterSpacing"
        | "longSyllableThreshold" | "wordFadeWidth" | "edgeFeather"));
    if page == 3 {
        rows.push(slider("lineRadius", "悬停框圆角", 0., 2., 0.05));
        rows.push(slider("hoverBgAlpha", "悬停框亮度", 0., 30., 1.));
    }
    rows
}
/// Legacy row list retained for QA callers (appearance/accent at page 4, row 4).
pub fn items(page: usize) -> Vec<Item> {
    match page {
        0 => vec![
            slider("fontSize", "原文字号", 50., 200., 1.),
            slider("transFontSize", "译文字号", 50., 200., 1.),
            slider("lineGap", "行间距", -0.6, 1.5, 0.02),
            slider("innerGap", "原文与译文间距", 0., 1.2, 0.02),
            slider("playingLineTopOffset", "当前行位置", 15., 85., 1.),
            segmented("align", "歌词对齐", &[("left", "左对齐"), ("right", "右对齐")]),
            segmented(
                "lineBreakMode",
                "折行方式",
                &[("natural", "自然折行"), ("balanced", "均衡分行")],
            ),
            toggle("showTranslation", "显示译文"),
            toggle("showRomanization", "显示罗马音 / 注音"),
        ],
        1 => vec![
            choice(
                "fontFamily",
                "原文字体",
                &[
                    ("Pretendard", "Pretendard"),
                    ("檎风黑体 CHS", "檎风黑体 CHS"),
                    ("微软雅黑", "微软雅黑"),
                    ("宋体", "宋体"),
                    ("黑体", "黑体"),
                    ("楷体", "楷体"),
                    ("等线", "等线"),
                    ("Consolas", "Consolas"),
                ],
            ),
            choice(
                "transFontFamily",
                "译文字体",
                &[
                    ("", "继承"),
                    ("檎风黑体 CHS", "檎风黑体 CHS"),
                    ("微软雅黑", "微软雅黑"),
                    ("宋体", "宋体"),
                    ("黑体", "黑体"),
                    ("楷体", "楷体"),
                    ("仿宋", "仿宋"),
                    ("等线", "等线"),
                ],
            ),
            slider("fontWeight", "原文字重", 300., 900., 10.),
            slider("transFontWeight", "译文字重", 300., 900., 50.),
            slider("letterSpacing", "字距", -0.02, 0.1, 0.005),
            slider("romanFontScale", "注音字号比例", 20., 150., 1.),
            slider("lineWidthAspect", "行宽比例", 0.3, 1., 0.01),
            slider("linePaddingX", "水平留白", 0., 4., 0.05),
        ],
        2 => vec![
            slider("unplayedOriginalOpacity", "未唱部分亮度", 5., 100., 1.),
            slider("brightAlpha", "已唱部分亮度", 30., 100., 1.),
            slider("translatedLyricsOpacity", "非当前行译文亮度", 10., 100., 1.),
            slider("transCurrentAlpha", "当前行译文亮度", 20., 100., 1.),
            slider("idleLineOpacity", "非当前行亮度", 5., 100., 1.),
            slider("bgLineScale", "和声行缩放", 30., 150., 1.),
            slider("bgLineOpacity", "和声行亮度", 0., 100., 1.),
            slider("phoneticLyricsOpacity", "注音亮度", 0., 100., 1.),
        ],
        4 => vec![
            toggle("showCover", "显示封面"),
            toggle("showProgress", "显示播放进度"),
            slider("coverRadius", "封面圆角", 0., 48., 1.),
            slider("cornerRadius", "窗口与面板圆角", 0., 36., 1.),
            choice(
                "uiAccentColor",
                "强调色",
                &[
                    ("#0a84ff", "蓝色"),
                    ("#7fd6c0", "青色"),
                    ("#5e5ce6", "靛紫"),
                    ("#ff375f", "粉红"),
                    ("#ff9f0a", "琥珀橙"),
                    ("#30d158", "翠绿"),
                ],
            ),
        ],
        _ => vec![
            toggle("enableSpring", "歌词滚动弹簧"),
            segmented(
                "springFeel",
                "弹簧手感",
                &[("lively", "灵动"), ("standard", "标准"), ("calm", "沉稳")],
            ),
            toggle("enableScale", "当前行缩放"),
            toggle("hidePassedLines", "隐藏已唱歌词"),
            toggle("blurMax", "非当前行模糊"),
            slider("blurStrength", "模糊强度", 50., 400., 1.),
            slider("wordFadeWidth", "逐字渐变宽度", 0., 2., 0.05),
            slider("edgeFeather", "上下羽化", 0., 160., 2.),
            slider("longSyllableThreshold", "长音阈值", 500., 4000., 100.),
        ],
    }
}
pub struct Settings {
    pub kind: SettingsKind,
    pub profile: &'static str,
    pub display_mode_label: Option<&'static str>,
    pub editing_key: Option<&'static str>,
    pub editing_draft: String,
    pub config: Value,
    pub open: bool,
    pub page: usize,
    pub title: &'static str,
    pub scroll: f32,
    scroll_spring: oly_core::scroll::Spring,
    pub status: String,
    store: Option<Arc<ConfigStore>>,
    pending: Value,
    deadline: f64,
    switch_springs: std::collections::HashMap<&'static str, oly_core::scroll::Spring>,
    pub segmented_lenses: std::collections::HashMap<&'static str, super::chrome::SegmentedLens>,
    pub segmented_drag: Option<(&'static str, usize)>,
    pub tab_lens: super::chrome::SegmentedLens,
    pub open_choice: Option<usize>,
    pub choice_scroll: f32,
    choice_scroll_spring: oly_core::scroll::Spring,
    pub choice_hover: Option<usize>,
    pub switch_preview:Option<(&'static str,f32)>,
    pub active_player: String,
    pub viewport_h: f32,
    outgoing_page: Option<usize>,
    outgoing_scroll: f32,
    page_anim: f32,
    page_dir: i8,
    last_tick_ms: Option<f64>,
    pub modal: Option<SettingsModal>,
    pub reset_confirm_until_ms: Option<f64>,
    pub pending_action: Option<&'static str>,
    pub current_track_artist: String,
    pub alias_changed_artist: Option<String>,
    pub status_alpha: f32,
    status_at_ms: Option<f64>,
    last_status: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            kind: SettingsKind::Lyrics,
            profile: "cover",
            display_mode_label: None,
            editing_key: None,
            editing_draft: String::new(),
            config: serde_json::from_str(include_str!(
                "../../../oly-core/src/config_defaults.json"
            ))
            .expect("display defaults"),
            open: false,
            page: 0,
            title: "歌词设置",
            scroll: 0.,
            scroll_spring: {
                let mut s = oly_core::scroll::Spring::new(0.0);
                s.update_params(oly_core::scroll::SpringParams::new(220.0, 26.0));
                s
            },
            status: String::new(),
            store: None,
            pending: json!({}),
            deadline: 0.,
            switch_springs: std::collections::HashMap::new(),
            segmented_lenses: std::collections::HashMap::new(),
            segmented_drag: None,
            tab_lens: super::chrome::SegmentedLens::default(),
            open_choice: None,
            choice_scroll: 0.,
            choice_scroll_spring: {
                let mut s = oly_core::scroll::Spring::new(0.0);
                s.update_params(oly_core::scroll::SpringParams::new(220.0, 26.0));
                s
            },
            choice_hover:None,switch_preview:None,
            active_player: String::new(),
            viewport_h: 0.,
            outgoing_page: None,
            outgoing_scroll: 0.0,
            page_anim: 1.0,
            page_dir: 1,
            last_tick_ms: None,
            modal: None,
            reset_confirm_until_ms: None,
            pending_action: None,
            current_track_artist: String::new(),
            alias_changed_artist: None,
            status_alpha: 0.0,
            status_at_ms: None,
            last_status: String::new(),
        }
    }
}
impl Settings {
    pub fn set_kind(&mut self, kind: SettingsKind) {
        self.editing_key = None;
        self.editing_draft.clear();
        self.kind = kind;
        self.title = match kind { SettingsKind::Application => "设置", SettingsKind::Lyrics => "歌词设置" };
        self.page = 0;
        self.set_scroll_immediate(0.0);
        self.outgoing_page = None;
        self.outgoing_scroll = 0.0;
        self.page_anim = 1.0;
        self.page_dir = 1;
        self.close_choice();
        self.switch_preview = None;
        self.segmented_drag = None;
        self.tab_lens = super::chrome::SegmentedLens::default();
    }
    pub fn set_scroll_immediate(&mut self, y: f32) {
        let y = y.max(0.0);
        self.scroll = y;
        self.scroll_spring.set_position(y as f64);
        self.scroll_spring.set_target_position(y as f64);
    }
    pub fn set_choice_scroll_immediate(&mut self, y: f32) {
        let y = y.max(0.0);
        self.choice_scroll = y;
        self.choice_scroll_spring.set_position(y as f64);
        self.choice_scroll_spring.set_target_position(y as f64);
    }
    pub fn wheel_scroll(&mut self, delta: f32, max_scroll: f32) {
        let next = ((self.scroll_spring.target() as f32) - delta).clamp(0.0, max_scroll.max(0.0));
        self.scroll_spring.set_target_position(next as f64);
    }
    pub fn wheel_choice_scroll(&mut self, delta: f32, max_scroll: f32) {
        let next = ((self.choice_scroll_spring.target() as f32) - delta).clamp(0.0, max_scroll.max(0.0));
        self.choice_scroll_spring.set_target_position(next as f64);
    }
    pub fn clamp_scroll(&mut self, max_scroll: f32) {
        let max_s = max_scroll.max(0.0);
        let target = (self.scroll_spring.target() as f32).clamp(0.0, max_s);
        self.scroll_spring.set_target_position(target as f64);
        self.scroll = (self.scroll_spring.position() as f32).clamp(0.0, max_s);
    }
    pub fn set_page(&mut self, page: usize) {
        let page = page.min(self.pages().len().saturating_sub(1));
        if self.page == page {
            return;
        }
        self.close_choice();
        self.segmented_drag = None;
        self.outgoing_page = Some(self.page);
        self.outgoing_scroll = self.scroll;
        self.page_dir = if page > self.page { 1 } else { -1 };
        self.page_anim = 0.0;
        self.page = page;
        self.set_scroll_immediate(0.0);
    }
    pub fn pages(&self) -> &'static [&'static str] {
        match self.kind { SettingsKind::Application => APPLICATION_PAGES, SettingsKind::Lyrics => PAGES }
    }
    pub fn items(&self) -> Vec<Item> { self.items_on(self.page) }
    fn items_on(&self, page: usize) -> Vec<Item> {
        let mut rows = match self.kind { SettingsKind::Application => application_items(page), SettingsKind::Lyrics => lyric_items(page) };
        if self.kind == SettingsKind::Application && page == 0 {
            if self.value("beatFollow").as_bool().unwrap_or(true) {
                if let Some(pos) = rows.iter().position(|item| item.key == "beatFollow") {
                    let beat_rows = [
                        slider("beatFlashThreshold", "鼓点闪光阈值", 1.02, 1.60, 0.01),
                        slider("beatFlashStrength", "鼓点闪光强度", 0.00, 2.50, 0.05),
                        slider("beatWobbleThreshold", "色团律动阈值", 1.05, 1.80, 0.01),
                        slider("beatWobbleStrength", "色团律动强度", 0.00, 2.50, 0.05),
                    ];
                    rows.splice(pos + 1..pos + 1, beat_rows);
                }
            }
        }
        if self.kind == SettingsKind::Application && page == 3 {
            let aliases = self.artist_aliases();
            if !aliases.is_empty() {
                let insert_at = rows.len().saturating_sub(1);
                let alias_rows: Vec<Item> = (0..aliases.len())
                    .map(|idx| Item {
                        key: "lyrics.artistAliases",
                        label: "",
                        control: Control::AliasRow(idx),
                    })
                    .collect();
                rows.splice(insert_at..insert_at, alias_rows);
            }
        }
        if self.kind == SettingsKind::Application && page == 4 {
            if matches!(self.value("lyrics.translateEngine").as_str(), Some("ai" | "ai-first")) {
                static PROVIDERS: std::sync::LazyLock<Vec<(&'static str, &'static str)>> = std::sync::LazyLock::new(|| {
                    oly_contracts::config::AI_PRESETS.iter().map(|p| (p.provider, p.name)).collect()
                });
                let provider = self.value("lyrics.aiPreset").as_str().unwrap_or("deepseek").to_owned();
                let mut ai_rows = vec![
                    choice("lyrics.aiPreset", "AI 服务商", PROVIDERS.as_slice()),
                    choice("lyrics.aiModel", "AI 模型", ai_models_for(&provider)),
                    text("lyrics.aiBaseUrl", "API 地址", false),
                    text("lyrics.aiKey", "API 密钥", true),
                    toggle("lyrics.aiFallback", "AI 失败时回退至免费翻译"),
                ];
                rows.append(&mut ai_rows);
            }
        }
        if self.kind == SettingsKind::Application && page == 5 {
            let saved = self.saved_player_offsets();
            let mut insert_pos = 1usize;
            for (idx, _) in saved.iter().enumerate() {
                rows.insert(
                    insert_pos,
                    Item {
                        key: CUSTOM_PLAYER_OFFSET_KEY,
                        label: "",
                        control: Control::PlayerOffset(idx),
                    },
                );
                insert_pos += 1;
            }
            let bucket = self.active_player_bucket();
            if !bucket.is_empty()
                && bucket != "apple"
                && bucket != "default"
                && !saved.iter().any(|b| b == &bucket)
            {
                rows.insert(
                    insert_pos,
                    slider(ACTIVE_PLAYER_OFFSET_KEY, "当前播放器同步偏移", -2000., 2000., 50.),
                );
            }
        }
        rows
    }
    pub fn saved_player_offsets(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .config
            .pointer("/lyrics/playerSyncOffsets")
            .and_then(Value::as_object)
            .map(|obj| {
                obj.iter()
                    .filter(|(k, v)| k.as_str() != "apple" && k.as_str() != "default" && v.is_number())
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
    pub fn remove_player_offset(&mut self, row_index: usize, now: f64) -> Option<String> {
        let item = self.items().get(row_index).copied()?;
        let Control::PlayerOffset(idx) = item.control else {
            return None;
        };
        let bucket = self.saved_player_offsets().get(idx).cloned()?;
        if let Some(obj) = self
            .config
            .get_mut("lyrics")
            .and_then(|l| l.get_mut("playerSyncOffsets"))
            .and_then(|v| v.as_object_mut())
        {
            obj.remove(&bucket);
        }
        let mut null_map = serde_json::Map::new();
        null_map.insert(bucket.clone(), Value::Null);
        let patch = nested_patch("lyrics.playerSyncOffsets", Value::Object(null_map));
        merge(&mut self.pending, patch);
        self.deadline = now + 120.0;
        if let Some(store) = self.store.as_ref() {
            let pending = std::mem::replace(&mut self.pending, json!({}));
            if let Err(err) = store.patch(pending) {
                log::warn!("settings player offset remove failed: {err}");
            }
        }
        let name = oly_core::mapping::player_display_name(&bucket);
        self.status = format!("已删除 {name} 同步偏移");
        Some(bucket)
    }
    pub fn active_player_bucket(&self) -> String {
        oly_core::mapping::player_offset_key(Some(&self.active_player))
    }
    fn resolve_key<'a>(&self, key: &'a str) -> std::borrow::Cow<'a, str> {
        if key == ACTIVE_PLAYER_OFFSET_KEY {
            std::borrow::Cow::Owned(format!(
                "lyrics.playerSyncOffsets.{}",
                self.active_player_bucket()
            ))
        } else {
            std::borrow::Cow::Borrowed(key)
        }
    }
    pub fn begin_edit(&mut self, index: usize) -> bool {
        let Some(item) = self.items().get(index).copied() else { return false; };
        if !matches!(item.control, Control::Text { .. }) { return false; }
        self.close_choice();
        self.editing_key = Some(item.key);
        self.editing_draft = self.value(item.key).as_str().unwrap_or_default().to_owned();
        true
    }
    pub fn insert_text(&mut self, text: &str) -> bool {
        if self.editing_key.is_none() { return false; }
        self.editing_draft.extend(text.chars().filter(|c| !c.is_control()));
        true
    }
    pub fn finish_edit(&mut self, now: f64, commit: bool) {
        if let Some(key) = self.editing_key.take() {
            let draft = std::mem::take(&mut self.editing_draft);
            if commit { self.set(key, json!(draft), now, false); }
        }
    }
    /// Physical key names are accepted for commands; pass actual typed text to
    /// insert_text so keyboard layout, case and pasted credentials are preserved.
    pub fn edit_key(&mut self, key: &str, now: f64) -> bool {
        if self.editing_key.is_none() { return false; }
        match key {
            "enter" | "return" => self.finish_edit(now, true),
            "escape" => self.finish_edit(now, false),
            "backspace" => { self.editing_draft.pop(); },
            "delete" => self.editing_draft.clear(),
            "space" => { self.insert_text(" "); },
            _ if key.chars().count() == 1 => { self.insert_text(key); },
            _ => {},
        }
        true
    }
    pub fn with_store(store: Arc<ConfigStore>) -> Self {
        let mut settings = Self::default();
        settings.config = store.get();
        settings.store = Some(store);
        settings
    }
    pub fn active_profile(&self) -> &'static str {
        if self.profile == "mini" {
            "mini"
        } else if self.config["display"]["showCover"]
            .as_bool()
            .unwrap_or(true)
        {
            "cover"
        } else {
            "nocover"
        }
    }
    pub fn effective(&self) -> Value {
        Display::effective_json_for(&self.config, Some(self.active_profile()))
    }
    pub fn pending(&self) -> bool {
        self.pending.as_object().is_some_and(|p| !p.is_empty())
    }
    pub fn animating(&self) -> bool {
        self.pending()
            || self.editing_key.is_some()
            || self.modal.is_some()
            || self.tab_lens.pending()
            || self.segmented_lenses.values().any(super::chrome::SegmentedLens::pending)
            || self.outgoing_page.is_some()
            || self.switch_springs.values().any(|s| !s.arrived())
            || !self.scroll_spring.arrived()
            || !self.choice_scroll_spring.arrived()
    }
    pub fn value(&self, key: &str) -> Value {
        let resolved = self.resolve_key(key);
        let raw = match config_path(&resolved) {
            ConfigPath::Document(path) => path
                .split('.')
                .try_fold(&self.config, |value, segment| value.get(segment))
                .cloned()
                .unwrap_or(Value::Null),
            ConfigPath::Display(field) => self.config["display"].get(field).cloned().unwrap_or(Value::Null),
            ConfigPath::Profile(field) => self.effective().get(field).cloned().unwrap_or(Value::Null),
        };
        if raw.is_null() {
            match resolved.as_ref() {
                "beatFollow" => return json!(true),
                "beatFlashThreshold" => return json!(1.04),
                "beatFlashStrength" => return json!(1.20),
                "beatWobbleThreshold" => return json!(1.10),
                "beatWobbleStrength" => return json!(1.20),
                _ => {}
            }
        }
        if raw.is_null() && resolved.starts_with("lyrics.playerSyncOffsets.") {
            let bucket = resolved.trim_start_matches("lyrics.playerSyncOffsets.");
            if bucket != "apple" && bucket != "default" {
                let fallback = &self.config["lyrics"]["playerSyncOffsets"]["default"];
                if !fallback.is_null() {
                    return fallback.clone();
                }
            }
            return json!(0);
        }
        raw
    }
    pub fn set(&mut self, key: &str, value: Value, now: f64, settle: bool) {
        let resolved = self.resolve_key(key);
        let shape = self.active_profile();
        let mut patch = match config_path(&resolved) {
            ConfigPath::Document(path) => nested_patch(path, value),
            ConfigPath::Display(field) => json!({ "display": { field: value } }),
            ConfigPath::Profile(field) => json!({ "display": { "typeProfiles": { shape: { field: value } } } }),
        };
        if resolved == "lyrics.aiPreset" {
            if let Some(selection) = patch.pointer("/lyrics/aiPreset").and_then(Value::as_str).and_then(oly_contracts::config::select_ai_provider) {
                merge(&mut patch, json!({"lyrics": {"aiBaseUrl": selection.ai_base_url, "aiModel": selection.ai_model, "aiKey": selection.ai_key}}));
            }
        }
        merge(&mut self.config, patch.clone());
        merge(&mut self.pending, patch);
        self.deadline = now + if settle { 120. } else { 0. };
    }
    pub fn close_choice(&mut self) {
        self.open_choice = None;
        self.set_choice_scroll_immediate(0.0);
        self.choice_hover=None;
    }
    pub fn custom_models_for(&self, provider: &str) -> Vec<String> {
        self.config
            .get("lyrics")
            .and_then(|l| l.get("aiCustomModels"))
            .and_then(|m| m.get(provider))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn model_options_for(&self, provider: &str) -> Vec<(String, String, bool)> {
        let mut out: Vec<(String, String, bool)> = ai_models_for(provider)
            .iter()
            .map(|&(v, l)| (v.to_string(), l.to_string(), false))
            .collect();
        for m in self.custom_models_for(provider) {
            if !out.iter().any(|(v, _, _)| v == &m) {
                out.push((m.clone(), m, true));
            }
        }
        out.push(("__custom_add__".to_string(), "+ 自定义模型...".to_string(), false));
        out
    }
    pub fn add_custom_model(&mut self, provider: &str, model: &str, now: f64) -> bool {
        let model = model.trim();
        if model.is_empty() {
            return false;
        }
        let mut list = self.custom_models_for(provider);
        if !ai_models_for(provider).iter().any(|&(v, _)| v == model)
            && !list.iter().any(|v| v == model)
        {
            list.push(model.to_string());
            let mut map = serde_json::Map::new();
            map.insert(provider.to_string(), json!(list));
            self.set("lyrics.aiCustomModels", Value::Object(map), now, false);
        }
        self.set("lyrics.aiModel", json!(model), now, false);
        self.status = format!("已添加模型 {model}");
        true
    }
    pub fn delete_custom_model_option(&mut self, opt_index: usize, now: f64) -> bool {
        let Some(row_idx) = self.open_choice else {
            return false;
        };
        let Some(item) = self.items().get(row_idx).copied() else {
            return false;
        };
        if item.key != "lyrics.aiModel" {
            return false;
        }
        let provider = self.value("lyrics.aiPreset").as_str().unwrap_or("deepseek").to_owned();
        let opts = self.model_options_for(&provider);
        let Some((key, _, is_custom)) = opts.get(opt_index).cloned() else {
            return false;
        };
        if !is_custom {
            return false;
        }
        let mut list = self.custom_models_for(&provider);
        list.retain(|m| m != &key);
        let mut map = serde_json::Map::new();
        map.insert(provider.clone(), json!(list));
        self.set("lyrics.aiCustomModels", Value::Object(map), now, false);
        if self.value("lyrics.aiModel").as_str() == Some(key.as_str()) {
            let fallback = oly_contracts::config::AI_PRESETS
                .iter()
                .find(|p| p.provider == provider)
                .map(|p| p.default_model)
                .unwrap_or("deepseek-flash");
            self.set("lyrics.aiModel", json!(fallback), now, false);
        }
        self.status = format!("已删除自定义模型 {key}");
        true
    }
    pub fn artist_aliases(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .config
            .get("lyrics")
            .and_then(|l| l.get("artistAliases"))
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| {
                        let key = k.trim();
                        let val = v.as_str().unwrap_or("").trim();
                        if !key.is_empty() && !val.is_empty() {
                            Some((key.to_string(), val.to_string()))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        out
    }
    pub fn add_artist_alias(&mut self, from: &str, to: &str, now: f64) -> bool {
        let from = from.trim();
        let to = to.trim();
        if from.is_empty() || to.is_empty() {
            return false;
        }
        let mut obj = self
            .config
            .get("lyrics")
            .and_then(|l| l.get("artistAliases"))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        obj.retain(|k, _| !k.trim().eq_ignore_ascii_case(from));
        obj.insert(from.to_string(), json!(to));
        self.set_exact("lyrics.artistAliases", Value::Object(obj), now);
        self.alias_changed_artist = Some(from.to_string());
        self.status = format!("已添加映射：{from} → {to}");
        true
    }
    pub fn remove_artist_alias(&mut self, from: &str, now: f64) -> bool {
        let Some(obj) = self
            .config
            .get_mut("lyrics")
            .and_then(|l| l.get_mut("artistAliases"))
            .and_then(|v| v.as_object_mut())
        else {
            return false;
        };
        let keys_to_remove: Vec<String> = obj
            .keys()
            .filter(|k| k.trim() == from.trim())
            .cloned()
            .collect();
        if keys_to_remove.is_empty() {
            return false;
        }
        let mut null_map = serde_json::Map::new();
        for k in &keys_to_remove {
            obj.remove(k);
            null_map.insert(k.clone(), Value::Null);
        }
        let patch = nested_patch("lyrics.artistAliases", Value::Object(null_map));
        merge(&mut self.pending, patch);
        self.deadline = now + 120.0;
        if let Some(store) = self.store.as_ref() {
            let pending = std::mem::replace(&mut self.pending, json!({}));
            if let Err(err) = store.patch(pending) {
                log::warn!("settings alias remove failed: {err}");
            }
        }
        self.alias_changed_artist = Some(from.to_string());
        self.status = format!("已移除映射：{from}");
        true
    }
    fn set_exact(&mut self, key: &str, value: Value, now: f64) {
        let patch = nested_patch(key, value);
        merge(&mut self.config, patch.clone());
        merge(&mut self.pending, patch);
        self.deadline = now + 120.0;
        if let Some(store) = self.store.as_ref() {
            let pending = std::mem::replace(&mut self.pending, json!({}));
            if let Err(err) = store.patch(pending) {
                log::warn!("settings exact save failed: {err}");
            }
        }
    }
    pub fn modal_rects(panel_rect: Box4, modal: &SettingsModal) -> (Box4, Box4, Option<Box4>, Box4, Box4) {
        Self::modal_rects_with_offset(panel_rect, modal, [0.0, 0.0])
    }
    pub fn modal_rects_with_offset(
        panel_rect: Box4,
        modal: &SettingsModal,
        offset: [f32; 2],
    ) -> (Box4, Box4, Option<Box4>, Box4, Box4) {
        let mw = match modal {
            SettingsModal::PrivacyPolicy => 400.0_f32.min((panel_rect.w - 28.0).max(260.0)),
            _ => 310.0_f32.min((panel_rect.w - 36.0).max(220.0)),
        };
        let mh = match modal {
            SettingsModal::CustomModel { .. } => 148.0,
            SettingsModal::ArtistAlias { .. } => 204.0,
            SettingsModal::PrivacyPolicy => 364.0_f32.min((panel_rect.h - 28.0).max(240.0)),
        };
        let box_r = Box4::new(
            panel_rect.x + (panel_rect.w - mw) * 0.5 + offset[0],
            panel_rect.y + (panel_rect.h - mh) * 0.5 + offset[1],
            mw,
            mh,
        );
        let (f1, f2, cancel, save) = match modal {
            SettingsModal::PrivacyPolicy => (
                Box4::new(0.0, 0.0, 0.0, 0.0),
                None,
                Box4::new(box_r.right() - 80.0, box_r.bottom() - 40.0, 64.0, 28.0),
                Box4::new(0.0, 0.0, 0.0, 0.0),
            ),
            _ => {
                let f1 = Box4::new(box_r.x + 16.0, box_r.y + 62.0, mw - 32.0, 30.0);
                let f2 = match modal {
                    SettingsModal::ArtistAlias { .. } => {
                        Some(Box4::new(box_r.x + 16.0, box_r.y + 118.0, mw - 32.0, 30.0))
                    }
                    _ => None,
                };
                let btn_y = box_r.bottom() - 42.0;
                let cancel = Box4::new(box_r.right() - 152.0, btn_y, 64.0, 28.0);
                let save = Box4::new(box_r.right() - 80.0, btn_y, 64.0, 28.0);
                (f1, f2, cancel, save)
            }
        };
        (box_r, f1, f2, cancel, save)
    }
    pub fn handle_modal_click(&mut self, x: f32, y: f32, panel_rect: Box4, offset: [f32; 2], now: f64) -> bool {
        let Some(modal) = self.modal.clone() else {
            return false;
        };
        let (box_r, f1, f2, cancel, save) = Self::modal_rects_with_offset(panel_rect, &modal, offset);
        if !box_r.contains(x, y) || cancel.contains(x, y) {
            self.modal = None;
            return true;
        }
        if save.contains(x, y) {
            self.commit_modal(now);
            return true;
        }
        if f1.contains(x, y) {
            if let Some(SettingsModal::ArtistAlias { focus, .. }) = self.modal.as_mut() {
                *focus = 0;
            }
            return true;
        }
        if f2.is_some_and(|r| r.contains(x, y)) {
            if let Some(SettingsModal::ArtistAlias { focus, .. }) = self.modal.as_mut() {
                *focus = 1;
            }
            return true;
        }
        false
    }
    pub fn commit_modal(&mut self, now: f64) -> bool {
        let Some(modal) = self.modal.take() else {
            return false;
        };
        match modal {
            SettingsModal::CustomModel { provider, draft } => {
                self.add_custom_model(&provider, &draft, now)
            }
            SettingsModal::ArtistAlias { from, to, .. } => {
                self.add_artist_alias(&from, &to, now)
            }
            SettingsModal::PrivacyPolicy => false,
        }
    }
    pub fn clear_modal_field(&mut self) {
        match self.modal.as_mut() {
            Some(SettingsModal::CustomModel { draft, .. }) => draft.clear(),
            Some(SettingsModal::ArtistAlias { from, to, focus }) => {
                if *focus == 0 {
                    from.clear();
                } else {
                    to.clear();
                }
            }
            Some(SettingsModal::PrivacyPolicy) | None => {}
        }
    }
    pub fn handle_modal_key(&mut self, key: &str, now: f64) -> bool {
        if self.modal.is_none() {
            return false;
        }
        match key {
            "escape" => {
                self.modal = None;
                true
            }
            "enter" | "return" => {
                if let Some(SettingsModal::ArtistAlias { focus, .. }) = self.modal.as_mut() {
                    if *focus == 0 {
                        *focus = 1;
                        return true;
                    }
                }
                self.commit_modal(now);
                true
            }
            "tab" => {
                if let Some(SettingsModal::ArtistAlias { focus, .. }) = self.modal.as_mut() {
                    *focus = if *focus == 0 { 1 } else { 0 };
                }
                true
            }
            "backspace" => {
                match self.modal.as_mut() {
                    Some(SettingsModal::CustomModel { draft, .. }) => {
                        draft.pop();
                    }
                    Some(SettingsModal::ArtistAlias { from, to, focus }) => {
                        if *focus == 0 {
                            from.pop();
                        } else {
                            to.pop();
                        }
                    }
                    Some(SettingsModal::PrivacyPolicy) | None => {}
                }
                true
            }
            "delete" => {
                self.clear_modal_field();
                true
            }
            "space" => {
                self.insert_modal_text(" ");
                true
            }
            _ if key.chars().count() == 1 => {
                self.insert_modal_text(key);
                true
            }
            _ => false,
        }
    }
    pub fn insert_modal_text(&mut self, text: &str) -> bool {
        let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
        if cleaned.is_empty() {
            return false;
        }
        match self.modal.as_mut() {
            Some(SettingsModal::CustomModel { draft, .. }) => {
                draft.push_str(&cleaned);
                true
            }
            Some(SettingsModal::ArtistAlias { from, to, focus }) => {
                if *focus == 0 {
                    from.push_str(&cleaned);
                } else {
                    to.push_str(&cleaned);
                }
                true
            }
            Some(SettingsModal::PrivacyPolicy) | None => false,
        }
    }
    pub fn activate_row(&mut self, index: usize, now: f64) -> bool {
        let Some(item) = self.items().get(index).copied() else {
            return false;
        };
        match item.control {
            Control::Switch => {
                self.close_choice();
                self.cycle(index, now);
                true
            }
            Control::Segmented(_) => {
                self.close_choice();
                self.cycle(index, now);
                true
            }
            Control::Choice(_) => {
                self.finish_edit(now, true);
                if self.open_choice == Some(index) {
                    self.close_choice();
                } else {
                    self.open_choice = Some(index);
                    self.set_choice_scroll_immediate(0.0);
                    self.choice_hover=None;
                }
                false
            }
            Control::Text { .. } => self.begin_edit(index),
            Control::Slider { .. } | Control::PlayerOffset(_) | Control::Info(_) => false,
            Control::Action { .. } => {
                self.finish_edit(now, true);
                self.close_choice();
                match item.key {
                    "lyrics.addArtistAlias" => {
                        let prefill = self.current_track_artist.trim().to_string();
                        let focus = if prefill.is_empty() { 0 } else { 1 };
                        self.modal = Some(SettingsModal::ArtistAlias {
                            from: prefill,
                            to: String::new(),
                            focus,
                        });
                    }
                    "action.privacyPolicy" => {
                        self.modal = Some(SettingsModal::PrivacyPolicy);
                    }
                    "action.resetDefaults" => {
                        if self.reset_confirm_until_ms.is_some_and(|t| now <= t) {
                            self.reset_confirm_until_ms = None;
                            self.pending_action = Some("action.resetDefaults");
                        } else {
                            self.reset_confirm_until_ms = Some(now + 3000.0);
                            self.status = "再次点击「恢复默认」以确认重置".to_string();
                        }
                    }
                    _ => {
                        self.pending_action = Some(item.key);
                    }
                }
                true
            }
            Control::AliasRow(alias_idx) => {
                self.close_choice();
                if let Some((from, _)) = self.artist_aliases().get(alias_idx).cloned() {
                    self.remove_artist_alias(&from, now);
                }
                true
            }
        }
    }
    pub fn select_choice_option(&mut self, opt_index: usize, now: f64) -> bool {
        let Some(row_idx) = self.open_choice else {
            return false;
        };
        let Some(item) = self.items().get(row_idx).copied() else {
            self.close_choice();
            return false;
        };
        let Control::Choice(options) = item.control else {
            self.close_choice();
            return false;
        };
        if item.key == "lyrics.aiModel" {
            let provider = self.value("lyrics.aiPreset").as_str().unwrap_or("deepseek").to_owned();
            let opts = self.model_options_for(&provider);
            let Some((key, _, _)) = opts.get(opt_index).cloned() else {
                self.close_choice();
                return false;
            };
            self.close_choice();
            if key == "__custom_add__" {
                self.finish_edit(now, true);
                self.modal = Some(SettingsModal::CustomModel {
                    provider,
                    draft: String::new(),
                });
                return false;
            }
            self.set(item.key, json!(key), now, false);
            return true;
        }
        let Some((key, _)) = options.get(opt_index).copied() else {
            self.close_choice();
            return false;
        };
        self.set(item.key, json!(key), now, false);
        self.close_choice();
        true
    }
    pub fn dropdown_layout(&self, panel_rect: Box4) -> Option<DropdownPresentation> {
        let row_idx = self.open_choice?;
        let item = self.items().get(row_idx).copied()?;
        let Control::Choice(opts) = item.control else {
            return None;
        };
        let dynamic_opts: Vec<(String, String, bool)> = if item.key == "lyrics.aiModel" {
            let provider = self.value("lyrics.aiPreset").as_str().unwrap_or("deepseek").to_owned();
            self.model_options_for(&provider)
        } else {
            opts.iter().map(|&(k, l)| (k.to_string(), l.to_string(), false)).collect()
        };
        let c = self.content_box(panel_rect);
        let (_, _, row) = self.row_rects(panel_rect).into_iter().find(|(i, _, _)| *i == row_idx)?;
        if row.bottom() < c.y || row.y > c.bottom() {
            return None;
        }
        let pill = Self::choice_pill_rect(row);
        const OPT_H: f32 = 31.0;
        const PAD: f32 = 5.0;
        let total_h = dynamic_opts.len() as f32 * OPT_H + PAD * 2.0;
        let window_bottom = if self.viewport_h > panel_rect.bottom() + 8.0 {
            self.viewport_h - 8.0
        } else {
            panel_rect.bottom() - 8.0
        };
        let min_top = if self.viewport_h > panel_rect.bottom() + 8.0 {
            8.0
        } else {
            c.y
        };
        let max_span = (window_bottom - min_top).max(72.0);
        let list_h = total_h.min(max_span);
        let below_y = pill.bottom() + 4.0;
        let above_y = pill.y - 4.0 - list_h;
        let list_y = if below_y + list_h <= window_bottom {
            below_y
        } else if above_y >= min_top {
            above_y
        } else {
            (window_bottom - list_h).max(min_top)
        };
        let list_w = pill.w.max(204.0).min((panel_rect.w - 24.0).max(140.0));
        let list_x = (pill.right() - list_w).max(panel_rect.x + 12.0);
        let rect = Box4::new(list_x, list_y, list_w, list_h);
        let max_scroll = (total_h - list_h).max(0.0);
        let scroll = self.choice_scroll.clamp(0.0, max_scroll);
        let current_val = self.value(item.key);
        let selected_idx = dynamic_opts
            .iter()
            .position(|(k, _, _)| Some(k.as_str()) == current_val.as_str())
            .unwrap_or(0);
        let is_color = item.key == "uiAccentColor";
        let options = dynamic_opts
            .iter()
            .enumerate()
            .map(|(i, (k, label, is_custom))| {
                let opt_r = Box4::new(
                    rect.x + PAD,
                    rect.y + PAD + i as f32 * OPT_H - scroll,
                    (rect.w - PAD * 2.0).max(20.0),
                    OPT_H - 2.0,
                );
                (i, k.clone(), label.clone(), opt_r, i == selected_idx, *is_custom)
            })
            .collect();
        let active=self.choice_hover.unwrap_or(selected_idx).min(dynamic_opts.len().saturating_sub(1));
        let lens_rect=Box4::new(rect.x+PAD,rect.y+PAD+active as f32*OPT_H-scroll+1.,(rect.w-PAD*2.).max(20.),OPT_H-2.);
        Some(DropdownPresentation {
            row_index: row_idx,
            option_count: dynamic_opts.len(),
            rect,
            options,
            lens_rect,
            lens_swell:0.,
            max_scroll,
            scroll,
            selected_idx,
            is_color,
            glass: None,
        })
    }
    pub fn step_dropdown(&mut self,_panel_rect:Box4,hovered_opt:Option<usize>,_held:bool,_dt_ms:f64){self.choice_hover=hovered_opt;}
    pub fn shared_store(&self)->Option<Arc<ConfigStore>>{self.store.clone()}
    pub fn preview_switch(&mut self,index:usize,fraction:f32){
        if let Some(item)=self.items().get(index).filter(|i|matches!(i.control,Control::Switch)){
            self.switch_preview=Some((item.key,fraction.clamp(0.,1.)));
            if let Some(spring)=self.switch_springs.get_mut(item.key){spring.set_position(fraction as f64);}
        }
    }
    pub fn finish_switch(&mut self,index:usize,fraction:f32,moved:bool,now:f64){
        self.switch_preview=None;
        if let Some(item)=self.items().get(index).filter(|i|matches!(i.control,Control::Switch)){
            let on=if moved{fraction>=0.5}else{!self.value(item.key).as_bool().unwrap_or(false)};
            self.set(item.key,json!(on),now,false);
        }
    }
    pub fn slide(&mut self, index: usize, fraction: f32, now: f64) {
        if let Some(item) = self.items().get(index).copied() {
            if let Some((min, max, step)) = item.control.slider_bounds() {
                let raw = min + (max - min) * fraction.clamp(0., 1.) as f64;
                let rounded = ((raw / step).round() * step).clamp(min, max);
                let v = if rounded.abs() < 1e-6 { 0.0 } else { (rounded * 1e6).round() / 1e6 };
                if let Control::PlayerOffset(idx) = item.control {
                    if let Some(bucket) = self.saved_player_offsets().get(idx).cloned() {
                        let key = format!("lyrics.playerSyncOffsets.{bucket}");
                        self.set(&key, json!(v), now, true);
                    }
                } else {
                    self.set(item.key, json!(v), now, true);
                }
            }
        }
    }
    pub fn cycle(&mut self, index: usize, now: f64) {
        if let Some(item) = self.items().get(index) {
            let v = self.value(item.key);
            match item.control {
                Control::Switch => {
                    self.set(item.key, json!(!v.as_bool().unwrap_or(false)), now, false)
                }
                Control::Segmented(options) | Control::Choice(options) => {
                    let current = options
                        .iter()
                        .position(|(k, _)| Some(*k) == v.as_str())
                        .unwrap_or(0);
                    self.set(
                        item.key,
                        json!(options[(current + 1) % options.len()].0),
                        now,
                        false,
                    );
                }
                _ => {}
            }
        }
    }
    pub fn tick(&mut self, now: f64) {
        let dt_ms = self
            .last_tick_ms
            .map(|prev| (now - prev).clamp(0.0, 100.0))
            .unwrap_or(16.0);
        self.last_tick_ms = Some(now);
        self.scroll_spring.update(dt_ms / 1000.0);
        self.scroll = self.scroll_spring.position().max(0.0) as f32;
        self.choice_scroll_spring.update(dt_ms / 1000.0);
        self.choice_scroll = self.choice_scroll_spring.position().max(0.0) as f32;
        if self.status != self.last_status {
            self.last_status = self.status.clone();
            if self.status.is_empty() {
                self.status_at_ms = None;
            } else {
                self.status_at_ms = Some(now);
            }
        }
        let toast_target = if self.status_at_ms.is_some_and(|t| now - t < 2600.0) {
            1.0
        } else {
            0.0
        };
        self.status_alpha = super::model::step_opacity(
            self.status_alpha,
            toast_target,
            dt_ms,
            if toast_target > 0.5 { 160.0 } else { 260.0 },
            false,
        );
        if toast_target == 0.0 && self.status_alpha <= 0.002 && !self.status.is_empty() {
            self.status.clear();
            self.last_status.clear();
            self.status_at_ms = None;
        }
        for page_idx in 0..self.pages().len() {
            for item in self.items_on(page_idx) {
                if matches!(item.control, Control::Switch) {
                    let on = self.value(item.key).as_bool().unwrap_or(false);
                    let target = self.switch_preview.filter(|(key,_)|*key==item.key).map(|(_,f)|f as f64).unwrap_or(if on { 1.0 } else { 0.0 });
                    let spring = self.switch_springs.entry(item.key).or_insert_with(|| {
                        let mut s = oly_core::scroll::Spring::new(target);
                        s.update_params(oly_core::scroll::SpringParams::new(360.0, 28.0));
                        s
                    });
                    spring.set_target_position(target);
                    spring.update(dt_ms / 1000.0);
                }
            }
        }
        if self.outgoing_page.is_some() {
            let step = (dt_ms as f32 / 240.0).max(0.0);
            self.page_anim = (self.page_anim + step).min(1.0);
            if self.page_anim >= 1.0 {
                self.outgoing_page = None;
            }
        }
        if self.pending() && now >= self.deadline {
            let patch = std::mem::replace(&mut self.pending, json!({}));
            self.status = match &self.store {
                Some(store) => match store.patch(patch) {
                    Ok(_) => String::new(),
                    Err(e) => format!("保存失败：{e}"),
                },
                None => "回放预览 · 本次会话生效".into(),
            };
        }
    }
    pub fn step_segmented(&mut self, rect: Box4, dt_ms: f64) {
        for (_, item, row) in self.row_rects(rect) {
            if let Control::Segmented(options) = item.control {
                let count = options.len().max(1);
                let track = Self::segmented_track_rect(row, count);
                let val = self.value(item.key);
                let selected = options
                    .iter()
                    .position(|(k, _)| Some(*k) == val.as_str())
                    .unwrap_or(0);
                let held = self.segmented_drag.is_some_and(|(k, _)| k == item.key);
                let target_slot = self
                    .segmented_drag
                    .filter(|(k, _)| *k == item.key)
                    .map(|(_, s)| s)
                    .unwrap_or(selected)
                    .min(count.saturating_sub(1));
                let cell_w = track.w / count as f32;
                let slot_x = target_slot as f32 * cell_w + 2.0;
                let slot_w = (cell_w - 4.0).max(16.0);
                let lens = self.segmented_lenses.entry(item.key).or_default();
                lens.step(slot_x, slot_w, held, dt_ms);
            }
        }
    }
    pub fn segmented_track_rect(row: Box4, count: usize) -> Box4 {
        let count = count.max(1);
        let preferred = if count >= 3 {
            (row.w * 0.56).clamp(156.0, 210.0)
        } else {
            (row.w * 0.48).clamp(132.0, 180.0)
        };
        let w = preferred.min((row.w - 104.0).max(110.0));
        Box4::new(row.right() - 8.0 - w, row.y + (row.h - 28.0) * 0.5, w, 28.0)
    }
    pub fn begin_segmented_drag(&mut self, key: &'static str, x: f32, track: Box4, count: usize) {
        let lens = self.segmented_lenses.entry(key).or_default();
        lens.update_drag(x, track.x, track.w, count, 2.0);
        let slot = lens.slot_at(track.x, track.w, count, x);
        self.segmented_drag = Some((key, slot));
    }
    pub fn update_segmented_drag(&mut self, key: &'static str, x: f32, track: Box4, count: usize) {
        let lens = self.segmented_lenses.entry(key).or_default();
        lens.update_drag(x, track.x, track.w, count, 2.0);
        let slot = lens.slot_at(track.x, track.w, count, x);
        self.segmented_drag = Some((key, slot));
    }
    pub fn finish_segmented_drag(
        &mut self,
        index: usize,
        x: f32,
        track: Box4,
        count: usize,
        now: f64,
    ) -> bool {
        self.segmented_drag = None;
        let Some(item) = self.items().get(index).copied() else {
            return false;
        };
        let Control::Segmented(options) = item.control else {
            return false;
        };
        let slot = if let Some(lens) = self.segmented_lenses.get_mut(item.key) {
            let s = lens.slot_at_release(track.x, track.w, count, x);
            lens.end_drag();
            s
        } else {
            0
        };
        if let Some((k, _)) = options.get(slot).copied() {
            self.set(item.key, json!(k), now, false);
            return true;
        }
        false
    }
    pub fn cancel_segmented_drag(&mut self) {
        if let Some((key, _)) = self.segmented_drag.take() {
            if let Some(lens) = self.segmented_lenses.get_mut(key) {
                lens.end_drag();
            }
        }
    }
    pub fn step_tabs(&mut self, rect: Box4, held: bool, dt_ms: f64) {
        let count = self.pages().len() as f32;
        if self.kind == SettingsKind::Application {
            let cell_h = 38.0;
            let slot_y = self.page as f32 * cell_h + 3.0;
            self.tab_lens.step(slot_y, 28.0, held, dt_ms);
        } else {
            let track_w = rect.w - 24.0;
            let cell_w = track_w / count;
            let slot_x = self.page as f32 * cell_w + 5.0;
            let slot_w = (cell_w - 10.0).max(20.0);
            self.tab_lens.step(slot_x, slot_w, held, dt_ms);
        }
    }
    pub fn sidebar_width_for(kind: SettingsKind, r: Box4) -> f32 {
        match kind {
            SettingsKind::Lyrics => 0.0,
            SettingsKind::Application => (r.w * 0.24).clamp(96.0, 126.0),
        }
    }
    pub fn panel_for(kind: SettingsKind, w: f32, h: f32) -> Box4 {
        match kind {
            SettingsKind::Lyrics => {
                if w <= 580.0 {
                    Box4::new(0., 0., 380f32.min(w), h)
                } else {
                    let pw = 380.0_f32.min((w - 24.0).max(300.0)).min(w);
                    let ph = 520.0_f32.min((h - 24.0).max(320.0)).min(h);
                    Box4::new(12.0, ((h - ph) * 0.5).max(12.0), pw, ph)
                }
            }
            SettingsKind::Application => {
                if w <= 580.0 {
                    Box4::new(0., 0., w, h)
                } else {
                    let pw = 540.0_f32.min((w - 24.0).max(320.0)).min(w);
                    let ph = 440.0_f32.min((h - 24.0).max(280.0)).min(h);
                    Box4::new(12.0, ((h - ph) * 0.5).max(12.0), pw, ph)
                }
            }
        }
    }
    pub fn panel(w: f32, h: f32) -> Box4 {
        Self::panel_for(SettingsKind::Lyrics, w, h)
    }
    pub fn panel_rect(&self, w: f32, h: f32) -> Box4 {
        Self::panel_for(self.kind, w, h)
    }
    pub fn close_rect_for(kind: SettingsKind, r: Box4) -> Box4 {
        match kind {
            SettingsKind::Lyrics => Box4::new(r.right() - 44., r.y + 10., 30., 30.),
            SettingsKind::Application => Box4::new(r.right() - 40., r.y + 12., 28., 28.),
        }
    }
    pub fn tab_rect(r: Box4, i: usize) -> Box4 {
        Self::tab_rect_for_kind(SettingsKind::Lyrics, r, i, PAGES.len())
    }
    pub fn tab_rect_for(&self, r: Box4, i: usize) -> Box4 {
        Self::tab_rect_for_kind(self.kind, r, i, self.pages().len())
    }
    fn tab_rect_for_kind(kind: SettingsKind, r: Box4, i: usize, count: usize) -> Box4 {
        match kind {
            SettingsKind::Lyrics => {
                let cell_w = (r.w - 24.0) / count.max(1) as f32;
                Box4::new(
                    r.x + 12.0 + i as f32 * cell_w + 2.0,
                    r.y + 54.0,
                    (cell_w - 4.0).max(24.0),
                    30.0,
                )
            }
            SettingsKind::Application => {
                let sw = Self::sidebar_width_for(kind, r);
                Box4::new(
                    r.x + 14.0,
                    r.y + 52.0 + i as f32 * 38.0,
                    (sw - 24.0).max(40.0),
                    34.0,
                )
            }
        }
    }
    pub fn content_for(kind: SettingsKind, rect: Box4) -> Box4 {
        match kind {
            SettingsKind::Lyrics => Box4::new(
                rect.x + 12.,
                rect.y + 96.,
                rect.w - 24.,
                (rect.h - 110.).max(10.),
            ),
            SettingsKind::Application => {
                let sw = Self::sidebar_width_for(kind, rect);
                Box4::new(
                    rect.x + sw + 14.,
                    rect.y + 52.,
                    (rect.w - sw - 28.).max(120.),
                    (rect.h - 66.).max(40.),
                )
            }
        }
    }
    pub fn content(rect: Box4) -> Box4 {
        Self::content_for(SettingsKind::Lyrics, rect)
    }
    pub fn content_box(&self, rect: Box4) -> Box4 {
        Self::content_for(self.kind, rect)
    }
    pub fn row_height(item: Item) -> f32 {
        if item.control.slider_bounds().is_some() { 52.0 } else { 42.0 }
    }
    pub fn choice_pill_rect(row: Box4) -> Box4 {
        let pill_w = (row.w * 0.46).clamp(112.0, 186.0).min((row.w - 92.0).max(80.0));
        Box4::new(row.right() - 8.0 - pill_w, row.y + (row.h - 28.0) * 0.5, pill_w, 28.0)
    }
    fn section_label(key: &str) -> Option<&'static str> {
        match key {
            "themeMode" => Some("主题与封面"),
            "windowBackground" => Some("背景与窗口"),
            "fontSize" => Some("字号与间距"),
            "align" => Some("对齐与内容"),
            "fontFamily" => Some("字体"),
            "enableSpring" => Some("动效"),
            "idleLineOpacity" => Some("亮度与羽化"),
            "lyrics.enabledSources.kugou" => Some("在线歌词源"),
            "lyrics.matchThreshold" => Some("匹配与缓存"),
            "lyrics.autoTranslate" => Some("翻译"),
            "lyrics.playerSyncOffsets.apple" => Some("播放器同步"),
            "action.clearLyricsCache" => Some("缓存与数据"),
            "action.resetDefaults" => Some("重置"),
            "about.author" => Some("关于 OverLyrics"),
            "action.privacyPolicy" => Some("法律与隐私"),
            _ => None,
        }
    }
    fn row_rects_for(&self, page: usize, rect: Box4, scroll: f32) -> Vec<(usize, Item, Box4)> {
        let c = self.content_box(rect);
        let mut y = c.y - scroll;
        let items = self.items_on(page);
        let mut out = Vec::with_capacity(items.len());
        let mut has_sync_header = false;
        let mut has_alias_header = false;
        for (i, item) in items.into_iter().enumerate() {
            let header = if matches!(item.key, "lyrics.playerSyncOffsets.apple")
                || item.key == ACTIVE_PLAYER_OFFSET_KEY
                || item.key == CUSTOM_PLAYER_OFFSET_KEY
            {
                if !has_sync_header {
                    has_sync_header = true;
                    Some("播放器同步")
                } else {
                    None
                }
            } else if item.key == "lyrics.artistAliases" {
                if !has_alias_header {
                    has_alias_header = true;
                    Some("歌手名映射")
                } else {
                    None
                }
            } else if item.key == "lyrics.addArtistAlias" && !has_alias_header {
                has_alias_header = true;
                Some("歌手名映射")
            } else {
                Self::section_label(item.key)
            };
            if header.is_some() {
                if i > 0 {
                    y += 10.0;
                }
                y += 24.0;
            }
            let h = Self::row_height(item);
            let r = Box4::new(c.x + 2.0, y, (c.w - 4.0).max(40.0), h);
            y += h + 4.0;
            out.push((i, item, r));
        }
        out
    }
    fn row_rects(&self, rect: Box4) -> Vec<(usize, Item, Box4)> {
        self.row_rects_for(self.page, rect, self.scroll)
    }
    fn rows_for(&self, page: usize, rect: Box4, scroll: f32) -> Vec<(usize, Item, Box4)> {
        let c = self.content_box(rect);
        self.row_rects_for(page, rect, scroll)
            .into_iter()
            .filter(|(_, _, r)| r.bottom() > c.y && r.y < c.bottom())
            .collect()
    }
    pub fn max_scroll(&self, rect: Box4) -> f32 {
        let c = self.content_box(rect);
        let rects = self.row_rects_for(self.page, rect, 0.0);
        let total_bottom = rects.last().map(|(_, _, r)| r.bottom() + 8.0).unwrap_or(c.y);
        (total_bottom - c.y - c.h).max(0.0)
    }
    pub fn rows(&self, rect: Box4) -> Vec<(usize, Item, Box4)> {
        self.rows_for(self.page, rect, self.scroll)
    }
    pub fn numeric(&self, item: Item) -> f64 {
        self.value(item.key).as_f64().unwrap_or_else(|| {
            SLIDERS
                .iter()
                .find(|s| s.key == item.key)
                .map(|s| s.fallback)
                .unwrap_or(0.)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_target_the_active_shape_and_keep_base_values() {
        let mut s = Settings::default();
        let base = s.config["display"]["fontSize"].clone();
        s.set("fontSize", json!(125), 0., true);
        assert_eq!(s.config["display"]["fontSize"], base);
        assert_eq!(s.effective()["fontSize"], 125);
        s.config["display"]["showCover"] = json!(false);
        s.set("fontSize", json!(75), 1., true);
        assert_eq!(s.effective()["fontSize"], 75);
        assert_eq!(
            s.config["display"]["typeProfiles"]["cover"]["fontSize"],
            125
        );
    }
    #[test]
    fn draft_updates_immediately_and_settles_without_writing_replay() {
        let mut s = Settings::default();
        s.slide(0, 1.5, 0.);
        assert_eq!(s.effective()["fontSize"], 200.);
        assert!(s.pending());
        s.tick(119.);
        assert!(s.pending());
        s.tick(120.);
        assert!(!s.pending());
        assert!(s.status.contains("回放"));
    }
}

pub struct DropdownPresentation {
    pub row_index: usize,
    pub option_count: usize,
    pub rect: Box4,
    pub options: Vec<(usize, String, String, Box4, bool, bool)>,
    pub lens_rect: Box4,
    pub lens_swell: f32,
    pub max_scroll: f32,
    pub scroll: f32,
    pub selected_idx: usize,
    pub is_color: bool,
    pub glass: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
}

impl DropdownPresentation {
    pub fn translate(&mut self, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.rect.x += dx;
        self.rect.y += dy;
        self.lens_rect.x += dx;
        self.lens_rect.y += dy;
        for (_, _, _, opt_r, _, _) in &mut self.options {
            opt_r.x += dx;
            opt_r.y += dy;
        }
    }
}

pub struct SegmentedPresentation {
    pub track_rect: Box4,
    pub well_rect: Box4,
    pub lens_rect: Box4,
    pub swell: f32,
    pub active_slot: usize,
    pub glass: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
}

pub struct Presentation {
    pub kind: SettingsKind,
    pub pages: &'static [&'static str],
    pub editing_key: Option<&'static str>,
    pub editing_draft: String,
    pub caret_visible: bool,
    pub rect: Box4,
    pub page: usize,
    pub title: &'static str,
    pub rows: Vec<(usize, Item, Box4)>,
    pub outgoing_rows: Vec<(usize, Item, Box4)>,
    pub page_progress: f32,
    pub page_dir: i8,
    pub values: Value,
    pub status: String,
    pub scroll: f32,
    pub max_scroll: f32,
    pub tab_well_rect: Box4,
    pub tab_lens_rect: Box4,
    pub tab_swell: f32,
    pub tab_lens_glass: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
    pub close_lens: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
    pub dropdown: Option<DropdownPresentation>,
    pub segmented: std::collections::HashMap<&'static str, SegmentedPresentation>,
    pub accent_hex: u32,
    pub switch_fractions: std::collections::HashMap<&'static str, f32>,
    pub active_player_bucket: String,
    pub active_profile: &'static str,
    pub active_mode_label: &'static str,
    pub artist_aliases: Vec<(String, String)>,
    pub custom_player_offsets: Vec<(String, f64)>,
    pub custom_models: Vec<String>,
    pub modal: Option<SettingsModal>,
    pub modal_offset: [f32; 2],
    pub modal_glass: Option<(
        oly_render::glass::Rect,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::CustomSurfaceHandle,
        gpui_kit::PaintHook,
    )>,
    pub theme_mode: super::model::ThemeMode,
    pub status_alpha: f32,
    pub reset_armed: bool,
}
impl Settings {
    pub fn presentation(&self, rect: Box4) -> Presentation {
        let accent_hex = Display::from_config(&self.config).accent_hex;
        let mut values = self.effective();
        for item in self.items() { values[item.key] = self.value(item.key); }
        let mut switch_fractions = std::collections::HashMap::new();
        for item in self.items() {
            if matches!(item.control, Control::Switch) {
                let fallback = if values
                    .get(item.key)
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    1.0
                } else {
                    0.0
                };
                let frac = self
                    .switch_springs
                    .get(item.key)
                    .map(|s| s.position() as f32)
                    .unwrap_or(fallback);
                let frac=self.switch_preview.filter(|(key,_)|*key==item.key).map(|(_,f)|f).unwrap_or(frac);
                switch_fractions.insert(item.key, frac);
            }
        }
        let (tab_well_rect, tab_lens_rect) = if self.kind == SettingsKind::Application {
            let sw = Self::sidebar_width_for(self.kind, rect);
            let count = self.pages().len() as f32;
            let base_well = Box4::new(rect.x + 11.0, rect.y + 49.0, (sw - 18.0).max(40.0), count * 38.0 + 2.0);
            (
                self.tab_lens.well_rect_axis(base_well, true),
                self.tab_lens.v_rect(
                    rect.x + 18.0,
                    rect.y + 52.0,
                    (sw - 32.0).max(36.0),
                    super::interaction::Feedback::default(),
                ),
            )
        } else {
            let base_well = Box4::new(rect.x + 10.0, rect.y + 52.0, rect.w - 20.0, 34.0);
            (
                self.tab_lens.well_rect_axis(base_well, false),
                self.tab_lens.rect(
                    rect.x + 12.0,
                    rect.y + 57.0,
                    24.0,
                    super::interaction::Feedback::default(),
                ),
            )
        };
        let rows = self.rows(rect);
        let mut segmented = std::collections::HashMap::new();
        for (_, item, row) in &rows {
            if let Control::Segmented(options) = item.control {
                let count = options.len().max(1);
                let track = Self::segmented_track_rect(*row, count);
                let selected = options
                    .iter()
                    .position(|(k, _)| Some(*k) == values.get(item.key).and_then(Value::as_str))
                    .unwrap_or(0);
                let (well_rect, lens_rect, swell, active_slot) =
                    if let Some(lens) = self.segmented_lenses.get(item.key) {
                        let cell_w = track.w / count as f32;
                        let lens_rect = lens.rect(
                            track.x,
                            track.y + 3.0,
                            (track.h - 6.0).max(16.0),
                            super::interaction::Feedback::default(),
                        );
                        let well_rect = lens.well_rect_axis(track, false);
                        let swell = lens.swell();
                        let held = self.segmented_drag.is_some_and(|(k, _)| k == item.key);
                        let active_slot = if held || lens.pending() || swell > 0.02 {
                            (((lens_rect.centre().0 - track.x) / cell_w.max(1.0)).floor() as isize)
                                .clamp(0, count.saturating_sub(1) as isize) as usize
                        } else {
                            selected
                        };
                        (well_rect, lens_rect, swell, active_slot)
                    } else {
                        let cell_w = track.w / count as f32;
                        let lens_rect = Box4::new(
                            track.x + selected as f32 * cell_w + 2.0,
                            track.y + 3.0,
                            (cell_w - 4.0).max(16.0),
                            (track.h - 6.0).max(16.0),
                        );
                        (track, lens_rect, 0.0, selected)
                    };
                segmented.insert(
                    item.key,
                    SegmentedPresentation {
                        track_rect: track,
                        well_rect,
                        lens_rect,
                        swell,
                        active_slot,
                        glass: None,
                    },
                );
            }
        }
        Presentation {
            kind: self.kind,
            pages: self.pages(),
            editing_key: self.editing_key,
            editing_draft: self.editing_draft.clone(),
            caret_visible: (self.editing_key.is_some() || self.modal.is_some())
                && ((self.last_tick_ms.unwrap_or(0.0) / 530.0) as u64).is_multiple_of(2),
            rect,
            page: self.page,
            title: self.title,
            rows,
            outgoing_rows: self
                .outgoing_page
                .filter(|_| self.page_anim < 1.0)
                .map(|prev| self.rows_for(prev, rect, self.outgoing_scroll))
                .unwrap_or_default(),
            page_progress: self.page_anim,
            page_dir: self.page_dir,
            values,
            status: self.status.clone(),
            scroll: self.scroll,
            max_scroll: self.max_scroll(rect),
            tab_well_rect,
            tab_lens_rect,
            tab_swell: self.tab_lens.swell(),
            tab_lens_glass: None,
            close_lens: None,
            dropdown: self.dropdown_layout(rect),
            segmented,
            accent_hex,
            switch_fractions,
            active_player_bucket: self.active_player_bucket(),
            active_profile: self.active_profile(),
            active_mode_label: self.display_mode_label.unwrap_or_else(|| match self.active_profile() {
                "nocover" => "无封面模式",
                "mini" => "迷你模式",
                _ => "窗口模式",
            }),
            artist_aliases: self.artist_aliases(),
            custom_player_offsets: self
                .saved_player_offsets()
                .into_iter()
                .map(|b| {
                    let val = self
                        .config
                        .pointer(&format!("/lyrics/playerSyncOffsets/{b}"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    (b, val)
                })
                .collect(),
            custom_models: {
                let provider = self.value("lyrics.aiPreset").as_str().unwrap_or("deepseek").to_owned();
                self.custom_models_for(&provider)
            },
            modal: self.modal.clone(),
            modal_offset: [0.0, 0.0],
            modal_glass: None,
            theme_mode: Display::from_config(&self.config).theme_mode,
            status_alpha: self.status_alpha,
            reset_armed: self
                .reset_confirm_until_ms
                .is_some_and(|t| self.last_tick_ms.unwrap_or(0.0) <= t),
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub fn paint(
    window: &mut gpui_kit::Window,
    cx: &gpui_kit::App,
    p: &Presentation,
    opacity: f32,
    feedback: impl Fn(u8) -> super::interaction::Feedback,
    choice_feedback: impl Fn(u8) -> super::interaction::Feedback,
    page_feedback: impl Fn(u8) -> super::interaction::Feedback,
    dropdown_feedback: impl Fn(u8) -> super::interaction::Feedback,
    close: super::interaction::Feedback,
    modal_btns: [super::interaction::Feedback; 2],
) {
    use super::chrome;
    let r = p.rect;
    let c = Settings::content_for(p.kind, r);
    let color = |a| chrome::text(a * opacity);
    let close_box = Settings::close_rect_for(p.kind, r);
    let sw = Settings::sidebar_width_for(p.kind, r);
    if p.kind == SettingsKind::Application {
        let header_cy = close_box.centre().1;
        chrome::fill_round(window, r.x + sw - 1.0, r.y + 14.0, 0.85, (r.h - 28.0).max(16.0), 0.4, color(0.08));
        chrome::paint_text_vcentred(window, p.title, r.x + 18., header_cy, 13., 600., color(0.55));
        let page_label = p.pages.get(p.page).copied().unwrap_or(p.title);
        chrome::paint_text_vcentred(window, page_label, c.x + 2., header_cy, 17., 700., color(0.96));
        if matches!(p.page, 1 | 2) {
            let profile_label = p.active_mode_label;
            let title_w = chrome::measure(window, page_label, 17., 700.).0;
            let badge_w = chrome::measure(window, profile_label, 11., 550.).0 + 14.0;
            let bx = c.x + 2. + title_w + 10.;
            if bx + badge_w < close_box.x - 8.0 {
                let badge_r = Box4::new(bx, header_cy - 10.0, badge_w, 20.0);
                chrome::fill_round(window, badge_r.x, badge_r.y, badge_r.w, badge_r.h, 6., color(0.075));
                chrome::paint_text_box_centred(window, profile_label, badge_r, 11., 550., color(0.66));
            }
        }
    } else {
        chrome::paint_text_vcentred(window, p.title, r.x + 18., close_box.centre().1, 17., 650., color(0.96));
    }
    if let Some((grect, surface, snapshot, hook)) = p.close_lens {
        window.paint_custom_surface_with_scene(
            gpui_kit::Bounds {
                origin: gpui_kit::point(gpui_kit::px(grect.left as f32), gpui_kit::px(grect.top as f32)),
                size: gpui_kit::size(gpui_kit::px(grect.width as f32), gpui_kit::px(grect.height as f32)),
            },
            surface,
            snapshot,
            hook,
        );
    }
    if p.close_lens.is_some() {
        let press = close.press.clamp(0.0, 1.0);
        let scale_btn = (0.90 + 0.14 * press).max(0.85);
        let lw = close_box.w * scale_btn * (1.0 + close.squeeze[0] * 0.72);
        let lh = close_box.h * scale_btn * (1.0 + close.squeeze[1] * 0.72);
        let cx_b = close_box.x + close_box.w * 0.5 + close.pull[0] * 0.55;
        let cy_b = close_box.y + close_box.h * 0.5 + close.pull[1] * 0.55;
        let lens_box = Box4::new(cx_b - lw * 0.5, cy_b - lh * 0.5, lw, lh);
        chrome::paint_segmented_lens(
            window,
            lens_box,
            lw.min(lh) * 0.5,
            (press * 0.85).clamp(0.0, 1.0),
            close,
            1.0,
        );
    }
    let icon_inset = if p.kind == SettingsKind::Application { 7. } else { 7. };
    let icon_sz = if p.kind == SettingsKind::Application { 14. } else { 16. };
    super::icons::paint_elastic(
        window,
        cx,
        super::icons::Icon::Close,
        Box4::new(close_box.x + icon_inset, close_box.y + icon_inset, icon_sz, icon_sz),
        color(0.75 + 0.22 * close.hover),
        close,
    );
    let swell = p.tab_swell.max(0.0);
    let is_app = p.kind == SettingsKind::Application;
    if !is_app {
        chrome::paint_segmented_well(
            window,
            p.tab_well_rect,
            11.0 + 1.5 * swell,
            opacity,
        );
    }
    let active_page_fb = page_feedback(p.page as u8);
    let lens_box = Box4::new(
        p.tab_lens_rect.x + active_page_fb.pull[0] * 0.35,
        p.tab_lens_rect.y + active_page_fb.pull[1] * 0.25,
        p.tab_lens_rect.w,
        p.tab_lens_rect.h,
    );
    let lens_radius = (lens_box.h * 0.5).max(if is_app { 10.0 } else { 9.5 } + 3.5 * swell);
    let (rest_alpha, _) = chrome::segmented_crossfade(swell);
    chrome::paint_segmented_rest_pill(window, lens_box, lens_radius, p.accent_hex, rest_alpha, opacity);
    let tab_font_sz = 13.0 * (1.0 + 0.08 * swell);
    let scaled_tab = |i: usize| {
        let b_base = Settings::tab_rect_for_kind(p.kind, r, i, p.pages.len());
        if is_app {
            let base_cy = r.y + 52.0 + (p.pages.len() as f32 * 38.0 - 4.0) * 0.5;
            let spread = p.tab_well_rect.h / (p.pages.len() as f32 * 38.0).max(1.0);
            let cy = base_cy + (b_base.centre().1 - base_cy) * spread;
            Box4::new(b_base.x, cy - b_base.h * 0.5, b_base.w, b_base.h)
        } else {
            let base_track_cx = r.x + r.w * 0.5;
            let spread = if r.w > 24.0 { p.tab_well_rect.w / (r.w - 20.0) } else { 1.0 };
            let cx = base_track_cx + (b_base.centre().0 - base_track_cx) * spread;
            Box4::new(cx - b_base.w * 0.5, b_base.y, b_base.w, b_base.h)
        }
    };
    for (i, label) in p.pages.iter().enumerate() {
        let tab = scaled_tab(i);
        let tf = page_feedback(i as u8);
        if p.page != i && (tf.hover > 0.01 || tf.press > 0.01) && swell < 0.1 {
            chrome::control_surface_alpha(window, tab, tf, false, opacity * 0.45);
        }
        let text_color = if p.page == i {
            chrome::segmented_active_text_blended(p.accent_hex, rest_alpha, 0.96 * opacity)
        } else {
            color(0.62 + 0.22 * tf.hover)
        };
        chrome::paint_text_box_centred(
            window,
            label,
            tab,
            tab_font_sz,
            if p.page == i { 650. } else { 550. },
            text_color,
        );
    }
    if swell > 0.005 {
        if let Some((grect, surface, snapshot, hook)) = p.tab_lens_glass {
            window.paint_custom_surface_with_scene(
                gpui_kit::Bounds {
                    origin: gpui_kit::point(gpui_kit::px(grect.left as f32), gpui_kit::px(grect.top as f32)),
                    size: gpui_kit::size(gpui_kit::px(grect.width as f32), gpui_kit::px(grect.height as f32)),
                },
                surface,
                snapshot,
                hook,
            );
        }
        chrome::paint_segmented_lens(window, lens_box, lens_radius, p.tab_swell, active_page_fb, opacity);
    }
    let page_mask = gpui_kit::ContentMask {
        bounds: gpui_kit::bounds(
            gpui_kit::point(gpui_kit::px(c.x - 4.0), gpui_kit::px(c.y)),
            gpui_kit::size(gpui_kit::px(c.w + 8.0), gpui_kit::px(c.h)),
        ),
    };
    if p.outgoing_rows.is_empty() || p.page_progress >= 1.0 {
        window.with_content_mask(Some(page_mask), |window| {
            paint_page_rows(
                window,
                p,
                &p.rows,
                0.0,
                0.0,
                opacity,
                &feedback,
                &choice_feedback,
            );
        });
    } else {
        let t = 1.0 - (1.0 - p.page_progress.clamp(0.0, 1.0)).powi(3);
        let dir = p.page_dir as f32;
        let (dx_out, dy_out, dx_in, dy_in) = if p.kind == SettingsKind::Application {
            let travel = (c.h * 0.14).clamp(28.0, 64.0);
            (0.0, -dir * travel * t, 0.0, dir * travel * (1.0 - t))
        } else {
            let travel = (c.w * 0.28).clamp(48.0, 110.0);
            (-dir * travel * t, 0.0, dir * travel * (1.0 - t), 0.0)
        };
        let out_alpha = opacity * (1.0 - t * 1.15).clamp(0.0, 1.0);
        let in_alpha = opacity * t.clamp(0.0, 1.0);
        window.with_content_mask(Some(page_mask), |window| {
            if out_alpha > 0.01 {
                let no_fb = |_: u8| super::interaction::Feedback::default();
                paint_page_rows(
                    window,
                    p,
                    &p.outgoing_rows,
                    dx_out,
                    dy_out,
                    out_alpha,
                    &no_fb,
                    &no_fb,
                );
            }
            if in_alpha > 0.01 {
                paint_page_rows(
                    window,
                    p,
                    &p.rows,
                    dx_in,
                    dy_in,
                    in_alpha,
                    &feedback,
                    &choice_feedback,
                );
            }
        });
    }
    if p.max_scroll > 0. {
        let fraction = p.scroll / p.max_scroll;
        let thumb = (c.h * c.h / (c.h + p.max_scroll)).max(24.);
        chrome::fill_round(
            window,
            r.right() - 6.,
            c.y + fraction * (c.h - thumb),
            2.,
            thumb,
            1.,
            color(0.35),
        );
    }
    if !p.status.is_empty() && p.status_alpha > 0.005 {
        let ta = (p.status_alpha * opacity).clamp(0.0, 1.0);
        let max_tw = (r.w - 48.0).max(120.0);
        let tw = chrome::measure(window, p.status.as_str(), 12.0, 560.0).0;
        let toast_w = (tw + 28.0).min(max_tw);
        let toast_h = 30.0;
        let toast_r = Box4::new(
            r.x + (r.w - toast_w) * 0.5,
            r.bottom() - toast_h - 12.0,
            toast_w,
            toast_h,
        );
        chrome::elevation(window, toast_r, 15.0, 0.26 * ta);
        let (bg, border) = match p.theme_mode {
            super::model::ThemeMode::Light => (
                gpui_kit::Hsla::from(gpui_kit::rgb(0xFF_FF_FF)).opacity(0.90 * ta),
                chrome::themed_border(0.14 * ta),
            ),
            super::model::ThemeMode::Dark => (
                gpui_kit::Hsla::from(gpui_kit::rgb(0x12_14_19)).opacity(0.92 * ta),
                chrome::themed_border(0.20 * ta),
            ),
            super::model::ThemeMode::LiquidGlass => (
                gpui_kit::Hsla::from(gpui_kit::rgb(0x1C_1E_24)).opacity(0.68 * ta),
                chrome::themed_border(0.26 * ta),
            ),
        };
        chrome::fill_round(window, toast_r.x, toast_r.y, toast_r.w, toast_r.h, 15.0, bg);
        chrome::stroke_round(window, toast_r.x, toast_r.y, toast_r.w, toast_r.h, 15.0, 0.85, border);
        chrome::paint_text_fitted_vcentred(
            window,
            p.status.as_str(),
            toast_r.x + 14.0,
            toast_r.centre().1,
            (toast_r.w - 28.0).max(40.0),
            12.0,
            560.0,
            chrome::text(0.94 * ta),
        );
    }
    if let Some(d) = &p.dropdown {
        // A full shadow under a translucent list also darkens its interior.
        // Keep list glass at the same tint regardless of its height or option type.
        if let Some((grect, surface, snapshot, hook)) = d.glass {
            window.paint_custom_surface_with_scene(
                gpui_kit::Bounds {
                    origin: gpui_kit::point(gpui_kit::px(grect.left as f32), gpui_kit::px(grect.top as f32)),
                    size: gpui_kit::size(gpui_kit::px(grect.width as f32), gpui_kit::px(grect.height as f32)),
                },
                surface,
                snapshot,
                hook,
            );
        }
        let fallback_alpha = if d.glass.is_none() {
            Some(0.94)
        } else {
            match p.theme_mode {
                super::model::ThemeMode::Light => Some(0.86),
                super::model::ThemeMode::Dark => Some(0.55),
                super::model::ThemeMode::LiquidGlass => None,
            }
        };
        if let Some(fa) = fallback_alpha {
            let tint = if p.theme_mode == super::model::ThemeMode::Dark {
                gpui_kit::Hsla::from(gpui_kit::rgb(0x0B_0D_11)).opacity(fa * opacity)
            } else {
                chrome::themed_surface(fa * opacity)
            };
            chrome::fill_round(window, d.rect.x, d.rect.y, d.rect.w, d.rect.h, 12., tint);
        }
        window.paint_quad(gpui_kit::quad(
            gpui_kit::bounds(gpui_kit::point(gpui_kit::px(d.rect.x), gpui_kit::px(d.rect.y)), gpui_kit::size(gpui_kit::px(d.rect.w), gpui_kit::px(d.rect.h))),
            gpui_kit::Corners::all(gpui_kit::px(12.0)),
            gpui_kit::transparent_black(),
            gpui_kit::Edges::all(gpui_kit::px(0.85)),
            color(0.22),
            gpui_kit::BorderStyle::Solid,
        ));
        let inner_mask = gpui_kit::ContentMask {
            bounds: gpui_kit::Bounds {
                origin: gpui_kit::point(gpui_kit::px(d.rect.x + 2.0), gpui_kit::px(d.rect.y + 4.0)),
                size: gpui_kit::size(gpui_kit::px((d.rect.w - 4.0).max(1.0)), gpui_kit::px((d.rect.h - 8.0).max(1.0))),
            },
        };
        window.with_content_mask(Some(inner_mask), |window| {
            chrome::fill_round(window,d.lens_rect.x,d.lens_rect.y,d.lens_rect.w,d.lens_rect.h,9.,color(0.14));
            for (opt_i, key, label, opt_r, selected, _is_custom) in &d.options {
                if opt_r.bottom() < d.rect.y || opt_r.y > d.rect.bottom() {
                    continue;
                }
                let swatch_hex = if d.is_color {
                    key.strip_prefix('#').and_then(|h| u32::from_str_radix(h, 16).ok())
                } else {
                    None
                };
                chrome::paint_list_item(
                    window,
                    *opt_r,
                    label,
                    *selected,
                    swatch_hex,
                    p.accent_hex,
                    dropdown_feedback(*opt_i as u8),
                    opacity,
                );
            }
        });
    }
    if let Some(modal) = &p.modal {
        let (box_r, f1, f2, cancel, save) = Settings::modal_rects_with_offset(r, modal, p.modal_offset);
        if let Some((grect, surface, snapshot, hook)) = p.modal_glass {
            window.paint_custom_surface_with_scene(
                gpui_kit::Bounds {
                    origin: gpui_kit::point(gpui_kit::px(grect.left as f32), gpui_kit::px(grect.top as f32)),
                    size: gpui_kit::size(gpui_kit::px(grect.width as f32), gpui_kit::px(grect.height as f32)),
                },
                surface,
                snapshot,
                hook,
            );
        }
        let fallback_alpha = if p.modal_glass.is_none() {
            Some(0.94)
        } else {
            match p.theme_mode {
                super::model::ThemeMode::Light => Some(0.86),
                super::model::ThemeMode::Dark => Some(0.55),
                super::model::ThemeMode::LiquidGlass => None,
            }
        };
        if let Some(fa) = fallback_alpha {
            let tint = if p.theme_mode == super::model::ThemeMode::Dark {
                gpui_kit::Hsla::from(gpui_kit::rgb(0x0B_0D_11)).opacity(fa * opacity)
            } else {
                chrome::themed_surface(fa * opacity)
            };
            chrome::fill_round(window, box_r.x, box_r.y, box_r.w, box_r.h, 14.0, tint);
        }
        chrome::stroke_round(window, box_r.x, box_r.y, box_r.w, box_r.h, 14.0, 0.9, chrome::themed_border(0.22 * opacity));
        if matches!(modal, SettingsModal::PrivacyPolicy) {
            chrome::paint_text(window, "隐私条款", box_r.x + 18.0, box_r.y + 15.0, 15.0, 650.0, color(0.95));
            let clauses = [
                ("1. 零遥测与本地存储", "本软件不含任何自有统计、埋点或数据上报服务器。所有设置、歌词缓存、偏移记录、自定义歌词及翻译 API Key 仅存储在本地（AppData\\Roaming\\OverLyrics）。"),
                ("2. 媒体信息与本地音频分析", "通过 Windows SMTC 读取当前播放曲目与进度，仅用于本地展示与匹配歌词；开启节拍律动时仅在内存中通过 WASAPI 环回分析实时振幅，不录音、不落盘、不上传。"),
                ("3. 第三方网络服务", "检索歌词与封面时会向您启用的公开词源（酷狗、网易云、QQ 音乐、LRCLIB、AMLL 等）发送曲目与歌手信息；启用翻译时会向您选定的翻译服务发送歌词文本。"),
                ("4. 控制权与开源透明", "您可随时在设置中关闭词源、清理缓存或删除本地配置目录。本项目完全开源，所有行为公开可审计。"),
            ];
            let max_w = (box_r.w - 36.0).max(120.0);
            let max_y = cancel.y - 10.0;
            let mut cy = box_r.y + 44.0;
            for (heading, body) in clauses {
                if cy + 14.0 > max_y { break; }
                chrome::paint_text(window, heading, box_r.x + 18.0, cy, 12.0, 650.0, color(0.92));
                cy += 17.0;
                for line in wrap_modal_lines(window, body, max_w, 11.2, 450.0) {
                    if cy + 13.0 > max_y { break; }
                    chrome::paint_text(window, &line, box_r.x + 18.0, cy, 11.2, 450.0, color(0.72));
                    cy += 15.0;
                }
                cy += 6.0;
            }
            chrome::paint_glass_button(window, cancel, "关闭", p.accent_hex, true, false, modal_btns[0], opacity);
            return;
        }
        let (title, l1, ph1, v1, f1_active, l2, ph2, v2, f2_active) = match modal {
            SettingsModal::CustomModel { draft, .. } => (
                "添加自定义模型",
                "模型 API 名称",
                "例如 gpt-5-mini",
                draft.as_str(),
                true,
                "",
                "",
                "",
                false,
            ),
            SettingsModal::ArtistAlias { from, to, focus } => (
                "添加歌手名映射",
                "原始歌手名（播放器显示）",
                "例如 Yanzi Sun",
                from.as_str(),
                *focus == 0,
                "目标歌手名（用于匹配歌词）",
                "例如 孙燕姿",
                to.as_str(),
                *focus == 1,
            ),
            SettingsModal::PrivacyPolicy => unreachable!(),
        };
        chrome::paint_text(window, title, box_r.x + 16.0, box_r.y + 15.0, 14.5, 650.0, color(0.95));
        chrome::paint_text(window, l1, f1.x + 2.0, f1.y - 16.5, 11.5, 580.0, color(0.62));
        chrome::paint_glass_input(window, f1, ph1, v1, false, f1_active, p.caret_visible, p.accent_hex, opacity);
        if let Some(f2_rect) = f2 {
            chrome::paint_text(window, l2, f2_rect.x + 2.0, f2_rect.y - 16.5, 11.5, 580.0, color(0.62));
            chrome::paint_glass_input(window, f2_rect, ph2, v2, false, f2_active, p.caret_visible, p.accent_hex, opacity);
        }
        chrome::paint_glass_button(window, cancel, "取消", p.accent_hex, false, false, modal_btns[0], opacity);
        chrome::paint_glass_button(window, save, "保存", p.accent_hex, true, false, modal_btns[1], opacity);
    }
}

fn wrap_modal_lines(window: &mut gpui_kit::Window, text: &str, max_w: f32, size: f32, weight: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if super::chrome::measure(window, &cur, size, weight).0 > max_w && cur.chars().count() > 1 {
            let last = cur.pop().unwrap();
            lines.push(cur);
            cur = String::from(last);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn paint_page_rows(
    window: &mut gpui_kit::Window,
    p: &Presentation,
    rows: &[(usize, Item, Box4)],
    dx: f32,
    dy: f32,
    opacity: f32,
    feedback: &impl Fn(u8) -> super::interaction::Feedback,
    choice_feedback: &impl Fn(u8) -> super::interaction::Feedback,
) {
    use super::chrome;
    let color = |a| chrome::text(a * opacity);
    let mut section_starts: Vec<usize> = Vec::new();
    let mut has_sync = false;
    let mut has_alias = false;
    for (idx, (_, item, _)) in rows.iter().enumerate() {
        let label = if matches!(item.key, "lyrics.playerSyncOffsets.apple")
            || item.key == ACTIVE_PLAYER_OFFSET_KEY
            || item.key == CUSTOM_PLAYER_OFFSET_KEY
        {
            if !has_sync {
                has_sync = true;
                Some("播放器同步")
            } else {
                None
            }
        } else if item.key == "lyrics.artistAliases" {
            if !has_alias {
                has_alias = true;
                Some("歌手名映射")
            } else {
                None
            }
        } else if item.key == "lyrics.addArtistAlias" && !has_alias {
            has_alias = true;
            Some("歌手名映射")
        } else {
            Settings::section_label(item.key)
        };
        if idx == 0 || label.is_some() {
            section_starts.push(idx);
        }
    }
    for (g_idx, &start) in section_starts.iter().enumerate() {
        let end = section_starts.get(g_idx + 1).copied().unwrap_or(rows.len());
        let (_, first_item, first) = &rows[start];
        let (_, _, last) = &rows[end - 1];
        let header = if first_item.key == ACTIVE_PLAYER_OFFSET_KEY || first_item.key == CUSTOM_PLAYER_OFFSET_KEY {
            Some("播放器同步")
        } else if matches!(first_item.key, "lyrics.artistAliases" | "lyrics.addArtistAlias") {
            Some("歌手名映射")
        } else {
            Settings::section_label(first_item.key)
        };
        if let Some(title) = header {
            chrome::paint_text(
                window,
                title,
                first.x + dx + 6.0,
                first.y + dy - 20.0,
                11.0,
                600.0,
                color(0.48),
            );
        }
        let group_box = Box4::new(
            first.x + dx - 2.0,
            first.y + dy - 3.0,
            first.w + 4.0,
            (last.bottom() - first.y) + 6.0,
        );
        if chrome::is_light_theme() {
            chrome::fill_round(
                window,
                group_box.x,
                group_box.y,
                group_box.w,
                group_box.h,
                12.0,
                gpui_kit::Hsla::from(gpui_kit::rgb(0xFF_FF_FF)).opacity(0.76 * opacity),
            );
            chrome::stroke_round(
                window,
                group_box.x,
                group_box.y,
                group_box.w,
                group_box.h,
                12.0,
                0.75,
                chrome::themed_border(0.08 * opacity),
            );
        } else {
            chrome::fill_round(window, group_box.x, group_box.y, group_box.w, group_box.h, 12.0, color(0.045));
            chrome::stroke_round(window, group_box.x, group_box.y, group_box.w, group_box.h, 12.0, 0.75, color(0.08));
        }
    }
    for (row_pos, (i, item, raw_row)) in rows.iter().enumerate() {
        let row = Box4::new(raw_row.x + dx, raw_row.y + dy, raw_row.w, raw_row.h);
        if row_pos > 0 && !section_starts.contains(&row_pos) {
            chrome::fill_round(window, row.x + 8.0, row.y - 2.0, row.w - 16.0, 0.75, 0.35, color(0.06));
        }
        let value = &p.values[item.key];
        match item.control {
            Control::Slider { min, max, .. } => {
                let label: std::borrow::Cow<'_, str> = if item.key == ACTIVE_PLAYER_OFFSET_KEY {
                    std::borrow::Cow::Owned(format!(
                        "{} 同步偏移",
                        oly_core::mapping::player_display_name(&p.active_player_bucket)
                    ))
                } else {
                    std::borrow::Cow::Borrowed(item.label)
                };
                let slider_top_cy = row.y + 11.0;
                chrome::paint_text_vcentred(
                    window,
                    &label,
                    row.x + 8.,
                    slider_top_cy,
                    12.5,
                    500.,
                    color(0.82),
                );
                let v = value.as_f64().unwrap_or(min);
                let text = if item.key.starts_with("lyrics.playerSyncOffsets.") {
                    let iv = v.round() as i64;
                    if iv == 0 {
                        "0 ms".to_string()
                    } else {
                        format!("{iv:+} ms")
                    }
                } else if item.key == "letterSpacing" {
                    format!("{v:.3} em")
                } else if matches!(item.key, "lineGap" | "innerGap" | "linePaddingX" | "lineRadius") {
                    format!("{v:.2} em")
                } else if matches!(item.key, "coverRadius" | "cornerRadius" | "edgeFeather") {
                    format!("{v:.0} px")
                } else if matches!(item.key, "motionCoverCacheMB" | "lyrics.lyricsCacheMB") {
                    format!("{v:.0} MB")
                } else if matches!(
                    item.key,
                    "fontSize"
                        | "transFontSize"
                        | "playingLineTopOffset"
                        | "romanFontScale"
                        | "bgLineScale"
                        | "idleLineOpacity"
                        | "unplayedOriginalOpacity"
                        | "brightAlpha"
                        | "translatedLyricsOpacity"
                        | "transCurrentAlpha"
                        | "bgLineOpacity"
                        | "phoneticLyricsOpacity"
                        | "blurStrength"
                        | "lyrics.matchThreshold"
                ) {
                    format!("{v:.0}%")
                } else if matches!(
                    item.key,
                    "beatFlashThreshold"
                        | "beatFlashStrength"
                        | "beatWobbleThreshold"
                        | "beatWobbleStrength"
                ) {
                    format!("{v:.2}x")
                } else if max <= 4. {
                    format!("{v:.2}")
                } else {
                    format!("{v:.0}")
                };
                let vw = chrome::measure(window, &text, 12., 550.).0;
                chrome::paint_text_vcentred(
                    window,
                    &text,
                    (row.right() - 8. - vw).max(row.x + 120.),
                    slider_top_cy,
                    12.,
                    550.,
                    color(0.9),
                );
                chrome::paint_slider(
                    window,
                    row.x + 8.,
                    row.y + 34.,
                    row.w - 16.,
                    ((v - min) / (max - min)) as f32,
                    chrome::accent_rgb(p.accent_hex, 0.9 * opacity),
                    color(0.18),
                    feedback(*i as u8),
                    opacity,
                );
            }
            Control::PlayerOffset(idx) => {
                if let Some((bucket, v)) = p.custom_player_offsets.get(idx) {
                    let label = format!(
                        "{} 同步偏移",
                        oly_core::mapping::player_display_name(bucket)
                    );
                    let slider_top_cy = row.y + 11.0;
                    chrome::paint_text_vcentred(
                        window,
                        &label,
                        row.x + 8.,
                        slider_top_cy,
                        12.5,
                        500.,
                        color(0.82),
                    );
                    let iv = v.round() as i64;
                    let text = if iv == 0 {
                        "0 ms".to_string()
                    } else {
                        format!("{iv:+} ms")
                    };
                    let vw = chrome::measure(window, &text, 12., 550.).0;
                    chrome::paint_text_vcentred(
                        window,
                        &text,
                        (row.right() - 8. - vw).max(row.x + 120.),
                        slider_top_cy,
                        12.,
                        550.,
                        color(0.9),
                    );
                    chrome::paint_slider(
                        window,
                        row.x + 8.,
                        row.y + 34.,
                        row.w - 16.,
                        ((*v - (-2000.0)) / 4000.0) as f32,
                        chrome::accent_rgb(p.accent_hex, 0.9 * opacity),
                        color(0.18),
                        feedback(*i as u8),
                        opacity,
                    );
                }
            }
            Control::Switch => {
                let track = super::details::switch_track(row);
                let label_w = (track.x - row.x - 16.).max(40.);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                let on = value.as_bool().unwrap_or(false);
                let frac = p
                    .switch_fractions
                    .get(item.key)
                    .copied()
                    .unwrap_or(if on { 1.0 } else { 0.0 });
                let sf = choice_feedback(*i as u8);
                chrome::paint_switch_with_accent(
                    window,
                    track.x,
                    track.y,
                    track.w,
                    track.h,
                    on,
                    frac,
                    sf,
                    p.accent_hex,
                    opacity,
                );
            }
            Control::Text { secret } => {
                let field = Settings::choice_pill_rect(row);
                let label_w = (field.x - row.x - 16.).max(40.);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                let active = p.editing_key == Some(item.key);
                let raw = if active { p.editing_draft.as_str() } else { value.as_str().unwrap_or_default() };
                let placeholder = if active { "输入后按回车确认" } else { "点击输入" };
                chrome::paint_glass_input(
                    window,
                    field,
                    placeholder,
                    raw,
                    secret,
                    active,
                    p.caret_visible,
                    p.accent_hex,
                    opacity,
                );
            }
            Control::Segmented(options) => {
                let count = options.len().max(1);
                let fallback_track = Settings::segmented_track_rect(*raw_row, count);
                let seg = p.segmented.get(item.key);
                let base_track = seg.map(|s| s.track_rect).unwrap_or(fallback_track);
                let well = seg.map(|s| s.well_rect).unwrap_or(base_track);
                let lens = seg.map(|s| s.lens_rect).unwrap_or(base_track);
                let swell = seg.map(|s| s.swell).unwrap_or(0.0).max(0.0);
                let active_slot = seg.map(|s| s.active_slot).unwrap_or(0);
                let fb = choice_feedback(*i as u8);
                let track = Box4::new(base_track.x + dx, base_track.y + dy, base_track.w, base_track.h);
                let well_d = Box4::new(well.x + dx, well.y + dy, well.w, well.h);
                let lens_d = Box4::new(
                    lens.x + dx + fb.pull[0] * 0.35,
                    lens.y + dy + fb.pull[1] * 0.25,
                    lens.w,
                    lens.h,
                );
                let label_w = (track.x - row.x - 16.).max(40.);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                chrome::paint_segmented_well(window, well_d, well_d.h * 0.5, opacity);
                let pill_radius = lens_d.h * 0.5;
                let (rest_alpha, _) = chrome::segmented_crossfade(swell);
                chrome::paint_segmented_rest_pill(
                    window,
                    lens_d,
                    pill_radius,
                    p.accent_hex,
                    rest_alpha,
                    opacity,
                );
                let cell_w = track.w / count as f32;
                let base_cx = track.centre().0;
                let spread = if track.w > 2.0 { well_d.w / track.w } else { 1.0 };
                let font_sz = 11.5 * (1.0 + 0.08 * swell);
                for (slot_i, (_, opt_label)) in options.iter().enumerate() {
                    let slot_cx = base_cx + ((track.x + (slot_i as f32 + 0.5) * cell_w) - base_cx) * spread;
                    let sb = Box4::new(slot_cx - cell_w * 0.5, track.y, cell_w, track.h);
                    let selected = slot_i == active_slot;
                    chrome::paint_text_box_centred(
                        window,
                        opt_label,
                        sb,
                        font_sz,
                        if selected { 650.0 } else { 520.0 },
                        if selected {
                            chrome::segmented_active_text_blended(p.accent_hex, rest_alpha, 0.96 * opacity)
                        } else {
                            color(0.64 + 0.18 * fb.hover)
                        },
                    );
                }
                if dx.abs() < 0.5 && dy.abs() < 0.5 && swell > 0.005 {
                    if let Some((grect, surface, snapshot, hook)) = seg.and_then(|s| s.glass) {
                        window.paint_custom_surface_with_scene(
                            gpui_kit::Bounds {
                                origin: gpui_kit::point(
                                    gpui_kit::px(grect.left as f32),
                                    gpui_kit::px(grect.top as f32),
                                ),
                                size: gpui_kit::size(
                                    gpui_kit::px(grect.width as f32),
                                    gpui_kit::px(grect.height as f32),
                                ),
                            },
                            surface,
                            snapshot,
                            hook,
                        );
                    }
                    chrome::paint_segmented_lens(window, lens_d, pill_radius, swell, fb, opacity);
                }
            }
            Control::Choice(options) => {
                let pill = Settings::choice_pill_rect(row);
                let label_w = (pill.x - row.x - 16.).max(40.);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                let label = options
                    .iter()
                    .find(|(k, _)| Some(*k) == value.as_str())
                    .map(|(_, v)| *v)
                    .or_else(|| {
                        if item.key == "lyrics.aiModel" {
                            value
                                .as_str()
                                .and_then(|cur| p.custom_models.iter().find(|m| m.as_str() == cur).map(String::as_str))
                        } else {
                            None
                        }
                    })
                    .unwrap_or("默认");
                let is_open = p.dropdown.as_ref().is_some_and(|d| d.row_index == *i);
                chrome::paint_choice_pill_state(
                    window,
                    pill,
                    label,
                    is_open,
                    p.accent_hex,
                    choice_feedback(*i as u8),
                    opacity,
                );
            }
            Control::Action { button, danger } => {
                let fb = choice_feedback(*i as u8);
                let dynamic_btn: Option<String> = if item.key == "action.checkUpdate" {
                    Some(crate::updater::global().button_label())
                } else {
                    None
                };
                let btn_text = if item.key == "action.resetDefaults" && p.reset_armed {
                    "再次点击确认"
                } else if let Some(ref dyn_label) = dynamic_btn {
                    dyn_label.as_str()
                } else {
                    button
                };
                let primary = item.key == "action.checkUpdate"
                    && crate::updater::global().is_update_available();
                let is_about_btn = matches!(
                    item.key,
                    "action.openWebsite"
                        | "action.openRepo"
                        | "action.checkUpdate"
                        | "action.privacyPolicy"
                );
                let btn_w = if is_about_btn {
                    64.0
                } else {
                    (chrome::measure(window, btn_text, 12.0, 580.0).0 + 24.0).clamp(64.0, 184.0)
                };
                let btn = Box4::new(row.right() - 8.0 - btn_w, row.y + (row.h - 28.0) * 0.5, btn_w, 28.0);
                let label_w = (btn.x - row.x - 16.0).max(40.0);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                chrome::paint_glass_button(
                    window,
                    btn,
                    btn_text,
                    p.accent_hex,
                    primary,
                    danger,
                    fb,
                    opacity,
                );
            }
            Control::Info(val) => {
                let dynamic_val = if item.key == "about.version" {
                    Some(crate::updater::global().version_badge(val))
                } else {
                    None
                };
                let val_str = dynamic_val.as_deref().unwrap_or(val);
                let val_w = chrome::measure(window, val_str, 12.5, 500.0).0;
                let label_w = (row.w - val_w - 28.0).max(40.0);
                chrome::paint_text_fitted_vcentred(
                    window,
                    item.label,
                    row.x + 8.,
                    row.centre().1,
                    label_w,
                    12.5,
                    500.,
                    color(0.84),
                );
                chrome::paint_text_vcentred(
                    window,
                    val_str,
                    row.right() - 8. - val_w,
                    row.centre().1,
                    12.5,
                    500.,
                    color(0.60),
                );
            }
            Control::AliasRow(alias_idx) => {
                if let Some((from, to)) = p.artist_aliases.get(alias_idx) {
                    let fb = choice_feedback(*i as u8);
                    let del_w = 50.0;
                    let del_r = Box4::new(row.right() - 8.0 - del_w, row.y + (row.h - 26.0) * 0.5, del_w, 26.0);
                    let text_w = (del_r.x - row.x - 16.0).max(40.0);
                    let display = format!("{from}  →  {to}");
                    chrome::paint_text_fitted_vcentred(
                        window,
                        &display,
                        row.x + 8.0,
                        row.centre().1,
                        text_w,
                        12.5,
                        500.0,
                        color(0.88),
                    );
                    chrome::paint_glass_button(
                        window,
                        del_r,
                        "删除",
                        p.accent_hex,
                        false,
                        false,
                        fb,
                        opacity,
                    );
                }
            }
        }
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        if self.pending() {
            if let Some(store) = &self.store {
                if let Err(error) = store.patch(self.pending.clone()) {
                    log::warn!("display draft save on close: {error}");
                }
            }
        }
    }
}
#[cfg(test)]
mod persistence_tests {
    use super::*;
    #[test]
    fn closing_before_settle_flushes_the_last_draft_to_the_isolated_store() {
        let root = oly_core::config::DataRoot::new(
            std::env::temp_dir().join(format!(
                "oly-display-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )),
        );
        let store = Arc::new(ConfigStore::load(&root).unwrap());
        {
            let mut s = Settings::with_store(store);
            s.set("fontSize", json!(123), 0., true);
            s.set("lyrics.enabledSources.netease", json!(false), 0., true);
            s.set("lyrics.matchThreshold", json!(84), 0., true);
            s.set("windowBackground", json!("glass"), 0., true);
        }
        let restored = ConfigStore::load(&root).unwrap();
        assert_eq!(Display::effective_json(&restored.get())["fontSize"], 123);
        assert_eq!(restored.get()["lyrics"]["enabledSources"]["netease"], false);
        assert_eq!(restored.get()["lyrics"]["matchThreshold"], 84);
        assert_eq!(restored.get()["display"]["windowBackground"], "glass");
    }
}

#[cfg(test)] mod sidebar_parity_tests {
 use super::*;
 #[test] fn lyrics_and_application_use_floating_left_edge_panels_in_wide_window() {
  let wide = Settings::panel(1272., 843.);
  assert_eq!(wide.x, 12.);
  assert!(wide.y > 0. && wide.h < 843.);
  assert_eq!(wide.w, 380.);
  let sidecar_lyrics = Settings::panel(380., 650.);
  assert_eq!(sidecar_lyrics.x, 0.); assert_eq!(sidecar_lyrics.y, 0.);
  assert_eq!(sidecar_lyrics.w, 380.); assert_eq!(sidecar_lyrics.h, 650.);
  let app_r = Settings::panel_for(SettingsKind::Application, 1272., 843.);
  assert_eq!(app_r.x, 12.);
  assert!(app_r.y > 0.);
  assert!(app_r.w < 1272. && app_r.h < 843.);
  let sidecar = Settings::panel_for(SettingsKind::Application, 540., 440.);
  assert_eq!(sidecar.x, 0.); assert_eq!(sidecar.y, 0.);
  assert_eq!(sidecar.w, 540.); assert_eq!(sidecar.h, 440.);
  let mut app = Settings::default();
  app.set_kind(SettingsKind::Application);
  let tab0 = app.tab_rect_for(app_r, 0);
  let tab1 = app.tab_rect_for(app_r, 1);
  assert_eq!(tab0.x, tab1.x);
  assert!(tab1.y > tab0.y);
  assert!(app.content_box(app_r).x >= tab0.right());
 }

 #[test] fn mini_and_cover_profiles_persist_separately() {
  let mut s = Settings::default();
  s.profile = "cover";
  s.set("fontSize", serde_json::json!(42), 0.0, false);
  s.profile = "mini";
  s.set("fontSize", serde_json::json!(18), 0.0, false);
  assert_eq!(s.value("fontSize"), serde_json::json!(18));
  s.profile = "cover";
  assert_eq!(s.value("fontSize"), serde_json::json!(42));
 }
}

#[cfg(test)] mod direct_list_tests {
 use super::*;
 #[test] fn list_follows_hover_without_waiting_for_a_spring_frame(){
 let mut s=Settings::default();s.page=1;s.open_choice=items(1).iter().position(|item|matches!(item.control,Control::Choice(_)));
 let panel=Settings::panel(380.,650.);let d=s.dropdown_layout(panel).unwrap();let last=d.option_count-1;
 s.step_dropdown(panel,Some(0),false,16.);
 s.step_dropdown(panel,Some(last),true,0.);
 let d=s.dropdown_layout(panel).unwrap();let row=d.options.iter().find(|(i,..)|*i==last).unwrap().3;
 assert_eq!(d.lens_rect.y,row.y+1.);assert_eq!(d.lens_rect.h,row.h);assert_eq!(d.lens_swell,0.);
 }
 #[test] fn choices_with_under_four_options_use_segmented_selectors(){
  let mut s=Settings::default();
  for kind in [SettingsKind::Lyrics, SettingsKind::Application] {
   s.set_kind(kind);
   for p in 0..s.pages().len() {
    s.set_page(p);
    for item in s.items() {
     if let Control::Choice(opts) = item.control {
      assert!(opts.len() >= 4 || item.key == "lyrics.aiModel", "{} should use Control::Segmented", item.key);
     }
    }
   }
  }
 }
}

#[cfg(test)]mod switch_drag_tests{
 use super::*;
 #[test]fn dragging_previews_then_commits_only_on_release(){
 let mut s=Settings::default();let index=items(0).iter().position(|i|matches!(i.control,Control::Switch)).unwrap();let key=items(0)[index].key;
 let before=s.value(key);let destination=if before.as_bool()==Some(true){0.}else{1.};
 s.preview_switch(index,destination);assert_eq!(s.value(key),before);assert!(!s.pending());
 s.finish_switch(index,destination,true,10.);assert_eq!(s.value(key).as_bool(),Some(destination>=0.5));assert!(s.pending());
 }
 #[test]fn cancelling_drag_does_not_change_the_setting(){let mut s=Settings::default();let index=items(0).iter().position(|i|matches!(i.control,Control::Switch)).unwrap();let key=items(0)[index].key;let before=s.value(key);s.preview_switch(index,0.4);s.switch_preview=None;assert_eq!(s.value(key),before);assert!(!s.pending());}
}

#[cfg(test)]
mod separation_tests {
    use super::*;

    #[test]
    fn advanced_parameters_are_hidden_from_every_basic_lyric_page() {
        let mut settings = Settings::default();
        let advanced = ["lineWidthAspect", "linePaddingX", "letterSpacing", "longSyllableThreshold", "wordFadeWidth", "edgeFeather"];
        for page in 0..4 {
            settings.page = page;
            assert!(settings.items().iter().all(|item| !advanced.contains(&item.key)));
        }
        settings.page = 4;
        assert_eq!(settings.items().iter().map(|i| i.key).collect::<Vec<_>>(), advanced);
        settings.page = 3;
        assert!(settings.items().iter().any(|i| i.key == "lineRadius"));
        assert!(settings.items().iter().any(|i| i.key == "hoverBgAlpha"));
    }

    #[test]
    fn application_panel_owns_appearance_and_sources_and_keeps_accent_qa_position() {
        let mut settings = Settings::default();
        for page in 0..settings.pages().len() {
            assert!(settings.items_on(page).iter().all(|i| !matches!(i.key, "showCover" | "motionCover" | "uiAccentColor")));
        }
        settings.set_kind(SettingsKind::Application);
        assert_eq!(settings.title, "设置");
        assert_eq!(settings.items()[0].key, "themeMode");
        assert_eq!(settings.items()[1].key, "uiAccentColor");
        assert_eq!(settings.items().last().map(|i| i.key), Some("cornerRadius"));
        settings.page = 3;
        assert_eq!(settings.items()[0].key, "lyrics.enabledSources.kugou");
        settings.page = 4;
        settings.set("lyrics.translateEngine", json!("ai"), 0., false);
        assert!(settings.items().iter().any(|i| i.key == "lyrics.aiModel"));
        assert!(settings.items().iter().any(|i| i.key == "lyrics.aiBaseUrl"));
        assert!(settings.items().iter().any(|i| i.key == "lyrics.aiKey"));
        assert!(settings.items().iter().any(|i| i.key == "lyrics.aiFallback"));
    }

    #[test]
    fn edits_route_to_document_global_display_and_active_profile_independently() {
        let mut settings = Settings::default();
        let before_other_source = settings.value("lyrics.enabledSources.qq");
        settings.set("lyrics.enabledSources.kugou", json!(false), 0., false);
        settings.set("lyrics.matchThreshold", json!(82), 0., false);
        settings.set("lyrics.autoTranslate", json!(false), 0., false);
        settings.set("windowBackground", json!("glass"), 0., false);
        settings.set("immersionCompact", json!(true), 0., false);
        settings.set("lineRadius", json!(0.75), 0., false);
        settings.set("hoverBgAlpha", json!(12), 0., false);
        assert_eq!(settings.config["lyrics"]["enabledSources"]["kugou"], false);
        assert_eq!(settings.value("lyrics.enabledSources.qq"), before_other_source);
        assert_eq!(settings.config["lyrics"]["matchThreshold"], 82);
        assert_eq!(settings.config["lyrics"]["autoTranslate"], false);
        assert_eq!(settings.config["display"]["windowBackground"], "glass");
        assert_eq!(settings.config["display"]["immersionCompact"], true);
        assert_eq!(settings.effective()["lineRadius"], 0.75);
        assert_eq!(settings.effective()["hoverBgAlpha"], 12);
        let profile = &settings.config["display"]["typeProfiles"]["cover"];
        assert!(profile.get("lyrics.matchThreshold").is_none());
        assert!(profile.get("windowBackground").is_none());
        settings.set_kind(SettingsKind::Application);
        settings.page = 3;
        let presentation = settings.presentation(Settings::panel(380., 650.));
        assert_eq!(presentation.values["lyrics.enabledSources.kugou"], false);
    }

    #[test]
    fn translation_credentials_are_editable_and_cancel_does_not_persist() {
        let mut settings = Settings::default();
        settings.set_kind(SettingsKind::Application);
        settings.page = 4;
        assert!(!settings.items().iter().any(|i| i.key == "lyrics.libreUrl"));
        settings.set("lyrics.translateEngine", json!("ai"), 0., false);
        assert!(!settings.items().iter().any(|i| i.key == "lyrics.libreUrl"));
        assert!(settings.items().iter().any(|item| item.key == "lyrics.aiBaseUrl"));
        assert!(settings.items().iter().any(|item| item.key == "lyrics.aiModel"));
        let key_row = settings.items().iter().position(|i| i.key == "lyrics.aiKey").unwrap();
        assert!(settings.begin_edit(key_row));
        settings.insert_text("Key-大写\n");
        assert_eq!(settings.editing_draft, "Key-大写");
        settings.edit_key("escape", 1.);
        assert_eq!(settings.value("lyrics.aiKey"), "");
        assert!(settings.begin_edit(key_row));
        settings.insert_text("SecretKey");
        settings.edit_key("enter", 2.);
        assert_eq!(settings.value("lyrics.aiKey"), "SecretKey");
        settings.set("lyrics.aiPreset", json!("openai"), 3., false);
        assert_eq!(settings.value("lyrics.aiBaseUrl"), "https://api.openai.com/v1");
        assert_eq!(settings.value("lyrics.aiModel"), "gpt-4o-mini");
        assert_eq!(settings.value("lyrics.aiKey"), "");
    }

    #[test]
    fn player_sync_offsets_route_by_player_and_remove_cleanly() {
        let mut settings = Settings::default();
        settings.set_kind(SettingsKind::Application);
        settings.set_page(5);
        assert!(settings.items().iter().any(|i| i.key == "lyrics.playerSyncOffsets.apple"));
        assert!(!settings.items().iter().any(|i| i.key == "lyrics.playerSyncOffsets.default"));

        settings.set("lyrics.playerSyncOffsets.apple", json!(-600), 0., false);
        assert_eq!(settings.config["lyrics"]["playerSyncOffsets"]["apple"], -600);

        settings.active_player = "QQMusic.exe".into();
        let keys: Vec<_> = settings.items().into_iter().map(|i| i.key).collect();
        assert!(keys.contains(&ACTIVE_PLAYER_OFFSET_KEY));
        assert!(keys.contains(&"lyrics.playerSyncOffsets.apple"));
        settings.set(ACTIVE_PLAYER_OFFSET_KEY, json!(350), 2., false);
        assert_eq!(settings.config["lyrics"]["playerSyncOffsets"]["qqmusic"], 350);
        assert_eq!(settings.config["lyrics"]["playerSyncOffsets"]["apple"], -600);
        assert!(settings.saved_player_offsets().contains(&"qqmusic".to_string()));

        let row_idx = settings
            .items()
            .iter()
            .position(|i| matches!(i.control, Control::PlayerOffset(_)))
            .expect("saved custom player row");
        assert_eq!(settings.remove_player_offset(row_idx, 3.), Some("qqmusic".to_string()));
        assert!(!settings.saved_player_offsets().contains(&"qqmusic".to_string()));
    }

    #[test]
    fn custom_models_and_artist_aliases_add_and_remove_cleanly() {
        let mut settings = Settings::default();
        settings.set_kind(SettingsKind::Application);
        settings.set_page(4);
        settings.set("lyrics.translateEngine", json!("ai"), 0., false);
        assert!(settings.add_custom_model("deepseek", "deepseek-v4-custom", 1.));
        assert_eq!(settings.value("lyrics.aiModel"), "deepseek-v4-custom");
        assert!(settings.custom_models_for("deepseek").contains(&"deepseek-v4-custom".to_string()));

        let model_row = settings.items().iter().position(|i| i.key == "lyrics.aiModel").unwrap();
        settings.open_choice = Some(model_row);
        let opts = settings.model_options_for("deepseek");
        let custom_idx = opts.iter().position(|(k, _, c)| *c && k == "deepseek-v4-custom").unwrap();
        assert!(settings.delete_custom_model_option(custom_idx, 2.));
        assert!(!settings.custom_models_for("deepseek").contains(&"deepseek-v4-custom".to_string()));

        settings.set_page(3);
        assert!(settings.add_artist_alias("Yanzi Sun", "孙燕姿", 3.));
        assert_eq!(settings.artist_aliases(), vec![("Yanzi Sun".to_string(), "孙燕姿".to_string())]);
        let track = oly_engine::types::Track {
            title: "天黑黑".into(),
            artist: Some("yanzi sun".into()),
            ..Default::default()
        };
        let resolved = crate::live::resolve_track_artist_alias(&settings.config, &track);
        assert_eq!(resolved.artist.as_deref(), Some("孙燕姿"));
        assert!(settings.remove_artist_alias("Yanzi Sun", 4.));
        assert!(settings.artist_aliases().is_empty());
    }

    #[test]
    fn beat_sliders_appear_only_when_beat_follow_switch_is_on() {
        let mut settings = Settings::default();
        settings.set_kind(SettingsKind::Application);
        settings.page = 0;

        let keys_on: Vec<_> = settings.items().into_iter().map(|i| i.key).collect();
        let follow_pos = keys_on
            .iter()
            .position(|&k| k == "beatFollow")
            .expect("beatFollow switch on appearance page");
        assert_eq!(
            &keys_on[follow_pos + 1..follow_pos + 5],
            &[
                "beatFlashThreshold",
                "beatFlashStrength",
                "beatWobbleThreshold",
                "beatWobbleStrength",
            ]
        );
        assert_eq!(settings.value("beatFollow"), json!(true));
        assert_eq!(settings.value("beatFlashThreshold"), json!(1.02));
        assert_eq!(settings.value("beatFlashStrength"), json!(1.2));
        assert_eq!(settings.value("beatWobbleThreshold"), json!(1.3));
        assert_eq!(settings.value("beatWobbleStrength"), json!(1.2));

        settings.set("beatFlashThreshold", json!(1.35), 1., false);
        settings.set("beatWobbleStrength", json!(2.0), 2., false);
        let parsed_on = Display::from_config(&settings.config);
        assert!(parsed_on.beat_follow);
        assert!((parsed_on.beat_flash_threshold - 1.35).abs() < 1e-6);
        assert!((parsed_on.beat_wobble_strength - 2.0).abs() < 1e-6);

        settings.set("beatFollow", json!(false), 3., false);
        let keys_off: Vec<_> = settings.items().into_iter().map(|i| i.key).collect();
        assert!(keys_off.contains(&"beatFollow"));
        for hidden in [
            "beatFlashThreshold",
            "beatFlashStrength",
            "beatWobbleThreshold",
            "beatWobbleStrength",
        ] {
            assert!(!keys_off.contains(&hidden), "{hidden} should be hidden when beatFollow is off");
        }
        assert!(!Display::from_config(&settings.config).beat_follow);
    }

    #[test]
    fn about_page_exposes_author_version_repo_and_privacy_modal() {
        let mut settings = Settings::default();
        settings.set_kind(SettingsKind::Application);
        assert_eq!(settings.pages().last().copied(), Some("关于"));
        settings.page = settings.pages().len() - 1;
        let keys: Vec<_> = settings.items().into_iter().map(|i| i.key).collect();
        assert_eq!(
            keys,
            vec![
                "about.author",
                "about.version",
                "action.openWebsite",
                "action.openRepo",
                "action.checkUpdate",
                "action.privacyPolicy",
            ]
        );
        let privacy_idx = keys.iter().position(|&k| k == "action.privacyPolicy").unwrap();
        assert!(settings.activate_row(privacy_idx, 1.0));
        assert_eq!(settings.modal, Some(SettingsModal::PrivacyPolicy));
        assert!(settings.handle_modal_key("escape", 2.0));
        assert!(settings.modal.is_none());
    }
}
