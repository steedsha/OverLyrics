use super::*;

impl PlayerView {
    /// The picker's version list: every fixture beside the loaded one, so
    /// "choose a version" really swaps documents instead of faking a choice.
    pub(super) fn execute_settings_action(&mut self, action: &str) -> (String, Option<serde_json::Value>) {
        if action == "action.openWebsite" {
            let url = crate::updater::WEBSITE_URL;
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", url])
                    .creation_flags(0x0800_0000)
                    .spawn();
            }
            return ("已在浏览器中打开官方网站".into(), None);
        }
        if action == "action.checkUpdate" {
            let mgr = crate::updater::global();
            return match mgr.state() {
                crate::updater::UpdateState::Available(info) => {
                    if mgr.start_install(info) {
                        ("正在下载新版本…".into(), None)
                    } else {
                        ("正在处理更新，请稍候…".into(), None)
                    }
                }
                crate::updater::UpdateState::Checking
                | crate::updater::UpdateState::Downloading { .. }
                | crate::updater::UpdateState::ReadyToRestart { .. } => {
                    ("正在处理更新，请稍候…".into(), None)
                }
                _ => {
                    if mgr.start_check(false) {
                        ("正在检查更新…".into(), None)
                    } else {
                        ("正在处理更新，请稍候…".into(), None)
                    }
                }
            };
        }
        if action == "action.openRepo" {
            let url = "https://github.com/steedsha/OverLyrics";
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", url])
                    .creation_flags(0x0800_0000)
                    .spawn();
            }
            return ("已在浏览器中打开开源仓库".into(), None);
        }
        let Some(live) = self.live.as_ref() else {
            return (String::new(), None);
        };
        match action {
            "action.pickCustomLyricsDir" => {
                if self.pending_folder_rx.is_none() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.pending_folder_rx = Some(rx);
                    std::thread::Builder::new()
                        .name("lyric-folder-dialog".into())
                        .spawn(move || {
                            let _ = tx.send(oly_platform_win::file_dialog::pick_lyrics_folder());
                        })
                        .ok();
                }
                ("正在打开目录选择器…".into(), None)
            }
            "action.clearCustomLyricsDir" => {
                if let Some(live) = self.live.as_ref() {
                    live.local_folder.set_dir(None);
                }
                self.change_setting("lyrics.customLyricsDir", serde_json::json!(""));
                ("已清除本地歌词扫描目录".into(), None)
            }
            "action.openCustomLyricsDir" => {
                let dir = live.custom_lyrics.root_dir().to_path_buf();
                let _ = std::fs::create_dir_all(&dir);
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                }
                ("已打开本地歌词保存目录".into(), None)
            }
            "action.clearLyricsCache" => {
                let n = live.clear_lyrics_cache();
                (format!("已清理 {n} 项歌词缓存"), None)
            }
            "action.clearCoverCache" => {
                let n = live.clear_cover_cache();
                (format!("已清理 {n} 项封面缓存"), None)
            }
            "action.resetTrackOffsets" => {
                let cleared = if self.sync.has_worker() {
                    self.sync.clear_all().is_ok()
                } else {
                    let _ = self.sync.clear_all();
                    live.reset_track_offsets().is_ok()
                };
                if cleared {
                    self.update_sync();
                    ("已重置全部单曲偏移".into(), None)
                } else {
                    (String::new(), None)
                }
            }
            "action.clearSourceMemory" => {
                let n = live.clear_source_memory();
                live.search.clear_cache();
                let current_track = live.track.clone();
                let generation = live.model.generation();
                if let Some(track) = current_track {
                    live.search.request(generation, track, None);
                }
                self.auto_pick_persisted = false;
                self.doc_auto_picked = false;
                if let Some(live) = self.live.as_mut() {
                    if live.track.is_some() {
                        live.status = "正在按词源优先级重新选词…".into();
                    }
                }
                let msg = if n > 0 {
                    format!("已清除 {n} 条手动选词记录")
                } else {
                    "已清除手动选词记录".into()
                };
                (msg, None)
            }
            "action.resetDefaults" => {
                if let Ok(fresh) = live.reset_all_settings() {
                    self.settings.config = fresh.clone();
                    self.sync_profile_display();
                    self.update_sync();
                    ("已恢复默认设置".into(), Some(fresh))
                } else {
                    (String::new(), None)
                }
            }
            _ => (String::new(), None),
        }
    }

    pub(super) fn notify_artist_alias_changed(&mut self, changed_artist: &str) {
        if let Some(live) = self.live.as_mut() {
            live.search.clear_cache();
            if let Some(track) = live.track.clone() {
                let cur_artist = oly_engine::strings::primary_artist(&track);
                if cur_artist.trim().eq_ignore_ascii_case(changed_artist.trim()) {
                    live.search.request(live.model.generation(), track, None);
                    live.status = "正在重新搜索歌词…".into();
                }
            }
        }
    }

    pub(super) fn has_multiple_media_sessions(&self) -> bool {
        self.live.as_ref().is_some_and(|l| l.media_sessions.len() > 1)
    }

    pub(super) fn friendly_media_name(app: &str) -> String {
        let low = app.to_ascii_lowercase();
        if low.contains("applemusic") {
            "Apple Music".into()
        } else if low.contains("spotify") {
            "Spotify".into()
        } else if low.contains("qqmusic") {
            "QQ 音乐".into()
        } else if low.contains("cloudmusic") {
            "网易云音乐".into()
        } else if low.contains("kugou") {
            "酷狗音乐".into()
        } else if low.contains("msedge") {
            "Microsoft Edge".into()
        } else if low.contains("chrome") {
            "Google Chrome".into()
        } else if low.contains("foobar2000") {
            "foobar2000".into()
        } else if low.contains("potplayer") {
            "PotPlayer".into()
        } else {
            let trimmed = app.split('!').next().unwrap_or(app);
            trimmed
                .strip_suffix(".exe")
                .or_else(|| trimmed.strip_suffix(".EXE"))
                .unwrap_or(trimmed)
                .to_string()
        }
    }

    pub(super) fn candidates(&self) -> Vec<PickerEntry> {
        if self.picker_kind == PickerKind::Media {
            let mut out = Vec::new();
            if let Some(live) = self.live.as_ref() {
                let pinned = live.pinned_media_app.as_deref();
                out.push(PickerEntry {
                    title: "自动跟随".into(),
                    subtitle: "自动选择正在播放的媒体".into(),
                    match_badge: String::new(),
                    timing_badge: None,
                    has_translation: false,
                    smart_translation: false,
                    smart_translation_key: String::new(),
                    path: PathBuf::new(),
                    source: "media-auto".into(),
                    id: String::new(),
                    candidate_index: 0,
                    selected: pinned.is_none(),
                });
                for s in &live.media_sessions {
                    let status_label = if s.status == 4 { "播放中" } else { "已暂停" };
                    let sub = if s.title.trim().is_empty() {
                        s.app.clone()
                    } else {
                        s.title.clone()
                    };
                    out.push(PickerEntry {
                        title: Self::friendly_media_name(&s.app),
                        subtitle: sub,
                        match_badge: String::new(),
                        timing_badge: Some(status_label),
                        has_translation: false,
                        smart_translation: false,
                        smart_translation_key: String::new(),
                        path: PathBuf::new(),
                        source: "media-pin".into(),
                        id: s.app.clone(),
                        candidate_index: 0,
                        selected: pinned == Some(s.app.as_str()),
                    });
                }
            }
            return out;
        }
        let qa_pool=if self.cfg.scenario.as_deref()==Some("settings-split"){
            use oly_engine::types::{Candidate,Verified};
            let c=|source:&str,id:&str,percent:Option<f64>,word:bool,trans:bool|Candidate{source:source.into(),id:id.into(),title:"Hard Times".into(),artist:Some("Paramore".into()),percent,duration_sec:Some(182.),verified:Some(Verified{word:Some(word),trans:Some(trans),timeline:Some(true),lines:Some(58),..Default::default()}),..Default::default()};
            Some(vec![c("amlldb","qa-amll",Some(98.4),true,true),c("kugou","qa-line",Some(70.),false,false),c("qq","qa-below",Some(69.9),true,true),c("lrclib","qa-unknown",None,false,false)])
        }else{None};
        if let Some(pool)=self.live.as_ref().map(|live|live.candidates.as_slice()).or(qa_pool.as_deref()) {
            let identity=if qa_pool.is_some(){Some(("amlldb","qa-amll"))}else{self.doc_identity.as_ref().map(|(s,i)|(s.as_str(),i.as_str()))};
            let track_key=self.live.as_ref().and_then(|live|live.model.packet.as_ref()).map(|p|p.track_key()).unwrap_or_else(||if qa_pool.is_some(){"qa-track".into()}else{String::new()});
            let mut out = Vec::new();
            let custom_enabled = self
                .settings
                .config
                .pointer("/lyrics/enabledSources/custom")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let mut custom_rows = Vec::new();
            if custom_enabled {
                if let Some(live) = self.live.as_ref() {
                    for entry in live.custom_lyrics.entries_for(&track_key) {
                        let selected = identity.is_some_and(|(s, id)| s == "custom" && id == entry.id);
                        custom_rows.push(PickerEntry {
                            title: format!("{} · 本地导入", entry.file_name),
                            subtitle: format!("{} · {} 行（右键删除）", entry.ext.to_uppercase(), entry.line_count),
                            match_badge: String::new(),
                            timing_badge: entry.word_timed.then_some("逐字"),
                            has_translation: entry.has_translation,
                            smart_translation: false,
                            smart_translation_key: String::new(),
                            path: PathBuf::new(),
                            source: "custom".into(),
                            id: entry.id.clone(),
                            candidate_index: 0,
                            selected,
                        });
                    }
                    if let Some(track) = live.track.as_ref() {
                        let threshold = self
                            .settings
                            .config
                            .pointer("/lyrics/matchThreshold")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(70.0);
                        for m in live.local_folder.matches_for(track, threshold) {
                            let selected = identity.is_some_and(|(s, id)| s == "custom" && id == m.id);
                            custom_rows.push(PickerEntry {
                                title: format!("{} · 本地目录", m.file_name),
                                subtitle: format!("{} · {} 行", m.ext.to_uppercase(), m.line_count),
                                match_badge: format!("{}%", m.percent.round() as i32),
                                timing_badge: m.word_timed.then_some("逐字"),
                                has_translation: m.has_translation,
                                smart_translation: false,
                                smart_translation_key: String::new(),
                                path: m.path,
                                source: "custom".into(),
                                id: m.id,
                                candidate_index: 0,
                                selected,
                            });
                        }
                    }
                }
            }
            let online_rows: Vec<PickerEntry> = crate::player::versions::version_rows(pool,&self.settings.config,identity).into_iter().map(|r|{
                let smart_translation_key=if track_key.is_empty(){String::new()}else{oly_contracts::config::candidate_translation_key(&track_key,&r.source,&r.id)};
                let smart_translation=!smart_translation_key.is_empty() && crate::player::translation::translation_enabled(&self.settings.config,&smart_translation_key);
                let match_badge = r.detail.strip_prefix("匹配 ").unwrap_or(&r.detail).to_owned();
                PickerEntry{title:r.title,subtitle:r.subtitle,match_badge,timing_badge:r.timing_badge,has_translation:r.has_translation,smart_translation,smart_translation_key,path:PathBuf::new(),source:r.source,id:r.id,candidate_index:r.candidate_index,selected:r.selected}}).collect();
            if custom_rows.is_empty() {
                out = online_rows;
            } else {
                let order = oly_engine::picker::SortPolicy::from_config(&self.settings.config)
                    .display_source_order(true);
                let custom_rank = order.iter().position(|s| s == "custom").unwrap_or(0);
                let mut inserted = false;
                for row in online_rows {
                    let row_rank = order.iter().position(|s| s == &row.source).unwrap_or(order.len());
                    if !inserted && custom_rank <= row_rank {
                        out.append(&mut custom_rows);
                        inserted = true;
                    }
                    out.push(row);
                }
                if !inserted {
                    out.append(&mut custom_rows);
                }
            }
            return out;
        }
        let Some(path)=self.cfg.fixture.as_ref() else{return Vec::new();};
        let Some(dir)=(if path.is_dir(){Some(path.as_path())}else{path.parent()}) else{return Vec::new();};
        let mut out=Vec::new();
        if let Ok(entries)=std::fs::read_dir(dir){for entry in entries.flatten(){
            let p=entry.path();if p.extension().and_then(|v|v.to_str())!=Some("json"){continue;}
            let name=p.file_stem().and_then(|v|v.to_str()).unwrap_or("?").to_string();
            out.push(PickerEntry{title:name,subtitle:"本地回放样例".into(),match_badge:String::new(),timing_badge:None,has_translation:true,smart_translation:false,smart_translation_key:String::new(),selected:p==*path,path:p,source:"fixture".into(),id:String::new(),candidate_index:0});
        }}
        out.sort_by(|a,b|a.title.cmp(&b.title));out
    }

    /// Whether the picker is showing. The more panel hosts the entry that opens
    /// it, so both live under the same anchor.
    pub(super) fn more_open_sub(&self) -> bool {
        self.picker_open
    }

    pub(super) fn player_base_offset_ms(&self) -> f64 {
        if self.source != Source::Live {
            return 0.0;
        }
        let app = self
            .live
            .as_ref()
            .and_then(|l| l.model.packet.as_ref())
            .filter(|p| !p.idle)
            .map(|p| p.app.as_str());
        let bucket = oly_core::mapping::player_offset_key(app);
        let offsets = &self.settings.config["lyrics"]["playerSyncOffsets"];
        if bucket == "apple" || bucket == "default" {
            offsets.get(&bucket).and_then(serde_json::Value::as_f64).unwrap_or(0.0)
        } else {
            offsets
                .get(&bucket)
                .and_then(serde_json::Value::as_f64)
                .or_else(|| offsets.get("default").and_then(serde_json::Value::as_f64))
                .unwrap_or(0.0)
        }
    }

    pub(super) fn lyric_time(&self)->f64 {self.position_ms+self.player_base_offset_ms()+self.sync.offset_ms() as f64}
    pub(super) fn update_sync(&mut self) {
        use oly_core::offsets::{song_of_packet,SongRef};
        let song=if let Some(live)=self.live.as_ref(){song_of_packet(live.model.packet.as_ref())}else{self.doc.as_ref().map(|d|SongRef{title:d.title.clone(),artist:if d.artist.is_empty(){d.artists.join(" / ")}else{d.artist.clone()},album:d.album.clone(),album_artist:String::new(),duration_ms:self.duration_ms,app:"fixture".into()})};
        let provider=self.doc_provider.as_str();
        self.sync.select(song,provider);
        if let Some(error)=self.sync.poll(){log::warn!("{error}");if let Some(live)=self.live.as_mut(){live.status=error;}}
    }
    pub(super) fn nudge_sync(&mut self, delta: i64) {
        match self.sync.nudge(delta) {
            Ok(ms) => {
                log::info!("[lyric-sync] offset_ms={ms}");
                self.pending_seek = true;
            }
            Err(error) => {
                log::warn!("{error}");
                if let Some(live) = self.live.as_mut() {
                    live.status = error;
                }
            }
        }
    }
    pub(super) fn live_action(&mut self, action: oly_platform_win::smtc::control::Action) {
        let live_now = Self::live_now_ms();
        let Some(live) = self.live.as_mut() else { return; };
        let Some(request) = live.model.prepare_control(action, oly_platform_win::smtc::now_ms()) else {
            live.status = "当前播放器不支持此操作".into(); return;
        };
        let request_id=request.request_id;
        live.model.preview_control(action,live_now);
        if !live.host.dispatch(crate::commands::HostCommand::GuardedTransport(request)) {
            live.model.control_result(request_id,false);
            live.status = "无法连接到播放器".into();
        }
        log::info!("[control-preview] action={action:?} playing={} lyric_ms={:.1} media_ms={:.1}",live.model.effective_playing(),live.model.position(live_now),live.model.media_position(live_now));
    }
    pub(super) fn can_seek_live(&self) -> bool {
        if self.source != Source::Live {
            return self.duration_ms > 0.0;
        }
        self.live.as_ref().is_some_and(|l| l.model.can_seek())
    }

    pub(super) fn live_seek(&mut self, target_ms: f64) {
        let Some(live) = self.live.as_mut() else { return; };
        let Some(request) = live.model.prepare_seek(target_ms, oly_platform_win::smtc::now_ms()) else {
            return;
        };
        if live.host.dispatch(crate::commands::HostCommand::Seek(request)) {
            self.position_ms = target_ms;
            self.pending_seek = true;
        }
    }

    pub(super) fn import_lyric_file(&mut self, path: &std::path::Path, open_picker: bool) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match live.import_custom_lyric(path) {
            Ok(entry) => {
                let doc = entry.doc();
                self.doc_auto_picked = false;
                self.auto_pick_persisted = false;
                self.doc_identity = Some(("custom".into(), entry.id));
                self.doc_provider = "custom".into();
                self.lyrics_auto_hidden = doc.instrumental;
                self.install_document(doc, false);
                self.picker_scroll_spring.set_position(0.0);
                self.picker_scroll_spring.set_target_position(0.0);
                if open_picker {
                    self.settings.open = false;
                    self.settings.close_choice();
                    self.close_volume_panel();
                    self.tool.set_more_open(true);
                    self.picker_open = true;
                }
            }
            Err(err) => {
                log::warn!("custom lyric import failed: {err}");
            }
        }
    }

    pub(super) fn on_right_down(&mut self, x: f32, y: f32) {
        let hit = pointer_hit(&self.hot, &self.panel_bounds, x, y);
        if self.settings.open {
            match hit {
                Some(Hot::DropdownOption(opt_i)) => {
                    self.settings.delete_custom_model_option(opt_i as usize, self.now_ms());
                    return;
                }
                Some(Hot::SettingChoice(row_i)) => {
                    if let Some(item) = self.settings.items().get(row_i as usize).copied() {
                        if let crate::player::settings::Control::AliasRow(alias_idx) = item.control {
                            if let Some((from, _)) = self.settings.artist_aliases().get(alias_idx).cloned() {
                                self.settings.remove_artist_alias(&from, self.now_ms());
                            }
                            return;
                        }
                    }
                }
                Some(Hot::SettingSlider(row_i)) => {
                    if self.settings.remove_player_offset(row_i as usize, self.now_ms()).is_some() {
                        self.sync_profile_display();
                        return;
                    }
                }
                _ => {
                    if let Some(pr) = self.panel_bounds.last().copied() {
                        if let Some((row_i, _, _)) = self
                            .settings
                            .rows(pr)
                            .into_iter()
                            .find(|(_, item, row)| {
                                matches!(item.control, crate::player::settings::Control::PlayerOffset(_))
                                    && row.contains(x, y)
                            })
                        {
                            if self.settings.remove_player_offset(row_i, self.now_ms()).is_some() {
                                self.sync_profile_display();
                                return;
                            }
                        }
                    }
                }
            }
        }
        let Some(Hot::Candidate(i)) = hit else {
            return;
        };
        let list = self.candidates();
        let Some(entry) = list.get(i).filter(|e| e.source == "custom") else {
            return;
        };
        let removed_id = entry.id.clone();
        let was_selected = self
            .doc_identity
            .as_ref()
            .is_some_and(|(s, id)| s == "custom" && id == &removed_id);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        if !live.remove_custom_lyric(&removed_id) {
            return;
        }
        if was_selected {
            let updated = self.candidates();
            if let Some(next_custom) = updated.iter().find(|e| e.source == "custom") {
                if let Some(doc) = self
                    .live
                    .as_ref()
                    .and_then(|l| l.custom_lyrics.get_doc(&next_custom.id))
                {
                    self.doc_identity = Some(("custom".into(), next_custom.id.clone()));
                    self.doc_provider = "custom".into();
                    self.lyrics_auto_hidden = doc.instrumental;
                    self.install_document(doc, false);
                    return;
                }
            }
            if let Some(live) = self.live.as_mut() {
                if let (Some(track), Some(candidate)) = (live.track.clone(), live.candidates.first().cloned()) {
                    live.search.request(live.model.generation(), track, Some(candidate));
                    live.status = "正在加载歌词…".into();
                } else {
                    self.doc = None;
                    self.doc_identity = None;
                    self.doc_provider.clear();
                    self.lines.clear();
                    self.rows.clear();
                }
            }
        }
    }

    pub(super) fn live_toggle(&mut self) {
        use oly_platform_win::smtc::control::Action;
        let playing = self
            .live
            .as_ref()
            .map_or(self.playing, |live| live.model.effective_playing());
        self.live_action(if playing { Action::Pause } else { Action::Play });
    }

    pub(super) fn menu_mode_track(&self) -> Option<(f32, f32)> {
        let r0 = self.hot.iter().find(|(h, _)| *h == Hot::MenuMode(0))?.1;
        let r3 = self.hot.iter().find(|(h, _)| *h == Hot::MenuMode(3))?.1;
        Some((r0.x - 1.0, (r3.right() - r0.x + 2.0).max(1.0)))
    }

    pub(super) fn settings_page_axis(&self) -> Option<(bool, f32, f32)> {
        let vertical = self.settings.kind == settings::SettingsKind::Application;
        let r0 = self.hot.iter().find(|(h, _)| *h == Hot::SettingsPage(0))?.1;
        let last = Hot::SettingsPage((self.settings.pages().len() - 1) as u8);
        let rn = self.hot.iter().find(|(h, _)| *h == last)?.1;
        if vertical {
            Some((true, r0.y - 2.0, (rn.bottom() - r0.y + 4.0).max(1.0)))
        } else {
            Some((false, r0.x - 2.0, (rn.right() - r0.x + 4.0).max(1.0)))
        }
    }

    pub(super) fn has_active_press(&self) -> bool {
        self.pressed.is_some()
            || self.drag_panel.is_some()
            || self.drag_thumb.is_some()
            || self.switch_drag.is_some()
            || self.inline_switch_drag.is_some()
            || self.source_reorder_drag.is_some()
            || self.segmented_drag.is_some()
    }

    pub(super) fn build_segmented_lens_glass(
        &mut self,
        window: &mut Window,
        kind: PanelKind,
        lens_rect: Box4,
        swell: f32,
        content_alpha: f32,
        fb: Feedback,
        scale: f32,
        vertical: bool,
    ) -> Option<PlanPanel> {
        let swell = swell.max(0.0);
        if swell <= 0.005 {
            return None;
        }
        let lens_box = Box4::new(
            lens_rect.x + fb.pull[0] * 0.35,
            lens_rect.y + fb.pull[1] * 0.25,
            lens_rect.w,
            lens_rect.h,
        );
        let pad = 44.0_f32;
        let req_w = (((lens_box.w + pad * 2.0) / 64.0).ceil().max(2.0)) as f64 * 64.0;
        let req_h = (((lens_box.h + pad * 2.0) / 64.0).ceil().max(2.0)) as f64 * 64.0;
        if !self.ensure_glass(window, kind, (req_w, req_h), scale) {
            return None;
        }
        let panel = match kind {
            PanelKind::TabLens => self.tab_lens_glass.as_mut()?,
            _ => self.segmented_lens_glass.as_mut()?,
        };
        let (pw, ph) = panel.pixels();
        let bw = pw as f64 / scale.max(0.5) as f64;
        let bh = ph as f64 / scale.max(0.5) as f64;
        let (cx, cy) = lens_box.centre();
        let left = (cx - bw as f32 * 0.5).round().max(0.0);
        let top = (cy - bh as f32 * 0.5).round().max(0.0);
        let backing = (bw, bh);
        let local_x = (lens_box.x - left).max(0.0) as f64 * scale as f64;
        let local_y = (lens_box.y - top).max(0.0) as f64 * scale as f64;
        let local_w = lens_box.w as f64 * scale as f64;
        let local_h = lens_box.h as f64 * scale as f64;
        let radius = (lens_box.h * 0.5) as f64 * scale as f64;
        panel.set_shape(
            GlassRect {
                left: local_x,
                top: local_y,
                width: local_w,
                height: local_h,
            },
            radius,
        );
        let (_, glass_alpha) = crate::player::chrome::segmented_crossfade(swell);
        let lens_alpha = (content_alpha * glass_alpha * 0.96).clamp(0.0, 0.96);
        if self.display.theme_mode == model::ThemeMode::Light {
            panel.set_tint(
                [0.96, 0.975, 1.0],
                0.10,
                [1.0, 1.0, 1.0, 0.95],
                [0.12, 0.16, 0.24, 0.58],
            );
        } else {
            panel.set_tint(
                [0.97, 0.985, 1.0],
                0.03,
                [1.0, 1.0, 1.0, 0.34],
                [0.60, 0.68, 0.82, 0.14],
            );
        }
        panel.set_lens_liquid(scale, lens_alpha, swell, vertical);
        panel.set_light([(0.38 + fb.focus[0] * 0.25).clamp(0.0, 1.0), 0.16, 0.92]);
        Some(PlanPanel {
            content_alpha,
            surface_alpha: lens_alpha,
            kind,
            rect: GlassRect {
                left: left as f64,
                top: top as f64,
                width: backing.0,
                height: backing.1,
            },
            surface: panel.surface(),
            snapshot: panel.snapshot(),
            hook: panel.hook(),
        })
    }

    pub(super) fn on_down(&mut self, x: f32, y: f32, _active: bool) {
        self.cursor = Some((x, y));
        if self.settings.open {
            if let Some(modal) = self.settings.modal.clone() {
                let panel_r = self.panel_bounds.last().copied().unwrap_or(Box4::new(0.0, 0.0, 540.0, 440.0));
                let mp = self.panel_elastics[4].pull();
                let (box_r, f1, f2, cancel, save) = Settings::modal_rects_with_offset(panel_r, &modal, mp);
                if cancel.contains(x, y) {
                    self.pressed = Some(Hot::ModalCancel);
                    self.press_gesture.begin(x, y);
                } else if save.w > 0.0 && save.contains(x, y) {
                    self.pressed = Some(Hot::ModalSave);
                    self.press_gesture.begin(x, y);
                } else if f1.contains(x, y) || f2.is_some_and(|r| r.contains(x, y)) || !box_r.contains(x, y) {
                    self.settings.handle_modal_click(x, y, panel_r, mp, self.now_ms());
                } else {
                    self.panel_press_candidate = Some(4);
                    self.press_gesture.begin(x, y);
                }
                return;
            }
        }
        let hit = pointer_hit(&self.hot,&self.panel_bounds,x,y);
        let editing_same = if let Some(Hot::SettingChoice(i))=hit {
            self.settings.items().get(i as usize).is_some_and(|item|Some(item.key)==self.settings.editing_key && matches!(item.control,Control::Text{..}))
        } else {false};
        if self.settings.editing_key.is_some() && !editing_same {self.settings.finish_edit(self.now_ms(),true);}
        self.pressed_candidate=if let Some(Hot::Candidate(i))=hit{self.candidates().get(i).map(|r|(r.source.clone(),r.id.clone(),r.candidate_index))}else{None};
        self.pressed = hit;
        self.press_gesture.begin(x,y);
        self.cursor=Some((x,y));
        if self.settings.open_choice.is_some()
            && !matches!(hit, Some(Hot::DropdownOption(_) | Hot::SettingChoice(_)))
        {
            let in_dropdown = self.panel_bounds.last().is_some_and(|panel_r| {
                self.settings.dropdown_layout(*panel_r).is_some_and(|mut d| {
                    let dp = self.panel_elastics[4].pull();
                    d.translate(dp[0], dp[1]);
                    d.rect.contains(x, y)
                })
            });
            if in_dropdown {
                self.pressed = None;
                self.drag_panel = Some(4);
                self.panel_press_candidate = None;
                return;
            }
            self.settings.close_choice();
        }
        if let Some(Hot::CoverSwitch(i)) = hit {
            let spring_key = if i == 0 { "cover:showCover" } else { "cover:motionCover" };
            let setting_key = if i == 0 { "showCover" } else { "motionCover" };
            let current_on = self.settings.value(setting_key).as_bool().unwrap_or(true);
            if let Some((_, row_r)) = self.hot.iter().find(|(h, _)| *h == Hot::CoverSwitch(i)) {
                let track = cover_switch_track_rect(*row_r);
                if track.contains(x, y) {
                    self.inline_switch_drag = Some(InlineSwitchDrag {
                        key: spring_key.into(),
                        track,
                        origin: x,
                        fraction: if current_on { 1.0 } else { 0.0 },
                        moved: false,
                    });
                    return;
                }
            }
        }
        if let Some(Hot::SmartTranslation(i)) = hit {
            if let Some(entry) = self.candidates().get(i) {
                if !entry.has_translation && !entry.smart_translation_key.is_empty() {
                    if let Some((_, row_r)) = self.hot.iter().find(|(h, _)| *h == Hot::Candidate(i)) {
                        let track = picker_smart_switch_rect(*row_r);
                        if track.contains(x, y) {
                            self.inline_switch_drag = Some(InlineSwitchDrag {
                                key: entry.smart_translation_key.clone(),
                                track,
                                origin: x,
                                fraction: if entry.smart_translation { 1.0 } else { 0.0 },
                                moved: false,
                            });
                            return;
                        }
                    }
                }
            }
        }
        if let Some(Hot::SettingChoice(index))=hit {
            if let Some((_,item,row))=self.panel_bounds.last().and_then(|r|self.settings.rows(*r).into_iter().find(|(i,_,_)|*i==index as usize)) {
                let is_source_row = item.key.starts_with("lyrics.enabledSources.");
                let track = if is_source_row {
                    Settings::source_switch_track(row)
                } else {
                    crate::player::details::switch_track(row)
                };
                if matches!(item.control,Control::Switch) && track.contains(x,y){
                    let fraction=if self.settings.value(item.key).as_bool().unwrap_or(false){1.}else{0.};
                    self.switch_drag=Some(crate::player::details::SwitchDrag{index:index as usize,track,origin:x,fraction,moved:false});return;
                }
                if is_source_row && matches!(item.control, Control::Switch) {
                    self.source_reorder_drag = Some(index as usize);
                    self.settings.source_drag = Some(index as usize);
                    self.settings.source_drag_grab_y = y - row.centre().1;
                    self.settings.source_drag_y = 0.0;
                    return;
                }
                if let Control::Segmented(options) = item.control {
                    let seg_track = Settings::segmented_track_rect(row, options.len());
                    if seg_track.contains(x, y) {
                        self.segmented_drag = Some((index as usize, item.key, seg_track, options.len()));
                        self.settings.begin_segmented_drag(item.key, x, seg_track, options.len());
                        return;
                    }
                }
            }
        }
        if matches!(hit, Some(Hot::MenuMode(_))) {
            self.staged_mode = None;
            if let Some((tx, tw)) = self.menu_mode_track() {
                self.mode_lens.update_drag(x, tx, tw, 4, 1.0);
            }
            return;
        }
        if matches!(hit, Some(Hot::SettingsPage(_))) {
            if let Some((vertical, origin, span)) = self.settings_page_axis() {
                let pos = if vertical { y } else { x };
                self.settings.tab_lens.update_drag(pos, origin, span, self.settings.pages().len(), 2.0);
            }
            return;
        }
        let panel_hit = self
            .panel_drag_targets
            .iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
            .map(|(idx, _)| *idx);
        if hit.is_none() {
            self.drag_panel = panel_hit;
            self.panel_press_candidate = None;
        } else {
            self.panel_press_candidate = panel_hit;
        }
        if hit.is_none() || matches!(hit,Some(Hot::Progress | Hot::VolumeTrack | Hot::Thumb | Hot::SettingSlider(_))) {self.activate(hit,x,y);}
    }
    pub(super) fn on_up(&mut self,x:f32,y:f32) {
        if let Some(mut drag) = self.inline_switch_drag.take() {
            drag.moved |= (x - drag.origin).abs() > 2.0;
            if drag.moved {
                drag.fraction = crate::player::details::switch_fraction(drag.track, x);
            }
            if drag.key == "__motion__" {
                let next = if drag.moved {
                    drag.fraction >= 0.5
                } else {
                    !self.display.motion_cover
                };
                self.change_setting("display.motionCover", serde_json::json!(next));
            } else if drag.key == "cover:showCover" || drag.key == "cover:motionCover" {
                let setting_key = if drag.key == "cover:showCover" {
                    "showCover"
                } else {
                    "motionCover"
                };
                let current_on = self.settings.value(setting_key).as_bool().unwrap_or(true);
                let next = if drag.moved {
                    drag.fraction >= 0.5
                } else {
                    !current_on
                };
                self.change_setting(setting_key, serde_json::json!(next));
                if drag.key == "cover:showCover" && next != current_on {
                    self.picker_open = false;
                    self.tool.set_more_open(false);
                    self.panel_motion[0] = PanelMotion::default();
                    self.panel_motion[2] = PanelMotion::default();
                    if !next {
                        self.island_wake_ms = self.now_ms();
                    }
                }
            } else {
                let current_on = self
                    .candidates()
                    .into_iter()
                    .find(|c| c.smart_translation_key == drag.key)
                    .map(|c| c.smart_translation)
                    .unwrap_or_else(|| {
                        self.settings.config["lyrics"]["translationOverrides"]
                            .get(&drag.key)
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    });
                let next = if drag.moved {
                    drag.fraction >= 0.5
                } else {
                    !current_on
                };
                let mut overrides = self.settings.config["lyrics"]["translationOverrides"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                overrides.insert(drag.key, serde_json::json!(next));
                self.change_setting("lyrics.translationOverrides", serde_json::Value::Object(overrides));
            }
            self.cancel_press();
            return;
        }
        if self.source_reorder_drag.take().is_some() {
            self.settings.finish_source_drag();
            self.cancel_press();
            return;
        }
        if let Some(mut drag)=self.switch_drag.take(){
            drag.update(x);self.settings.finish_switch(drag.index,drag.fraction,drag.moved,self.now_ms());
            self.sync_profile_display();self.cancel_press();return;
        }
        if let Some((index, _, track, count)) = self.segmented_drag.take() {
            if self.settings.finish_segmented_drag(index, x, track, count, self.now_ms()) {
                self.sync_profile_display();
            }
            self.cancel_press();
            return;
        }
        self.press_gesture.update(x,y);
        let hit=self.pressed;
        if let Some(Hot::MenuMode(initial)) = hit {
            let slot = if let Some((tx, tw)) = self.menu_mode_track() {
                self.mode_lens.slot_at_release(tx, tw, 4, x) as u8
            } else {
                initial
            };
            self.mode_lens.end_drag();
            self.staged_mode = Some(slot);
            self.cancel_press();
            return;
        }
        if let Some(Hot::SettingsPage(initial)) = hit {
            let page = if let Some((vertical, origin, span)) = self.settings_page_axis() {
                let pos = if vertical { y } else { x };
                self.settings.tab_lens.slot_at(origin, span, self.settings.pages().len(), pos)
            } else {
                initial as usize
            };
            self.settings.tab_lens.end_drag();
            self.settings.finish_edit(self.now_ms(), true);
            self.settings.set_page(page);
            self.cancel_press();
            return;
        }
        if matches!(hit, Some(Hot::DropdownOption(_))) {
            if self.drag_panel.is_none() {
                if let Some(Hot::DropdownOption(opt_i)) = pointer_hit(&self.hot, &self.panel_bounds, x, y) {
                    self.activate(Some(Hot::DropdownOption(opt_i)), x, y);
                }
            }
            self.cancel_press();
            return;
        }
        let same=hit.is_some()&&pointer_hit(&self.hot,&self.panel_bounds,x,y)==hit;
        let click=self.press_gesture.release(same);
        if click&&!matches!(hit,Some(Hot::Progress | Hot::VolumeTrack | Hot::Thumb | Hot::SettingSlider(_))) {self.activate(hit,x,y);}
        if self.source == Source::Live && hit == Some(Hot::Progress) {
            let target = self
                .hot
                .iter()
                .find(|(h, _)| *h == Hot::Progress)
                .map(|(_, r)| ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64 * self.duration_ms)
                .or(self.seek_preview_ms);
            self.seek_preview_ms = None;
            if let Some(target_ms) = target {
                self.live_seek(target_ms);
            }
        }
        self.cancel_press();
    }
    pub(super) fn cancel_press(&mut self) {
        self.seek_preview_ms=None;
        self.pressed_candidate=None;
        self.drag_panel=None;
        self.panel_press_candidate=None;
        self.inline_switch_drag=None;
        self.switch_drag=None;self.settings.switch_preview=None;
        self.source_reorder_drag=None;self.settings.source_drag=None;
        self.segmented_drag=None;self.settings.cancel_segmented_drag();
        self.pressed=None;
        self.press_gesture.cancel();
        self.drag_thumb=None;
        self.scroll.set_thumb_held(false);
        self.mode_lens.end_drag();
        self.settings.tab_lens.end_drag();
    }
    pub(super) fn click_at(&mut self,x:f32,y:f32) {self.on_down(x,y,true);self.on_up(x,y);}
    pub(super) fn activate(&mut self,hit:Option<Hot>,x:f32,y:f32) {
        if self.source == Source::Live {
            use oly_platform_win::smtc::control::Action;
            match hit {
                Some(Hot::Play) => { self.live_toggle(); return; }
                Some(Hot::Prev) => { self.live_action(Action::Prev); return; }
                Some(Hot::Next) => { self.live_action(Action::Next); return; }
                _ => {}
            }
        }
        match hit {
            Some(Hot::Play) => {
                self.playing = !self.playing;
            }
            Some(Hot::Prev) => {
                self.position_ms = (self.position_ms - 5000.0).max(0.0);
                self.pending_seek = true;
            }
            Some(Hot::Next) => {
                self.position_ms = (self.position_ms + 5000.0).min(self.duration_ms.max(0.0));
                self.pending_seek = true;
            }
            Some(Hot::More) => {
                let open = self.tool.toggle_more();
                if open {
                    self.picker_open = false;
                }
                self.close_volume_panel();
                self.settings.close_choice();
                self.settings.open=false;
                if let Some((_, fb)) = self.feedback.iter_mut().find(|(h, _)| *h == Hot::More) {
                    fb.trigger_hop(1.3);
                }
            }
            Some(Hot::LyricsToggle) => {
                if self.lyrics_hidden() {
                    self.lyrics_user_hidden = false;
                    if self.lyrics_auto_hidden {
                        self.lyrics_user_force_shown = true;
                    }
                } else if self.lyrics_auto_hidden && self.lyrics_user_force_shown {
                    self.lyrics_user_force_shown = false;
                } else {
                    self.lyrics_user_force_shown = false;
                    self.lyrics_user_hidden = true;
                }
                if let Some((_, fb)) = self.feedback.iter_mut().find(|(h, _)| *h == Hot::LyricsToggle) {
                    fb.trigger_hop(1.3);
                }
            }
            Some(Hot::Fold)=>{self.pending_mode=Some(if self.tool.mode()==Some(Mode::Mini){1}else{0});}
            Some(Hot::Volume) => {
                self.settings.close_choice();
                self.settings.open=false;
                self.picker_open = false;
                let open = self.tool.toggle_volume();
                self.tool.set_more_open(false);
                if open {
                    if self.source == Source::Live {
                        self.open_live_volume(self.now_ms());
                    } else {
                        self.volume = VolumePanel::open("", Some(self.volume.value()), VolumeSource::Internal);
                        self.volume.on_read(Some(self.volume.value()), VolumeSource::Internal);
                    }
                } else {
                    self.close_volume_panel();
                }
                if let Some((_, fb)) = self.feedback.iter_mut().find(|(h, _)| *h == Hot::Volume) {
                    fb.trigger_hop(1.3);
                }
            }
            Some(Hot::Progress) => {
                if let Some((_, r)) = self.hot.iter().find(|(h, _)| *h == Hot::Progress) {
                    let f = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64;
                    let target = f * self.duration_ms;
                    if self.source == Source::Live {
                        if self.can_seek_live() {
                            self.seek_preview_ms = Some(target);
                        }
                    } else {
                        self.position_ms = target;
                        self.pending_seek = true;
                    }
                }
            }
            Some(Hot::Thumb) => {
                self.drag_thumb = Some((y, self.scroll.offset()));
                self.scroll.set_thumb_held(true);
            }
            Some(Hot::SyncMinus)=>self.nudge_sync(-200),
            Some(Hot::SyncPlus)=>self.nudge_sync(200),
            Some(Hot::LyricRow(idx)) => {
                if let Some(row) = self.rows.get(idx) {
                    let offset_ms = self.player_base_offset_ms() + self.sync.offset_ms() as f64;
                    let target_ms = (row.start_ms - offset_ms).clamp(0.0, self.duration_ms.max(0.0));
                    if self.source == Source::Live {
                        if self.can_seek_live() {
                            self.live_seek(target_ms);
                        }
                    } else {
                        self.position_ms = target_ms;
                        self.pending_seek = true;
                    }
                }
            }
            Some(Hot::MenuMode(mode))=>{self.pending_mode=Some(mode);},
            Some(Hot::MoreRow(1)) => {
                self.picker_kind = PickerKind::Lyrics;
                self.picker_scroll_spring.set_position(0.0);
                self.picker_scroll_spring.set_target_position(0.0);
                self.picker_open = true;
            }
            Some(Hot::MoreRow(2)) => {
                self.picker_kind = PickerKind::Cover;
                self.picker_scroll_spring.set_position(0.0);
                self.picker_scroll_spring.set_target_position(0.0);
                self.picker_open = true;
            }
            Some(Hot::MoreRow(i)) => {
                let has_media = self.has_multiple_media_sessions();
                let settings_idx = if has_media { 5 } else { 4 };
                if i == 3 && self.settings_host.is_none() {
                    self.pending_desktop_lyrics = true;
                    self.tool.set_more_open(false);
                    self.picker_open = false;
                } else if has_media && i == 4 {
                    self.picker_kind = PickerKind::Media;
                    self.picker_scroll_spring.set_position(0.0);
                    self.picker_scroll_spring.set_target_position(0.0);
                    self.picker_open = true;
                } else if i == settings_idx {
                    self.settings.set_kind(settings::SettingsKind::Application);
                    self.settings.set_scroll_immediate(0.0);
                    self.settings.close_choice();
                    if matches!(self.tool.mode(),Some(Mode::Mini|Mode::Lyrics|Mode::Album)) && self.settings_host.is_none() {
                        self.settings.open = false;
                        self.pending_settings_window = Some(0);
                    } else {
                        self.settings.open = true;
                    }
                    self.tool.set_more_open(false);
                    self.picker_open = false;
                }
            }
            Some(Hot::SettingsClose)=>{self.settings.finish_edit(self.now_ms(),true);self.settings.close_choice();self.settings.open=false;}
            Some(Hot::SettingsPage(page))=>{self.settings.finish_edit(self.now_ms(),true);self.settings.set_page(page as usize);}
            Some(Hot::SettingChoice(index))=>{
                if self.settings.items().get(index as usize).is_some_and(|item|Some(item.key)==self.settings.editing_key){return;}
                if self.settings.activate_row(index as usize, self.now_ms()) {
                    self.sync_profile_display();
                }
            }
            Some(Hot::DropdownOption(opt_i))=>{
                if self.settings.select_choice_option(opt_i as usize, self.now_ms()) {
                    self.sync_profile_display();
                }
            }
            Some(Hot::ModalCancel) => {
                self.settings.modal = None;
            }
            Some(Hot::ModalSave) => {
                self.settings.commit_modal(self.now_ms());
            }
            Some(Hot::SettingSlider(index))=>{self.slide_setting(index,x);}
            Some(Hot::WindowPin) => {
                self.always_on_top = !self.always_on_top;
                self.applied_always_on_top = None;
                let patch = serde_json::json!({ "window": { "alwaysOnTop": self.always_on_top } });
                if let Some(live) = self.live.as_ref() {
                    let _ = live.config.patch(patch.clone());
                }
                if let Some(obj) = self.settings.config.as_object_mut() {
                    let win_obj = obj
                        .entry("window")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(w) = win_obj.as_object_mut() {
                        w.insert("alwaysOnTop".into(), serde_json::json!(self.always_on_top));
                    }
                }
            }
            Some(Hot::WindowMinimize) => {
                self.pending_minimize = true;
            }
            Some(Hot::WindowFullscreen) => {
                self.pending_mode = Some(if self.last_fullscreen { self.pre_fullscreen_mode } else { 3 });
            }
            Some(Hot::WindowClose) => {
                if self.settings_host.is_some() {
                    self.pending_quit = true;
                } else {
                    match self
                        .settings
                        .value("window.closeAction")
                        .as_str()
                        .unwrap_or("ask")
                    {
                        "minimize" => {
                            self.pending_minimize_to_tray = true;
                        }
                        "quit" => {
                            self.pending_quit = true;
                        }
                        _ => {
                            self.pending_close_dialog = true;
                        }
                    }
                }
            }

            Some(Hot::VolumeTrack) => {
                if let Some((_, r)) = self.hot.iter().find(|(h, _)| *h == Hot::VolumeTrack) {
                    let f = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64;
                    let value = SLIDER_MIN + f * (SLIDER_MAX - SLIDER_MIN);
                    if let Some(write) = self.volume.on_slide(value, self.now_ms()) {
                        self.send_volume_write(write);
                    }
                }
            }
            Some(Hot::CoverSwitch(i)) => {
                let setting_key = if i == 0 { "showCover" } else { "motionCover" };
                let current_on = self.settings.value(setting_key).as_bool().unwrap_or(true);
                let next = !current_on;
                self.change_setting(setting_key, serde_json::json!(next));
                if i == 0 {
                    self.picker_open = false;
                    self.tool.set_more_open(false);
                    self.panel_motion[0] = PanelMotion::default();
                    self.panel_motion[2] = PanelMotion::default();
                    if !next {
                        self.island_wake_ms = self.now_ms();
                    }
                }
            }
            Some(Hot::SmartTranslation(i)) => {
                self.toggle_smart_translation_at(i);
            }
            Some(Hot::AlbumEnter) => {
                if self.tool.mode() == Some(Mode::Mini) {
                    self.pending_mode = Some(4);
                }
            }
            Some(Hot::AlbumReturn) => {
                if self.tool.mode() == Some(Mode::Album) {
                    self.pending_mode = Some(0);
                }
            }
            Some(Hot::PickerImport) => {
                self.trigger_picker_import();
            }
            Some(Hot::PickerRefresh) => {
                if let Some((_, fb)) = self.feedback.iter_mut().find(|(h, _)| *h == Hot::PickerRefresh) {
                    fb.trigger_hop(1.8);
                }
                self.trigger_picker_refresh();
            }
            Some(Hot::Candidate(i)) => {
                self.tool.set_more_open(true);
                self.picker_open = true;
                self.pick_lyric_candidate_at(i);
            }
            None => {
                if self.settings.open && !self.panel_bounds.last().is_some_and(|r|r.contains(x,y)) {self.settings.close_choice();self.settings.open=false;}
                // A pointer down outside both panels closes them, exactly like
                // the legacy capture-phase listener.
                if !self.panel_bounds.iter().any(|r|r.contains(x,y)) && !self.hot.iter().any(|(h, r)| r.contains(x, y) && !matches!(h, Hot::Progress)) {
                    self.tool.set_more_open(false);
                    self.close_volume_panel();
                }
            }
        }
    }

    pub(super) fn change_setting(&mut self,key:&str,value:serde_json::Value) {
        self.settings.set(key,value,self.now_ms(),false);
        self.sync_profile_display();
        self.sync_settings_child_config = true;
    }
    pub(super) fn slide_setting(&mut self,index:u8,x:f32) {
        if let Some((_,r))=self.hot.iter().find(|(h,_)|*h==Hot::SettingSlider(index)) {
            self.settings.slide(index as usize,(x-r.x)/r.w.max(1.0),self.now_ms());
            self.sync_profile_display();
        }
    }
    pub(super) fn on_drag(&mut self, x: f32, y: f32) {
        if let Some(drag) = self.inline_switch_drag.as_mut() {
            drag.moved |= (x - drag.origin).abs() > 2.0;
            if drag.moved {
                drag.fraction = crate::player::details::switch_fraction(drag.track, x);
                if let Some(spring) = self.smart_switch_springs.get_mut(&drag.key) {
                    spring.set_position(drag.fraction as f64);
                }
            }
            return;
        }
        if let Some(mut cur_idx) = self.source_reorder_drag {
            if let Some(panel_rect) = self.panel_bounds.last().copied() {
                let source_rows: Vec<(usize, Box4)> = self
                    .settings
                    .rows(panel_rect)
                    .into_iter()
                    .filter(|(_, item, _)| item.key.starts_with("lyrics.enabledSources."))
                    .map(|(i, _, r)| (i, r))
                    .collect();
                if let (Some(&(_, first_r)), Some(&(_, last_r))) =
                    (source_rows.first(), source_rows.last())
                {
                    let min_cy = first_r.centre().1 - 8.0;
                    let max_cy = last_r.centre().1 + 8.0;
                    let desired_cy = (y - self.settings.source_drag_grab_y).clamp(min_cy, max_cy);
                    if let Some(&(_, cur_r)) = source_rows.iter().find(|&&(i, _)| i == cur_idx) {
                        let mut drag_dy = desired_cy - cur_r.centre().1;
                        let stride = 40.0_f32;
                        let now = self.now_ms();
                        while drag_dy < -stride * 0.5 && cur_idx > 0 {
                            let next_idx = cur_idx - 1;
                            if self.settings.move_source_order(cur_idx, next_idx, now) {
                                cur_idx = next_idx;
                                self.source_reorder_drag = Some(cur_idx);
                                self.settings.source_drag = Some(cur_idx);
                                drag_dy += stride;
                            } else {
                                break;
                            }
                        }
                        while drag_dy > stride * 0.5 && cur_idx + 1 < source_rows.len() {
                            let next_idx = cur_idx + 1;
                            if self.settings.move_source_order(cur_idx, next_idx, now) {
                                cur_idx = next_idx;
                                self.source_reorder_drag = Some(cur_idx);
                                self.settings.source_drag = Some(cur_idx);
                                drag_dy -= stride;
                            } else {
                                break;
                            }
                        }
                        self.settings.source_drag_y = drag_dy;
                    }
                }
            }
            return;
        }
        if let Some(drag)=self.switch_drag.as_mut(){drag.update(x);if drag.moved{self.settings.preview_switch(drag.index,drag.fraction);}return;}
        if let Some((_, key, track, count)) = self.segmented_drag {
            self.settings.update_segmented_drag(key, x, track, count);
            return;
        }
        self.press_gesture.update(x,y);
        if matches!(self.pressed, Some(Hot::MenuMode(_))) {
            if let Some((tx, tw)) = self.menu_mode_track() {
                self.mode_lens.update_drag(x, tx, tw, 4, 1.0);
                let slot = self.mode_lens.slot_at(tx, tw, 4, x) as u8;
                self.pressed = Some(Hot::MenuMode(slot));
            }
            return;
        }
        if matches!(self.pressed, Some(Hot::SettingsPage(_))) {
            if let Some((vertical, origin, span)) = self.settings_page_axis() {
                let pos = if vertical { y } else { x };
                let count = self.settings.pages().len();
                self.settings.tab_lens.update_drag(pos, origin, span, count, 2.0);
                let slot = self.settings.tab_lens.slot_at(origin, span, count, pos);
                if self.settings.page != slot {
                    self.settings.set_page(slot);
                }
                self.pressed = Some(Hot::SettingsPage(slot as u8));
            }
            return;
        }
        if matches!(self.pressed, Some(Hot::DropdownOption(_))) {
            if let Some(Hot::DropdownOption(opt_i)) = pointer_hit(&self.hot, &self.panel_bounds, x, y) {
                self.pressed = Some(Hot::DropdownOption(opt_i));
            }
            return;
        }
        if let Some(Hot::SettingSlider(index))=self.pressed {self.slide_setting(index,x);return;}
        if self.pressed == Some(Hot::Progress) {
            if let Some((_, r)) = self.hot.iter().find(|(h, _)| *h == Hot::Progress) {
                let f = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64;
                let target = f * self.duration_ms;
                if self.source == Source::Live {
                    if self.can_seek_live() {
                        self.seek_preview_ms = Some(target);
                    }
                } else {
                    self.position_ms = target;
                    self.pending_seek = true;
                }
            }
            return;
        }
        if let Some((origin_y, origin_offset)) = self.drag_thumb {
            self.browse_idle_ms = self.now_ms();
            // The track is vertical, so the gesture is too: one pixel of thumb
            // travel covers `span / track` pixels of document. Dragging the
            // thumb *down* moves towards later content, which the engine stores
            // as a decreasing offset.
            let span = (self.scroll.engine().max_offset() - self.scroll.engine().min_offset()).max(1.0);
            let travel = (self.chrome.scroll_track.h - self.chrome.scroll_thumb.h).max(1.0) as f64;
            let factor = span / travel;
            let dy = (y - origin_y) as f64;
            self.scroll.move_to(origin_offset - dy * factor);
        }
        if self.pressed == Some(Hot::VolumeTrack) {
            if let Some((_, r)) = self.hot.iter().find(|(h, _)| *h == Hot::VolumeTrack) {
                let f = ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0) as f64;
                let value = SLIDER_MIN + f * (SLIDER_MAX - SLIDER_MIN);
                if let Some(write) = self.volume.on_slide(value, self.now_ms()) {
                    self.send_volume_write(write);
                }
            }
        }
        if self.drag_panel.is_none()
            && self.press_gesture.dragged()
            && self.drag_thumb.is_none()
            && !matches!(
                self.pressed,
                Some(Hot::SettingSlider(_) | Hot::VolumeTrack | Hot::Progress)
            )
        {
            if let Some(idx) = self.panel_press_candidate.take() {
                self.drag_panel = Some(idx);
                self.pressed = None;
                self.pressed_candidate = None;
            }
        }
    }

    pub(super) fn on_key(&mut self, key: &str) {
        if self.settings.handle_modal_key(key, self.now_ms()) {
            return;
        }
        if self.settings.edit_key(key,self.now_ms()){return;}
        if self.source == Source::Live && key == "space" { self.live_toggle(); return; }
        if self.source == Source::Live && matches!(key, "left" | "right") {
            return;
        }
        match key {
            "escape" => {
                if self.settings.open {
                    if self.settings.open_choice.is_some() {
                        self.settings.close_choice();
                    } else {
                        self.settings.open = false;
                    }
                    return;
                }
                // Escape unwinds one level at a time: the version list is a
                // child of the more panel, so it goes first and the panel stays
                // up. A second Escape closes the panel itself.
                if self.picker_open && self.tool.panels().more_open {
                    self.picker_open = false;
                } else if self.tool.panels().more_open || self.tool.panels().volume_open {
                    self.tool.set_more_open(false);
                    self.close_volume_panel();
                } else if self.last_fullscreen && self.settings_host.is_none() {
                    self.activate(Some(Hot::WindowFullscreen), 0.0, 0.0);
                }
            }
            "f11" => {
                if self.settings_host.is_none() {
                    self.activate(Some(Hot::WindowFullscreen), 0.0, 0.0);
                }
            }
            "space" => {
                self.playing = !self.playing;
            }
            "left" => {
                self.position_ms = (self.position_ms - 5000.0).max(0.0);
                self.pending_seek = true;
            }
            "right" => {
                self.position_ms = (self.position_ms + 5000.0).min(self.duration_ms.max(0.0));
                self.pending_seek = true;
            }
            _ => {}
        }
    }
}
