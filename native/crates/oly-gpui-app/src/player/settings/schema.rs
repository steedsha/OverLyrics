use super::*;

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
pub(super) const fn slider(key: &'static str, label: &'static str, min: f64, max: f64, step: f64) -> Item {
    Item {
        key,
        label,
        control: Control::Slider { min, max, step },
    }
}
pub(super) const fn toggle(key: &'static str, label: &'static str) -> Item {
    Item {
        key,
        label,
        control: Control::Switch,
    }
}
pub(super) const fn text(key: &'static str, label: &'static str, secret: bool) -> Item {
    Item { key, label, control: Control::Text { secret } }
}
pub(super) const fn choice(
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
pub(super) const fn segmented(
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
pub(super) const fn action(key: &'static str, label: &'static str, button: &'static str, danger: bool) -> Item {
    Item {
        key,
        label,
        control: Control::Action { button, danger },
    }
}
pub(super) const fn info(key: &'static str, label: &'static str, value: &'static str) -> Item {
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

pub(super) const ACTIVE_PLAYER_OFFSET_KEY: &str = "lyrics.playerSyncOffsets._current";
pub(super) const CUSTOM_PLAYER_OFFSET_KEY: &str = "lyrics.playerSyncOffsets._custom";

/// Display controls are shape-specific unless explicitly global; dotted keys
/// identify a path in the complete document, never a display profile field.
pub(super) enum ConfigPath<'a> {
    Profile(&'a str),
    Display(&'a str),
    Document(&'a str),
}
pub(super) fn config_path(key: &str) -> ConfigPath<'_> {
    if key.contains('.') { return ConfigPath::Document(key); }
    if matches!(key, "showCover" | "showProgress" | "uiAccentColor" | "coverRadius"
        | "cornerRadius" | "motionCover" | "motionCoverCacheMB" | "windowBackground"
        | "immersionCompact" | "beatFollow" | "beatFlashThreshold" | "beatFlashStrength"
        | "beatWobbleThreshold" | "beatWobbleStrength" | "backdropStyle" | "themeMode") {
        ConfigPath::Display(key)
    } else { ConfigPath::Profile(key) }
}
pub(super) fn nested_patch(path: &str, value: Value) -> Value {
    path.split('.').rev().fold(value, |value, segment| {
        let mut map = serde_json::Map::new();
        map.insert(segment.to_owned(), value);
        Value::Object(map)
    })
}
pub(super) fn ai_models_for(provider: &str) -> &'static [(&'static str, &'static str)] {
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

pub(super) fn application_items(page: usize) -> Vec<Item> {
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
            segmented(
                "lyrics.tierPreference",
                "版本偏好",
                &[("trans", "译文优先"), ("word", "逐字优先")],
            ),
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
            segmented(
                "window.closeAction",
                "关闭窗口时",
                &[("ask", "每次询问"), ("minimize", "最小化到托盘"), ("quit", "退出程序")],
            ),
            slider("lyrics.playerSyncOffsets.apple", "Apple Music 同步偏移", -2000., 2000., 50.),
            action("action.clearLyricsCache", "本地歌词缓存", "清除", false),
            action("action.clearCoverCache", "动态封面缓存", "清除", false),
            action("action.resetTrackOffsets", "单曲偏移记录", "清除", false),
            action("action.clearSourceMemory", "手动选词记录", "清除", false),
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
pub(super) fn lyric_items(page: usize) -> Vec<Item> {
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
