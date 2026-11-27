use super::*;

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
        assert_eq!(settings.items()[0].key, "lyrics.enabledSources.amlldb");
        assert!(settings.move_source_order(0, 2, 0.0));
        assert_eq!(settings.source_order()[0], "qq");
        assert_eq!(settings.source_order()[2], "amlldb");
        assert!(settings.items().iter().any(|i| i.key == "lyrics.tierPreference"));
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
