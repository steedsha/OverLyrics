//! Live display drafts with isolated persistence and legacy profile keys.
use super::model::{Box4, Display};
use crate::ui::settings::SLIDERS;
use oly_core::config::{merge, ConfigStore};
use serde_json::{json, Value};
use std::sync::Arc;

mod paint;
mod schema;
#[cfg(test)]
mod tests;

pub use paint::*;
pub use schema::*;

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
            config: oly_core::config::default_config(),
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