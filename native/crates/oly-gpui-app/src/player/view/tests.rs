use super::*;

#[cfg(test)]
mod live_clock_tests {
    use super::*;

    #[::std::prelude::v1::test]
    fn live_now_ms_uses_unix_epoch_scale_aligned_with_smtc_timestamps() {
        let live_now = PlayerView::live_now_ms();
        assert!(
            live_now > 1_700_000_000_000.0,
            "live_now_ms must be Unix epoch ms so ProgressClock preserves (now - last_updated_unix_ms), got {live_now}"
        );
        #[cfg(windows)]
        {
            let smtc_now = oly_platform_win::smtc::now_ms() as f64;
            assert!(
                (live_now - smtc_now).abs() < 5_000.0,
                "live_now_ms ({live_now}) must stay in the same clock domain as smtc::now_ms ({smtc_now})"
            );
        }
    }
}

#[cfg(test)]
mod liquid_hit_tests {
    use super::*;
    #[::std::prelude::v1::test]
    fn panel_padding_blocks_controls_behind_it() {
        let panel=Box4::new(0.0,0.0,100.0,100.0);
        let hot=[(Hot::Play,panel),(Hot::MoreRow(0),Box4::new(10.0,10.0,80.0,30.0))];
        assert_eq!(pointer_hit(&hot,&[panel],4.0,4.0),None);
        assert_eq!(pointer_hit(&hot,&[panel],20.0,20.0),Some(Hot::MoreRow(0)));
        assert_eq!(pointer_hit(&hot,&[],4.0,4.0),Some(Hot::Play));
    }
}

#[cfg(test)]mod input_timeline_tests {
 use super::*;
 #[::std::prelude::v1::test]fn fixture_source_survives_the_render_document_projection(){let path=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lyrics/hard-times.json");if !path.is_file(){return;}let(doc,provider)=load_fixture(&path).unwrap();assert_eq!(provider,"kugou");assert!(!doc.lines.is_empty());}
 #[::std::prelude::v1::test]fn sync_labels_follow_the_original_direction(){assert_eq!(offset_label(0),"0ms");assert_eq!(offset_label(200),"+200ms");assert_eq!(offset_label(-200),"-200ms");}
}

#[cfg(test)] mod compact_regression_tests {
 use super::*;
 #[::std::prelude::v1::test] fn a_hidden_lyric_column_has_a_bounded_scroll_thumb() {
 let mut m=model::metrics(380.,176.,(1920.,1080.),false,&Default::default());
 m.lyrics=Box4::new(20.,190.,340.,1.);
 let chrome=Chrome::build(&m,(0.,0.,0.));
 assert!(chrome.scroll_thumb.h>0. && chrome.scroll_thumb.h<=chrome.scroll_track.h);
 }
 #[::std::prelude::v1::test] fn scroll_thumb_does_not_span_full_track_and_fold_area_stays_out_of_drag_regions() {
 let m=model::compact_metrics(380.,650.,true,&Default::default());
 let chrome=Chrome::build(&m,(0.,0.,10.));
 assert!(chrome.scroll_thumb.h<=chrome.scroll_track.h*0.66);
 let fb=crate::player::details::fold_hit_box(m.win_w,m.win_h);
 let rs=input::drag_regions(&m,[fb]);
 assert!(!rs.iter().any(|r|r.contains(m.win_w*0.5,m.win_h-6.)));
 }
 #[::std::prelude::v1::test] fn settings_general_page_keeps_accent_index_and_wires_motion_cover() {
  let general = settings::items(4);
  assert_eq!(general.get(4).map(|i| i.key), Some("uiAccentColor"));
  let mut app=Settings::default();app.set_kind(settings::SettingsKind::Application);app.page=0;
  assert!(app.items().iter().any(|i| i.key == "motionCover"));
  assert!(app.items().iter().any(|i| i.key == "motionCoverCacheMB"));
  let cfg = serde_json::json!({"display": {"motionCover": false, "motionCoverCacheMB": 1200}});
  let d = Display::from_config(&cfg);
  assert!(!d.motion_cover);
  assert_eq!(d.motion_cache_mb, 1200);
 }
}

#[cfg(test)]
mod scenario_clock_tests {
    use super::*;
    #[::std::prelude::v1::test]
    fn slow_startup_preserves_a_painted_gap_before_a_menu_hit() {
        let mut s=Scenario{steps:vec![(100.,Act::OpenMore),(300.,Act::MoreRow(1)),(850.,Act::SettingsShot(false))],..Default::default()};
        assert_eq!(s.take_due(1200.),Some(Act::OpenMore));
        assert_eq!(s.take_due(1200.),None);
        assert_eq!(s.take_due(1399.),None);
        assert_eq!(s.take_due(1400.),Some(Act::MoreRow(1)));
        assert_eq!(s.take_due(1949.),None);
        assert_eq!(s.take_due(1950.),Some(Act::SettingsShot(false)));
        assert_eq!(s.elapsed(5100.),4000.);
    }
}

#[cfg(test)]
mod window_lyrics_toggle_tests {
    use super::*;

    #[::std::prelude::v1::test]
    fn window_toolbar_orders_controls_left_to_right() {
        let m = model::metrics(1272.0, 843.0, (1920.0, 1080.0), false, &Default::default());
        let c6 = Chrome::build(&m, (0.0, 0.0, 0.0));
        assert!(c6.volume.x < c6.prev.x);
        assert!(c6.prev.x < c6.play.x);
        assert!(c6.play.x < c6.next.x);
        assert!(c6.next.x < c6.more.x);
        assert!(c6.more.x < c6.lyrics_toggle.x);
        assert!((c6.play.centre().0 - m.cover.centre().0).abs() < 0.01);
        assert!((c6.lyrics_toggle.centre().0 - c6.more.centre().0) < (c6.more.centre().0 - c6.next.centre().0));
        assert!((c6.lyrics_toggle.right() - m.toolbar.right()).abs() < 0.01);
    }

    #[::std::prelude::v1::test]
    fn lyric_source_enabled_helper_detects_all_disabled() {
        assert!(any_lyric_source_enabled(&serde_json::json!({})));
        assert!(any_lyric_source_enabled(&serde_json::json!({
            "lyrics": { "enabledSources": { "qq": false, "kugou": true } }
        })));
        assert!(!any_lyric_source_enabled(&serde_json::json!({
            "lyrics": { "enabledSources": { "qq": false, "kugou": false, "netease": false } }
        })));
    }

    #[::std::prelude::v1::test]
    fn caption_rects_fit_window_and_compact_modes() {
        let (w_zone, [w_pin, w_min, w_fs, w_close]) = window_caption_rects(1280.0, false);
        assert!(w_pin.x < w_min.x && w_min.x < w_fs.x && w_fs.x < w_close.x);
        assert!(w_close.right() <= 1280.0 && w_zone.contains(w_pin.centre().0, w_pin.centre().1) && w_zone.contains(w_close.centre().0, w_close.centre().1));
        assert_eq!(w_min.w, 28.0);

        let (c_zone, [c_pin, c_min, c_fs, c_close]) = window_caption_rects(380.0, true);
        assert!(c_pin.x < c_min.x && c_min.x < c_fs.x && c_fs.x < c_close.x);
        assert!(c_close.right() <= 380.0 && c_zone.contains(c_pin.centre().0, c_pin.centre().1) && c_zone.contains(c_close.centre().0, c_close.centre().1));
        assert!(c_min.w < w_min.w && c_min.h < w_min.h);
    }
}
