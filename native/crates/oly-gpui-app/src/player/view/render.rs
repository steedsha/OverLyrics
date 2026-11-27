use super::*;

#[cfg(feature="qa-capture")]
struct CornerBackdrop;
#[cfg(feature="qa-capture")]
impl Render for CornerBackdrop {
    fn render(&mut self,_:&mut Window,_:&mut Context<Self>)->impl IntoElement{
        div().size_full().bg(rgb(0xffffff)).child(canvas(|_,_,_|{},|bounds,_,window,_|{
            let w=f32::from(bounds.size.width);let h=f32::from(bounds.size.height);
            for (bx,by) in [(0.,0.),(w-64.,0.),(0.,h-64.),(w-64.,h-64.)]{
                for y in 0..16 {for x in 0..16 {
                    chrome::fill_round(window,bx+x as f32*4.,by+y as f32*4.,4.,4.,0.,Hsla::from(rgb(if (x+y)%2==0{0x181820}else{0xeed3ed})));
                }}
            }
        }).size_full())
    }
}

impl Render for PlayerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_minimize {
            self.pending_minimize = false;
            window.minimize_window();
        }
        if let Some(msg) = crate::updater::global().take_notice() {
            self.settings.status = msg;
        }
        if crate::updater::global().take_restart_ready() {
            self.pending_quit = true;
        }
        if self.pending_quit {
            self.pending_quit = false;
            self.settings.finish_edit(self.now_ms(), true);
            if let Some(child) = self.settings_child.take() {
                let _ = child.update(cx, |_, w, _| w.remove_window());
            }
            if let Some(child) = self.desktop_child.take() {
                let _ = child.update(cx, |_, w, _| w.remove_window());
            }
            if let Some(child) = self.tray_menu.take() {
                let _ = child.update(cx, |_, w, _| w.remove_window());
            }
            self.tray = None;
            cx.quit();
        }
        if self.settings_host.is_none() {
            if self.main_hwnd.is_none() {
                self.main_hwnd = crate::player::win32_window::hwnd_from_window(window);
            }
            if self.pending_desktop_lyrics {
                self.pending_desktop_lyrics = false;
                let this = cx.entity().downgrade();
                window.defer(cx, move |w, cx| {
                    let _ = this.update(cx, |pv, cx| {
                        pv.open_desktop_lyrics_window(w, cx);
                    });
                });
            }
        }
        if self.pending_settings_after_mode
            && self.pending_mode.is_none()
            && self.mode_transition.is_none()
            && self.tool.mode() == Some(Mode::Lyrics)
        {
            self.pending_settings_after_mode = false;
            self.pending_settings_window = Some(0);
        }
        if let Some(page) = self.pending_settings_window.take() {
            let this = cx.entity().downgrade();
            window.defer(cx, move |w, cx| {
                let _ = this.update(cx, |pv, cx| {
                    pv.open_settings_window(page, w, cx);
                });
            });
        }
        if self.settings_host.is_some() && !self.settings.open && !self.panel_motion[3].visible(){window.remove_window();return div().into_any_element();}
        if self.settings_host.is_none() {
            let effective_top = self.always_on_top && !window.is_fullscreen();
            if self.applied_always_on_top != Some(effective_top) {
                if let Some(hwnd) = crate::player::win32_window::hwnd_from_window(window) {
                    crate::player::win32_window::set_always_on_top(hwnd, effective_top);
                    self.applied_always_on_top = Some(effective_top);
                }
            }
            if let Some(mode) = self.pending_mode.take() {
                if let Some(child) = self.settings_child.take() {
                    let _ = child.update(cx, |_, w, _| w.remove_window());
                }
                let target_shape = crate::player::transition::WindowShape::from_u8(mode);
                let cur_logical = crate::player::transition::WindowShape::from_mode(
                    self.tool.mode().unwrap_or(Mode::Window),
                    window.is_fullscreen(),
                );
                let same_target = cur_logical == target_shape && self.mode_transition.is_none();
                let already_going = self
                    .mode_transition
                    .as_ref()
                    .is_some_and(|t| t.to_shape == target_shape);
                if !same_target && !already_going {
                    let wb = window.bounds();
                    let cur_rect = crate::player::transition::RectF::new(
                        f32::from(wb.origin.x),
                        f32::from(wb.origin.y),
                        f32::from(wb.size.width),
                        f32::from(wb.size.height),
                    );
                    if self.mode_transition.is_none() {
                        self.geometry_memory.record_leaving(cur_logical, cur_rect);
                        if cur_logical == crate::player::transition::WindowShape::Window
                            && !window.is_fullscreen()
                        {
                            self.wide_size = Some((cur_rect.w, cur_rect.h));
                        }
                    }
                    if target_shape == crate::player::transition::WindowShape::Fullscreen
                        && !window.is_fullscreen()
                    {
                        self.pre_fullscreen_mode = match cur_logical {
                            crate::player::transition::WindowShape::Mini => 0,
                            crate::player::transition::WindowShape::Lyrics => 1,
                            _ => 2,
                        };
                    }
                    let screen_area = window.display(cx).map(|d| {
                        let b = d.bounds();
                        crate::player::transition::RectF::new(
                            f32::from(b.origin.x),
                            f32::from(b.origin.y),
                            f32::from(b.size.width),
                            f32::from(b.size.height),
                        )
                    });
                    let work_area = window.display(cx).map(|d| {
                        let b = d.visible_bounds();
                        crate::player::transition::RectF::new(
                            f32::from(b.origin.x),
                            f32::from(b.origin.y),
                            f32::from(b.size.width),
                            f32::from(b.size.height),
                        )
                    });
                    self.close_volume_panel();
                    self.tool.set_more_open(false);
                    self.picker_open = false;
                    self.settings.open = false;
                    self.settings.close_choice();
                    self.chrome_wake_ms = if target_shape == crate::player::transition::WindowShape::Lyrics {
                        -10_000.0
                    } else {
                        self.now_ms()
                    };
                    self.cursor_idle_ms = self.now_ms();
                    self.tool.on_mode_changed(target_shape.to_mode());
                    self.sync_profile_display();
                    let from_shape = self
                        .mode_transition
                        .as_ref()
                        .filter(|t| {
                            t.to_shape != crate::player::transition::WindowShape::Fullscreen
                                || window.is_fullscreen()
                        })
                        .map(|t| t.to_shape)
                        .unwrap_or(cur_logical);
                    let target_area = if target_shape == crate::player::transition::WindowShape::Fullscreen {
                        screen_area.or(work_area)
                    } else {
                        work_area
                    };
                    let target_rect = crate::player::transition::transition_target(
                        cur_rect,
                        from_shape,
                        target_shape,
                        self.wide_size.unwrap_or((1280.0, 840.0)),
                        target_area,
                        self.geometry_memory.wide_bounds,
                        self.geometry_memory.compact_origin,
                    );
                    self.mode_transition = Some(crate::player::transition::ModeTransition::new(
                        from_shape,
                        target_shape,
                        cur_rect,
                        target_rect,
                    ));
                }
            }
        }
        let mut render_mode = self.tool.mode().unwrap_or(Mode::Window);
        let mut content_alpha = 1.0_f32;
        let mut chrome_alpha = 1.0_f32;
        let step_now = self.now_ms();
        let mut render_fullscreen = window.is_fullscreen();
        if let Some(tr) = self.mode_transition.as_mut() {
            let step = tr.step(step_now, window.is_fullscreen());
            render_mode = step.render_mode;
            content_alpha = step.content_alpha;
            chrome_alpha = step.chrome_alpha;
            render_fullscreen = if step.finished {
                tr.to_shape == crate::player::transition::WindowShape::Fullscreen
            } else {
                tr.render_fullscreen(window.is_fullscreen(), step_now)
            };
            if let Some(b) = step.bounds {
                window.set_bounds(gpui_kit::Bounds {
                    origin: point(px(b.x), px(b.y)),
                    size: gpui_kit::size(px(b.w), px(b.h)),
                });
            }
            if step.trigger_fullscreen_toggle {
                window.toggle_fullscreen();
            }
            if step.finished {
                let landed_lyrics = tr.to_shape == crate::player::transition::WindowShape::Lyrics;
                self.mode_transition = None;
                if landed_lyrics {
                    self.chrome_wake_ms = -10_000.0;
                }
            }
        }
        if self.tool.mode() == Some(Mode::Mini)
            && self.pending_mode.is_none()
            && self.mode_transition.is_none()
        {
            let h = if self.settings.open || self.panel_motion[3].visible() {
                650.0
            } else {
                176.0
            };
            if (f32::from(window.viewport_size().height) - h).abs() > 0.5 {
                window.resize(gpui_kit::size(px(380.0), px(h)));
            }
        }
        if self.settings_host.is_none()
            && !window.is_fullscreen()
            && self
                .mode_transition
                .as_ref()
                .is_none_or(|t| !t.involves_fullscreen())
        {
            let allow_resize = !matches!(self.tool.mode(), Some(Mode::Mini | Mode::Lyrics));
            if window.is_resizable() != allow_resize {
                window.set_resizable(allow_resize);
            }
            if self.tool.mode() == Some(Mode::Window)
                && self.cfg.capture_rgba.is_none()
                && self.cfg.scenario.is_none()
            {
                let vp = window.viewport_size();
                let vw = f32::from(vp.width);
                let vh = f32::from(vp.height);
                if vw < 760.0 || vh < 500.0 {
                    window.resize(gpui_kit::size(px(vw.max(760.0)), px(vh.max(500.0))));
                }
            }
        }
        let bounds=window.window_bounds();
        if Some(bounds)!=self.last_window_bounds {log::info!("[window-input] bounds={bounds:?}");self.last_window_bounds=Some(bounds);}
        if let Some(at) = self.cfg.freeze_ms {
            self.playing = false;
            self.position_ms = at;
        }
        let now = self.now_ms();
        let dt = if self.last_ms <= 0.0 {
            0.0
        } else {
            (now - self.last_ms).clamp(0.0, 250.0)
        };
        if self.settings_host.is_none() {
            self.settings.has_local_lyrics = self
                .live
                .as_ref()
                .is_some_and(|l| l.custom_lyrics.has_any());
        }
        self.settings.tick(now);
        if let Some(host)=self.settings_host.as_mut(){
            if host.last_config!=self.settings.config{
                let config=self.settings.config.clone();host.last_config=config.clone();
                let _=host.parent.update(cx,|parent,cx|{parent.settings.config=config;parent.sync_profile_display();cx.notify();});
            }
            if host.parent.upgrade().is_none(){self.settings.open=false;}
        }
        let live_now = if self.source == Source::Live { Self::live_now_ms() } else { now };
        self.advance(now, live_now);
        if self.settings_host.is_none() {
            if let Some(child_view) = self.settings_child_view.clone() {
                let push_config = self.sync_settings_child_config;
                self.sync_settings_child_config = false;
                let config = self.settings.config.clone();
                let active_player = self.settings.active_player.clone();
                let profile = self.settings.profile;
                let display_mode_label = self.settings.display_mode_label;
                let track_artist = self
                    .live
                    .as_ref()
                    .and_then(|l| l.track.as_ref())
                    .map(oly_engine::strings::primary_artist)
                    .unwrap_or_default();
                let differs = push_config
                    || child_view.upgrade().is_some_and(|v| {
                        let r = v.read(cx);
                        r.settings.active_player != active_player
                            || r.settings.current_track_artist != track_artist
                            || r.settings.profile != profile
                            || r.settings.display_mode_label != display_mode_label
                    });
                if differs {
                    let _ = child_view.update(cx, |v, cx| {
                        if push_config {
                            v.settings.config = config.clone();
                            if let Some(host) = v.settings_host.as_mut() {
                                host.last_config = config.clone();
                            }
                        }
                        v.settings.profile = profile;
                        v.settings.display_mode_label = display_mode_label;
                        v.sync_profile_display();
                        v.settings.active_player = active_player;
                        v.settings.current_track_artist = track_artist;
                        cx.notify();
                    });
                }
            } else {
                self.sync_settings_child_config = false;
            }
        }
        self.update_sync();
        self.frames += 1;

        // The scripted pass drives the very same entry points the pointer and
        // keyboard listeners call, so the recording shows the wiring rather
        // than a set of fields being assigned.
        self.apply_scenario(now, window, cx);

        self.cadence.observe(window, now);

        // An offscreen capture pins the clock for one painted frame, then
        // reads that frame's scene back on the next pass. The two passes are
        // what make the PNG the frame at `capture_at_ms` rather than whatever
        // the wall clock had reached by the time the copy ran.
        if self.cfg.capture_rgba.is_some() && !self.captured {
            if self.capture_ready {
                self.captured = true;
                self.take_offscreen_capture(window);
                cx.quit();
            } else {
                // Pin the clock before the warm-up so every frame the capture
                // could read shows the same media time.
                self.playing = false;
                if let Some(at) = self.cfg.capture_at_ms {
                    self.position_ms = at;
                }
                // The background pass needs a few frames before its first
                // present texture has content, and the glyph atlas fills on
                // first use. Capturing frame 1 would copy a cleared target.
                if self.frames >= 30 {
                    self.capture_ready = true;
                }
                window.request_animation_frame();
                cx.notify();
            }
        }
        let target_morph = if self.playing { 1.0 } else { 0.0 };
        let target_cover_scale = if self.playing { 1.0 } else { 0.74 };
        let target_cover_vis = if self.display.show_cover { 1.0 } else { 0.0 };
        if self.frames <= 1 || (self.cfg.capture_rgba.is_some() && !self.playing) {
            self.play_morph = crate::player::interaction::state_icon_spring(target_morph);
            self.cover_scale_spring.set_position(target_cover_scale);
            self.cover_vis_spring.set_position(target_cover_vis);
        } else {
            self.play_morph.set_target_position(target_morph);
            self.play_morph.update(dt / 1000.0);
            self.cover_scale_spring.set_target_position(target_cover_scale);
            self.cover_scale_spring.update(dt / 1000.0);
            self.cover_vis_spring.set_target_position(target_cover_vis);
            self.cover_vis_spring.update(dt / 1000.0);
        }

        // A bounded run writes its evidence and quits, so the harness reads
        // the numbers from the run instead of recomputing them.
        if !self.reported {
            if let Some(limit) = self.cfg.seconds {
                let elapsed=self.scenario.as_ref().map(|s|s.elapsed(now)).unwrap_or(now);
                if elapsed >= limit as f64 * 1000.0 {
                    self.reported = true;
                    self.write_reports();
                    cx.quit();
                }
            }
        }

        if self.motion_needs_clear {
            self.motion_needs_clear = false;
            self.clear_motion_surface(window);
        }
        if self.settings.open
            && (self.settings.modal.is_some() || self.settings.editing_key.is_some())
            && !self.focus.is_focused(window)
        {
            window.focus(&self.focus, cx);
        }
        if self.settings_host.is_none() {
            self.settings.current_track_artist = self
                .live
                .as_ref()
                .and_then(|l| l.track.as_ref())
                .map(oly_engine::strings::primary_artist)
                .unwrap_or_default();
        }
        if let Some(action) = self.settings.pending_action.take() {
            if let Some(host) = self.settings_host.as_mut() {
                let parent = host.parent.clone();
                if let Ok((status, reset_cfg)) = parent.update(cx, |p, cx| {
                    let res = p.execute_settings_action(action);
                    cx.notify();
                    res
                }) {
                    if let Some(fresh) = reset_cfg {
                        self.settings.config = fresh.clone();
                        host.last_config = fresh;
                        self.sync_profile_display();
                    }
                    if !status.is_empty() {
                        self.settings.status = status;
                    }
                }
            } else {
                let (status, _) = self.execute_settings_action(action);
                if !status.is_empty() {
                    self.settings.status = status;
                }
            }
        }
        if let Some(changed_artist) = self.settings.alias_changed_artist.take() {
            if let Some(host) = self.settings_host.as_ref() {
                let _ = host.parent.update(cx, |p, cx| {
                    p.notify_artist_alias_changed(&changed_artist);
                    cx.notify();
                });
            } else {
                self.notify_artist_alias_changed(&changed_artist);
            }
        }

        let mut m = self.metrics_for_mode(render_mode, window, cx);
        let cover_vis = if render_mode == Mode::Window {
            self.cover_vis_spring.position().clamp(0.0, 1.0) as f32
        } else if self.display.show_cover {
            1.0
        } else {
            0.0
        };
        let cover_scale = if render_mode == Mode::Window {
            self.cover_scale_spring.position().clamp(0.5, 1.15) as f32
        } else {
            1.0
        };
        if render_mode == Mode::Window && cover_vis > 0.001 && cover_vis < 0.999 {
            let vp_size = window.viewport_size();
            let (vw, vh) = (f32::from(vp_size.width), f32::from(vp_size.height));
            let screen = cx
                .primary_display()
                .map(|d| {
                    let b = d.bounds();
                    (f32::from(b.size.width), f32::from(b.size.height))
                })
                .unwrap_or((vw, vh));
            let mut d_on = self.display;
            d_on.show_cover = true;
            let mut d_off = self.display;
            d_off.show_cover = false;
            let m_on = model::metrics(vw, vh, screen, window.is_fullscreen(), &d_on);
            let m_off = model::metrics(vw, vh, screen, window.is_fullscreen(), &d_off);
            let lx = m_off.lyrics.x + (m_on.lyrics.x - m_off.lyrics.x) * cover_vis;
            let lw = m_off.lyrics.w + (m_on.lyrics.w - m_off.lyrics.w) * cover_vis;
            m.lyric_viewport = Box4::new(lx, m_on.lyrics.y, lw, m_on.lyrics.h);
            m.lyrics = model::lyric_content(m.lyric_viewport, m.font, vw / m.ui_scale.max(0.25), &self.display);
            m.art_height = m_on.art_height;
            let slide_left = -(1.0 - cover_vis) * (m_on.cover.w * 0.48 + m_on.wide_left);
            let col_slide = slide_left;
            m.cover = Box4::new(m_on.cover.x + slide_left, m_on.cover.y, m_on.cover.w, m_on.cover.h);
            m.cover_meta_top = m_on.cover_meta_top;
            m.progress = Box4::new(m_on.progress.x + col_slide, m_on.progress.y, m_on.progress.w, m_on.progress.h);
            m.toolbar = Box4::new(m_on.toolbar.x + col_slide, m_on.toolbar.y, m_on.toolbar.w, m_on.toolbar.h);
        }
        let center_target = if render_mode == Mode::Window && self.lyrics_hidden() {
            1.0
        } else {
            0.0
        };
        if self.frames <= 1 {
            self.lyrics_center.set_position(center_target);
            self.lyrics_reveal_alpha = if render_mode == Mode::Window && self.lyrics_hidden() {
                0.0
            } else {
                1.0
            };
        } else {
            self.lyrics_center.set_target_position(center_target);
            self.lyrics_center.update(dt / 1000.0);
        }
        let center_t = if render_mode == Mode::Window {
            self.lyrics_center.position().clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        if render_mode == Mode::Window && self.frames > 1 {
            let reveal_target = if !self.lyrics_hidden() && center_t <= 0.03 {
                1.0
            } else {
                0.0
            };
            let duration_ms = if reveal_target > 0.5 { 220.0 } else { 140.0 };
            self.lyrics_reveal_alpha = model::step_opacity(
                self.lyrics_reveal_alpha,
                reveal_target,
                dt,
                duration_ms,
                false,
            );
        } else if render_mode != Mode::Window {
            self.lyrics_reveal_alpha = 1.0;
        }
        let lyrics_alpha = if render_mode == Mode::Window {
            crate::player::transition::quintic(self.lyrics_reveal_alpha)
        } else {
            1.0
        };
        let scale = window.scale_factor();
        let phys = (
            (m.win_w * scale).round().max(1.0) as u32,
            (m.win_h * scale).round().max(1.0) as u32,
        );

        let motion_path = self.pending_motion_path.take().and_then(|(generation, path)| {
            (self.display.motion_cover
                && self
                    .live
                    .as_ref()
                    .is_some_and(|l| l.model.generation() == generation))
            .then_some(path)
        });
        let tick_playing = if self.source == Source::Live {
            self.live
                .as_ref()
                .is_some_and(|l| l.model.effective_playing())
        } else {
            self.playing
        };
        let immersion=self.display.immersion_background && (matches!(render_mode,Mode::Window) || self.display.immersion_compact);
        let render_bg = immersion && self.settings_host.is_none();
        if let Some(scene) = self.scene.as_mut() {
            if let Some(path) = motion_path {
                if let Err(error) = scene.open_motion_looped(&path) {
                    log::warn!("live motion cover {}: {error:#}", path.display());
                }
            }
            scene.set_beat(crate::player::scene::BeatConfig {
                active: render_bg,
                enabled: self.display.beat_follow,
                flash_threshold: self.display.beat_flash_threshold,
                flash_strength: self.display.beat_flash_strength,
                wobble_threshold: self.display.beat_wobble_threshold,
                wobble_strength: self.display.beat_wobble_strength,
            });
            if render_bg {
                // Cap blurred immersion background target to 1280px longest edge
                // to save up to 9x VRAM & fragment work on 2K/4K displays.
                let max_edge = phys.0.max(phys.1);
                let (bg_w, bg_h) = if max_edge > 1280 {
                    let s = 1280.0 / max_edge as f32;
                    (
                        ((phys.0 as f32 * s).round() as u32).max(1),
                        ((phys.1 as f32 * s).round() as u32).max(1),
                    )
                } else {
                    phys
                };
                scene.resize(bg_w, bg_h);
            }
            if window.is_visible(){
                if let Err(error) = scene.tick(now, self.position_ms, tick_playing, render_bg) {log::warn!("scene tick: {error:#}");}
            }
        }
        self.sync_background(window, scale, render_bg);
        if self.settings_host.is_none() && self.native_background!=Some(immersion){
            window.set_background_appearance(if immersion{WindowBackgroundAppearance::Transparent}else{WindowBackgroundAppearance::Blurred});
            self.native_background=Some(immersion);
        }
        self.sync_motion(window);
        self.sync_cover_surface(window, dt);

        // Rows first: laying them out is what gives the scroll engine its
        // bounds, and the lyric scrollbar's thumb is a function of those bounds.
        let (mut plan_rows, mut plan_interlude, mut plan_bottom) =
            if matches!(render_mode, Mode::Mini | Mode::Album) || content_alpha <= 0.005 {
                (Vec::new(), None, None)
            } else {
                self.plan_rows(&m, dt)
            };
        let row_fade = chrome_alpha * lyrics_alpha;
        if row_fade < 0.999 {
            for row in &mut plan_rows {
                row.light.base *= row_fade;
                row.light.dark *= row_fade;
                row.light.bright *= row_fade;
                row.translation_alpha *= row_fade;
                row.roman_alpha *= row_fade;
            }
            if let Some(inter) = plan_interlude.as_mut() {
                inter.opacity *= row_fade;
            }
            if let Some(bottom) = plan_bottom.as_mut() {
                bottom.alpha *= row_fade;
            }
        }

        let engine = self.scroll.engine();
        let mut chrome = Chrome::build(
            &m,
            (engine.offset(), engine.min_offset(), engine.max_offset()),
        );
        self.settings.viewport_h = m.win_h;
        match render_mode {
            Mode::Mini => {
                chrome.prev=Box4::new(m.win_w/2.-96.,88.,40.,40.);
                chrome.play=Box4::new(m.win_w/2.-24.,86.,48.,44.);
                chrome.next=Box4::new(m.win_w/2.+56.,88.,40.,40.);
                chrome.more=Box4::new(0.,0.,0.,0.);
                chrome.volume=Box4::new(0.,0.,0.,0.);
                chrome.lyrics_toggle=Box4::new(0.,0.,0.,0.);
            }
            Mode::Album => {
                let x=(m.win_w-296.)/2.;let y=m.win_h-56.;
                chrome.volume=Box4::new(x+4.,y+4.,34.,34.);
                chrome.prev=Box4::new(x+60.,y+2.,38.,38.);
                chrome.play=Box4::new(x+124.,y,48.,42.);
                chrome.next=Box4::new(x+196.,y+2.,38.,38.);
                chrome.more=Box4::new(x+258.,y+4.,34.,34.);
                chrome.lyrics_toggle=Box4::new(0.,0.,0.,0.);
            }
            Mode::Lyrics => {
                let x=(m.win_w-296.)/2.;let y=m.win_h-106.;
                chrome.volume=Box4::new(x+4.,y+8.,36.,36.);
                chrome.prev=Box4::new(x+60.,y+4.,44.,44.);
                chrome.play=Box4::new(x+120.,y,56.,52.);
                chrome.next=Box4::new(x+192.,y+4.,44.,44.);
                chrome.more=Box4::new(x+256.,y+8.,36.,36.);
                chrome.lyrics_toggle=Box4::new(0.,0.,0.,0.);
            }
            _ => {}
        }
        if render_mode == Mode::Window && center_t > 0.0001 {
            let center_left = ((m.win_w - m.column) * 0.5).max(16.0);
            let shift_x = (center_left - m.wide_left) * center_t;
            m.wide_left += shift_x;
            m.cover.x += shift_x;
            m.progress.x += shift_x;
            m.toolbar.x += shift_x;
            chrome.progress.x += shift_x;
            chrome.volume.x += shift_x;
            chrome.more.x += shift_x;
            chrome.prev.x += shift_x;
            chrome.play.x += shift_x;
            chrome.next.x += shift_x;
            chrome.lyrics_toggle.x += shift_x;
        }
        let compact_auto_hide = matches!(render_mode, Mode::Lyrics | Mode::Album);
        let compact_wake = (now - self.chrome_wake_ms < 3000.0)
            || self.tool.panels().any_open()
            || self.settings.open
            || self.pressed.is_some();
        let album_meta_wake = (now - self.chrome_wake_ms < 5200.0)
            || self.tool.panels().any_open()
            || self.settings.open
            || self.pressed.is_some();
        if compact_auto_hide {
            if chrome_alpha <= 0.01 && self.mode_transition.is_some() {
                self.compact_chrome_alpha = if compact_wake { 1.0 } else { 0.0 };
                self.album_meta_alpha = if album_meta_wake { 1.0 } else { 0.0 };
            } else {
                self.album_meta_alpha = model::step_opacity(
                    self.album_meta_alpha,
                    if album_meta_wake { 1.0 } else { 0.0 },
                    dt,
                    if album_meta_wake { 150.0 } else { 360.0 },
                    false,
                );
                self.compact_chrome_alpha = model::step_opacity(
                    self.compact_chrome_alpha,
                    if compact_wake { 1.0 } else { 0.0 },
                    dt,
                    if compact_wake { 240.0 } else { 300.0 },
                    false,
                );
            }
        } else {
            self.compact_chrome_alpha = 1.0;
            self.album_meta_alpha = 1.0;
        }
        let compact_eased = crate::player::transition::quintic(self.compact_chrome_alpha);
        if render_mode == Mode::Album {
            m.cover_meta_top = m.win_h - 72.0 - 72.0 * compact_eased;
        }
        if compact_auto_hide {
            let slide_y = (1.0 - compact_eased) * 14.0;
            chrome.prev.y += slide_y;
            chrome.play.y += slide_y;
            chrome.next.y += slide_y;
            chrome.volume.y += slide_y;
            chrome.more.y += slide_y;
            chrome.progress.y += slide_y;
        }
        let nocover_island = render_mode == Mode::Window && !self.display.show_cover && self.settings_host.is_none();
        if nocover_island && cover_vis <= 0.01 {
            if self.cover_gone_at_ms.is_none() {
                self.cover_gone_at_ms = Some(now);
            }
        } else {
            self.cover_gone_at_ms = None;
        }
        let island_ready = nocover_island
            && cover_vis <= 0.01
            && self.cover_gone_at_ms.is_some_and(|t| now - t >= 200.0);
        let island_w = 520.0_f32.min((m.win_w - 220.0).max(280.0));
        let island_h = 54.0_f32;
        let island_x = (m.win_w - island_w) * 0.5;
        let island_wake_zone = Box4::new(island_x - 24.0, 0.0, island_w + 48.0, 76.0);
        let island_awake = island_ready
            && ((now - self.island_wake_ms < 3600.0)
                || self.cursor.is_some_and(|(cx, cy)| island_wake_zone.contains(cx, cy))
                || self.tool.panels().any_open()
                || self.settings.open
                || self.pressed.is_some());
        self.island_drop = model::step_opacity(
            self.island_drop,
            if island_awake { 1.0 } else { 0.0 },
            dt,
            if island_awake { 280.0 } else { 220.0 },
            false,
        );
        let island_plan = if nocover_island && self.island_drop > 0.005 {
            let eased = crate::player::transition::quintic(self.island_drop);
            let iy = -island_h + (island_h + 14.0) * eased;
            let ir = Box4::new(island_x, iy, island_w, island_h);
            chrome.more = Box4::new(ir.right() - 36.0, iy + 10.0, 28.0, 28.0);
            chrome.volume = Box4::new(ir.right() - 68.0, iy + 10.0, 28.0, 28.0);
            chrome.next = Box4::new(ir.right() - 102.0, iy + 9.0, 30.0, 30.0);
            chrome.play = Box4::new(ir.right() - 140.0, iy + 7.0, 34.0, 34.0);
            chrome.prev = Box4::new(ir.right() - 174.0, iy + 9.0, 30.0, 30.0);
            chrome.lyrics_toggle = Box4::default();
            chrome.progress = Box4::new(ir.x + 16.0, iy + island_h - 8.5, (ir.w - 32.0).max(1.0), 2.5);
            Some((ir, eased))
        } else {
            if nocover_island && cover_vis <= 0.01 {
                chrome.prev = Box4::default();
                chrome.play = Box4::default();
                chrome.next = Box4::default();
                chrome.volume = Box4::default();
                chrome.more = Box4::default();
                chrome.lyrics_toggle = Box4::default();
                chrome.progress = Box4::default();
            }
            None
        };
        self.chrome = chrome.clone();

        let span = (engine.max_offset() - engine.min_offset()).max(0.0);
        let scrollable = !self.rows.is_empty()
            && !matches!(render_mode, Mode::Mini | Mode::Album)
            && !(render_mode == Mode::Window && self.lyrics_hidden())
            && lyrics_alpha > 0.01
            && span > 2.0
            && m.lyrics.h > 20.0;
        let thumb_held = scrollable && self.drag_thumb.is_some();
        let pull = if thumb_held {
            self.press_gesture.delta()
        } else {
            [0.0; 2]
        };
        self.thumb_elastic.step(pull, thumb_held, dt);
        let pressure = self.thumb_elastic.pressure().clamp(0.0, 1.0);
        let reveal = scrollable
            && (thumb_held
                || self.thumb_elastic.pending()
                || self
                    .cursor
                    .is_some_and(|(x, y)| m.lyric_viewport.contains(x, y) || x >= m.win_w - 36.0));
        let mut scroll_glass = None;
        if reveal && (thumb_held || pressure > 0.02) {
            let size = (32.0, (chrome.scroll_thumb.h + 20.0).ceil().max(24.0) as f64);
            if self.ensure_glass(window, PanelKind::Thumb, size, scale) {
                let shape = crate::player::scrollbar::held_shape(
                    chrome.scroll_thumb.h,
                    pressure,
                    self.thumb_elastic.pull(),
                );
                if let Some(panel) = self.thumb_glass.as_mut() {
                    panel.set_shape(
                        GlassRect {
                            left: shape.x as f64 * scale as f64,
                            top: shape.y as f64 * scale as f64,
                            width: shape.w as f64 * scale as f64,
                            height: shape.h as f64 * scale as f64,
                        },
                        shape.w as f64 * 0.5 * scale as f64,
                    );
                    panel.set_liquid(scale, (0.85 * pressure).clamp(0.15, 0.92), pressure);
                    panel.set_light([0.15, 0.1, 0.8]);
                    scroll_glass = Some(PlanPanel {
                        kind: PanelKind::Thumb,
                        content_alpha: 1.0,
                        surface_alpha: pressure,
                        rect: GlassRect {
                            left: chrome.scroll_track.x as f64 - 14.0,
                            top: chrome.scroll_thumb.y as f64 - 10.0,
                            width: size.0,
                            height: size.1,
                        },
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
            }
        }
        let prog_h = chrome.progress.h.clamp(14.0, 18.0);
        let prog_y = chrome.progress.y + (chrome.progress.h - prog_h) * 0.5;
        let mut hot: Vec<(Hot, Box4)> = vec![
            (Hot::Prev, chrome.prev),
            (Hot::Play, chrome.play),
            (Hot::Next, chrome.next),
            // The rail is `.lg-slider` at `width: calc(100% + 26px);
            // margin-inline: -13px`, so the pointer target overhangs the
            // progress column exactly like the painted bar. The seek fraction
            // is then measured over the same box the fill is drawn across.
            (
                Hot::Progress,
                Box4::new(
                    chrome.progress.x - 13.0,
                    prog_y,
                    chrome.progress.w + 26.0,
                    prog_h,
                ),
            ),
        ];
        if chrome.more.w > 0.0 && chrome.more.h > 0.0 {
            hot.push((Hot::More, chrome.more));
        }
        if chrome.volume.w > 0.0 && chrome.volume.h > 0.0 {
            hot.push((Hot::Volume, chrome.volume));
        }
        if chrome.lyrics_toggle.w > 0.0 && chrome.lyrics_toggle.h > 0.0 {
            hot.push((Hot::LyricsToggle, chrome.lyrics_toggle));
        }
        if scrollable {
            hot.push((
                Hot::Thumb,
                crate::player::scrollbar::hit(chrome.scroll_track, chrome.scroll_thumb),
            ));
        }

        if matches!(render_mode, Mode::Lyrics | Mode::Album) && self.compact_chrome_alpha < 0.08 {hot.retain(|(h,_)|!matches!(h,Hot::Prev|Hot::Play|Hot::Next|Hot::Volume|Hot::More|Hot::LyricsToggle|Hot::Progress));}
        if nocover_island && self.island_drop < 0.08 {
            hot.retain(|(h, _)| !matches!(h, Hot::Prev | Hot::Play | Hot::Next | Hot::Volume | Hot::More | Hot::LyricsToggle | Hot::Progress));
        }
        if render_mode == Mode::Mini && m.art_height > 0.0 {
            hot.push((Hot::AlbumEnter, m.cover));
        }
        let fold_box = crate::player::details::fold_hit_box(m.win_w, m.win_h);
        let compact_fold_allowed = self.settings_host.is_none()
            && matches!(self.tool.mode(), Some(Mode::Mini | Mode::Lyrics));
        if compact_fold_allowed && crate::player::details::fold_visible(self.cursor, m.win_w, m.win_h) {
            hot.push((Hot::Fold, fold_box));
        }
        let cap_allowed = self.settings_host.is_none();
        let cap_compact = matches!(render_mode, Mode::Mini | Mode::Album | Mode::Lyrics);
        let (cap_zone, [cap_pin, cap_min, cap_fs, cap_close]) = window_caption_rects(m.win_w, cap_compact);
        let album_ret = album_return_rect();
        let album_ret_zone = Box4::new(0.0, 0.0, 52.0, 48.0);
        let cap_hovered = cap_allowed
            && (self.cursor.is_some_and(|(cx, cy)| {
                (cap_zone.contains(cx, cy) || (render_mode == Mode::Album && album_ret_zone.contains(cx, cy)))
                    && !self.panel_bounds.iter().any(|b| b.contains(cx, cy))
            }) || matches!(
                self.pressed,
                Some(Hot::WindowPin | Hot::WindowMinimize | Hot::WindowFullscreen | Hot::WindowClose | Hot::AlbumReturn)
            ));
        if cap_allowed {
            self.window_controls_alpha = model::step_opacity(
                self.window_controls_alpha,
                if cap_hovered { 1.0 } else { 0.0 },
                dt,
                160.0,
                false,
            );
        } else {
            self.window_controls_alpha = 0.0;
        }
        if cap_allowed && (cap_hovered || self.window_controls_alpha > 0.05) {
            hot.push((Hot::WindowPin, cap_pin));
            hot.push((Hot::WindowMinimize, cap_min));
            hot.push((Hot::WindowFullscreen, cap_fs));
            hot.push((Hot::WindowClose, cap_close));
            if render_mode == Mode::Album {
                hot.push((Hot::AlbumReturn, album_ret));
            }
        }
        if !self.display.show_progress || !self.can_seek_live() {hot.retain(|(h,_)|*h!=Hot::Progress);}
        let (more_open, volume_open) = self.open_panels();
        let picker_open = self.tool.panels().more_open && self.more_open_sub();
        // A child replaces its parent surface; sampling both compounds the tint.
        let more_open = more_open && !picker_open;
        // Replacement panels never leave a second blurred surface underneath.
        if picker_open {self.panel_motion[0]=PanelMotion::default();}
        if more_open {self.panel_motion[2]=PanelMotion::default();}
        if volume_open {self.panel_motion[0]=PanelMotion::default();self.panel_motion[2]=PanelMotion::default();}
        if more_open || picker_open {self.panel_motion[1]=PanelMotion::default();}
        self.panel_motion[0].step(more_open,dt);
        self.panel_motion[1].step(volume_open,dt);
        self.panel_motion[2].step(picker_open,dt);
        self.panel_motion[3].step(self.settings.open,dt);
        let mut panel_elastic_pending = false;
        let raw_panel_drag = self.press_gesture.delta();
        for (idx, elastic) in self.panel_elastics.iter_mut().enumerate() {
            let held = self.drag_panel == Some(idx);
            let delta = if held {
                [raw_panel_drag[0] * 1.15, raw_panel_drag[1] * 1.15]
            } else {
                [0.0, 0.0]
            };
            elastic.step(delta, held, dt);
            panel_elastic_pending |= elastic.pending();
        }
        let refresh_searching = self.picker_refresh_active
            || self.live.as_ref().is_some_and(|l| l.status == "正在搜索歌词…");
        if refresh_searching {
            self.picker_refresh_spinning = true;
        }
        if !picker_open && !refresh_searching {
            self.picker_refresh_angle = 0.0;
            self.picker_refresh_spinning = false;
        } else if self.picker_refresh_spinning || self.picker_refresh_angle > 0.0 {
            let step = std::f32::consts::TAU * 1.45 * (dt.clamp(0.0, 100.0) as f32 / 1000.0);
            let next = self.picker_refresh_angle + step;
            if next >= std::f32::consts::TAU {
                if refresh_searching {
                    self.picker_refresh_angle = next % std::f32::consts::TAU;
                } else {
                    self.picker_refresh_angle = 0.0;
                    self.picker_refresh_spinning = false;
                }
            } else {
                self.picker_refresh_angle = next;
            }
        }
        let translation_pending=self.translation_motion.step(self.display.show_translation,false,dt);

        log::trace!(
            "panels more={more_open} volume={volume_open} picker={picker_open} glass={}/{}/{}",
            self.more_glass.is_some(),
            self.volume_glass.is_some(),
            self.picker_glass.is_some()
        );

        // Panels are glass surfaces over the frame that is already painted.
        let mut panels: Vec<PlanPanel> = Vec::new();
        let mut panel_drag_targets: Vec<(usize, Box4)> = Vec::new();
        let panel_radius = if self.settings_host.is_some(){8.0}else{self.display.corner_radius.clamp(8.0, 28.0)};
        let viewport = (m.win_w as f64, m.win_h as f64);
        let current_window_mode: u8 = if window.is_fullscreen() {
            3
        } else {
            match self.tool.mode().unwrap_or(Mode::Window) {
                Mode::Mini => 0,
                Mode::Lyrics => 1,
                _ => 2,
            }
        };
        let mut active_mode_u8 = self.staged_mode.unwrap_or(current_window_mode);
        let mut mode_well_rect = Box4::default();
        let mut mode_lens_rect = Box4::default();
        let mut mode_swell = 0.0;
        let mut mode_content_alpha = 1.0_f32;
        let show_media = self.has_multiple_media_sessions();
        if self.panel_motion[0].visible() {
            let size = input::more_size_with_media(m.win_h, show_media);
            let (mut rect, side) = self.panel_rect(chrome.more, size, viewport);
            let aim=self.cursor.map(|(x,y)|((x as f64-rect.left)/rect.width,(y as f64-rect.top)/rect.height));
            let (lx,ly,energy)=match aim {Some((x,y)) if (-0.15..=1.15).contains(&x)&&(-0.15..=1.15).contains(&y)=>(x,y,1.0),_=>(0.25,0.05,0.35)};
            self.panel_motion[0].aim_light(lx,ly,energy);
            let light=self.panel_motion[0].light();
            let presentation=&self.panel_motion[0];
            rect.top += (if side==Side::Above {1.0} else {-1.0})*presentation.offset() as f64;
            let p0 = self.panel_elastics[0].pull();
            rect.left += p0[0] as f64;
            rect.top += p0[1] as f64;
            if more_open {
                panel_drag_targets.push((0, Box4::new(rect.left as f32, rect.top as f32, rect.width as f32, rect.height as f32)));
            }
            let content_alpha=presentation.content_alpha();
            let surface_alpha=presentation.surface_alpha();
            let optical_motion=(1.0-presentation.progress()).abs().min(1.0) as f32;
            mode_content_alpha = content_alpha;
            let cell_w = (size.0 as f32 - 20.0) / 4.0;
            let mode_held = matches!(self.pressed, Some(Hot::MenuMode(_)));
            let target_mode_u8 = if mode_held {
                current_window_mode
            } else {
                self.staged_mode.unwrap_or(current_window_mode)
            };
            self.mode_lens.step(
                target_mode_u8 as f32 * cell_w + 4.0,
                (cell_w - 8.0).max(16.0),
                mode_held,
                dt,
            );
            if !mode_held {
                if let Some(staged) = self.staged_mode {
                    if self.mode_lens.settled_near_target() {
                        self.staged_mode = None;
                        if staged != current_window_mode {
                            self.pending_mode = Some(staged);
                        }
                    }
                }
            }
            let base_well = Box4::new(
                rect.left as f32 + 8.0,
                rect.top as f32 + 6.0,
                rect.width as f32 - 16.0,
                32.0,
            );
            mode_well_rect = self.mode_lens.well_rect(base_well);
            mode_lens_rect = self.mode_lens.rect(
                rect.left as f32 + 10.0,
                rect.top as f32 + 10.0,
                24.0,
                Feedback::default(),
            );
            mode_swell = self.mode_lens.swell();
            let track_x = rect.left as f32 + 10.0;
            let visual_slot = (((mode_lens_rect.centre().0 - track_x) / cell_w).floor() as i32)
                .clamp(0, 3) as u8;
            active_mode_u8 = if mode_held || self.mode_lens.pending() || self.staged_mode.is_some() {
                visual_slot
            } else {
                current_window_mode
            };
            if self.ensure_glass(window, PanelKind::More, size, scale) {
                if let Some(panel) = self.more_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::More, immersion, self.display.theme_mode);
                    panel.set_radius(panel_radius as f64 * scale as f64);
                    panel.set_liquid(scale, surface_alpha, optical_motion);
                    panel.set_light(light);
                    panels.push(PlanPanel {
                        content_alpha,
                        surface_alpha,
                        kind: PanelKind::More,
                        rect,
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
                if more_open {
                    let r=Box4::new(rect.left as f32,rect.top as f32,rect.width as f32,rect.height as f32);
                    let row_count = if show_media { 6 } else { 5 };
                    for i in 0..4 {hot.push((Hot::MenuMode(i),input::mode_rect(r,i)));}
                    for i in 1..row_count {hot.push((Hot::MoreRow(i as u8),input::row_rect_count(r,i as u8,row_count)));}
                    if self.sync.enabled(){hot.push((Hot::SyncMinus,input::sync_rect_count(r,false,row_count)));hot.push((Hot::SyncPlus,input::sync_rect_count(r,true,row_count)));}
                }
            }
        } else if let Some(staged) = self.staged_mode.take() {
            if staged != current_window_mode {
                self.pending_mode = Some(staged);
            }
        }
        if self.panel_motion[1].visible() {
            let size = (240.0, 96.0);
            let (mut rect, side) = place_popover(GlassRect {left:chrome.volume.x as f64,top:chrome.volume.y as f64,width:chrome.volume.w as f64,height:chrome.volume.h as f64},size,viewport,Some(Side::Above));
            let aim=self.cursor.map(|(x,y)|((x as f64-rect.left)/rect.width,(y as f64-rect.top)/rect.height));
            let (lx,ly,energy)=match aim {Some((x,y)) if (-0.15..=1.15).contains(&x)&&(-0.15..=1.15).contains(&y)=>(x,y,1.0),_=>(0.25,0.05,0.35)};
            self.panel_motion[1].aim_light(lx,ly,energy);
            let light=self.panel_motion[1].light();
            let presentation=&self.panel_motion[1];
            rect.top += (if side==Side::Above {1.0} else {-1.0})*presentation.offset() as f64;
            let p1 = self.panel_elastics[1].pull();
            rect.left += p1[0] as f64;
            rect.top += p1[1] as f64;
            if volume_open {
                panel_drag_targets.push((1, Box4::new(rect.left as f32, rect.top as f32, rect.width as f32, rect.height as f32)));
            }
            let content_alpha=presentation.content_alpha();
            let surface_alpha=presentation.surface_alpha();
            let optical_motion=(1.0-presentation.progress()).abs().min(1.0) as f32;
            if self.ensure_glass(window, PanelKind::Volume, size, scale) {
                if let Some(panel) = self.volume_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::Volume, immersion, self.display.theme_mode);
                    panel.set_radius(panel_radius as f64 * scale as f64);
                    panel.set_liquid(scale, surface_alpha, optical_motion);
                    panel.set_light(light);
                    panels.push(PlanPanel {
                        content_alpha,
                        surface_alpha,
                        kind: PanelKind::Volume,
                        rect,
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
                if volume_open { hot.push((
                    Hot::VolumeTrack,
                    Box4::new(
                        rect.left as f32 + 14.0,
                        rect.top as f32 + 52.0,
                        rect.width as f32 - 28.0,
                        20.0,
                    ),
                )); }
            }
        }
        if self.panel_motion[2].visible() {
            let is_cover = self.picker_kind == PickerKind::Cover;
            let list = if is_cover { Vec::new() } else { self.candidates() };
            let visible_rows = self
                .picker_locked_rows
                .filter(|_| self.picker_refresh_active)
                .unwrap_or_else(|| list.len().clamp(1, 4));
            let size = if is_cover {
                (
                    268.0_f64.min(viewport.0 - 16.0),
                    (VERSION_HEADER as f64 + 2.0 * COVER_SWITCH_ROW as f64 + 8.0).min(viewport.1 - 16.0),
                )
            } else {
                (
                    420.0_f64.min(viewport.0 - 16.0),
                    (VERSION_HEADER as f64 + visible_rows as f64 * VERSION_ROW as f64 + 4.0).min(viewport.1 - 16.0),
                )
            };
            let (mut rect, side) = self.panel_rect(chrome.more, size, viewport);
            let aim=self.cursor.map(|(x,y)|((x as f64-rect.left)/rect.width,(y as f64-rect.top)/rect.height));
            let (lx,ly,energy)=match aim {Some((x,y)) if (-0.15..=1.15).contains(&x)&&(-0.15..=1.15).contains(&y)=>(x,y,1.0),_=>(0.25,0.05,0.35)};
            self.panel_motion[2].aim_light(lx,ly,energy);
            let light=self.panel_motion[2].light();
            let presentation=&self.panel_motion[2];
            rect.top += (if side==Side::Above {1.0} else {-1.0})*presentation.offset() as f64;
            let p2 = self.panel_elastics[2].pull();
            rect.left += p2[0] as f64;
            rect.top += p2[1] as f64;
            if picker_open {
                panel_drag_targets.push((2, Box4::new(rect.left as f32, rect.top as f32, rect.width as f32, rect.height as f32)));
            }
            let content_alpha=presentation.content_alpha();
            let surface_alpha=presentation.surface_alpha();
            let optical_motion=(1.0-presentation.progress()).abs().min(1.0) as f32;
            if self.ensure_glass(window, PanelKind::Picker, size, scale) {
                if let Some(panel) = self.picker_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::Picker, immersion, self.display.theme_mode);
                    panel.set_radius(panel_radius as f64 * scale as f64);
                    panel.set_liquid(scale, surface_alpha, optical_motion);
                    panel.set_light(light);
                    panels.push(PlanPanel {
                        content_alpha,
                        surface_alpha,
                        kind: PanelKind::Picker,
                        rect,
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
                if !is_cover {
                    self.picker_capacity=(((rect.height as f32-VERSION_HEADER)/VERSION_ROW).floor() as usize).min(4);
                    let max_scroll_px = list.len().saturating_sub(self.picker_capacity) as f32 * VERSION_ROW;
                    let clamped_t = (self.picker_scroll_spring.target() as f32).clamp(0.0, max_scroll_px);
                    self.picker_scroll_spring.set_target_position(clamped_t as f64);
                    self.picker_scroll_spring.update(dt / 1000.0);
                }
                let picker_scroll_px = (self.picker_scroll_spring.position() as f32).clamp(
                    0.0,
                    list.len().saturating_sub(self.picker_capacity) as f32 * VERSION_ROW,
                );
                if picker_open {
                    if is_cover {
                        for i in 0..2u8 {
                            hot.push((
                                Hot::CoverSwitch(i),
                                cover_switch_row_rect(rect.left as f32, rect.top as f32, rect.width as f32, i),
                            ));
                        }
                    } else {
                        if self.picker_kind == PickerKind::Lyrics {
                            hot.push((Hot::PickerImport, picker_import_rect(rect)));
                            hot.push((Hot::PickerRefresh, picker_refresh_rect(rect)));
                        }
                        let list_top = rect.top as f32 + VERSION_HEADER;
                        let list_bot = list_top + self.picker_capacity as f32 * VERSION_ROW;
                        for (i,entry) in list.iter().enumerate() {
                            let y = list_top + i as f32 * VERSION_ROW - picker_scroll_px;
                            let row_r=Box4::new(rect.left as f32+8.,y,rect.width as f32-16.,VERSION_ROW-4.);
                            if row_r.bottom() <= list_top + 2.0 || row_r.y >= list_bot - 2.0 {
                                continue;
                            }
                            hot.push((Hot::Candidate(i),row_r));
                            if !entry.has_translation && !entry.smart_translation_key.is_empty() {
                                hot.push((Hot::SmartTranslation(i),picker_smart_btn_rect(row_r)));
                            }
                        }
                    }
                }
            }
        }
        let mut settings_paint=None;
        if self.panel_motion[3].visible() {
            let r=self.settings.panel_rect(m.win_w,m.win_h);
            self.settings.clamp_scroll(self.settings.max_scroll(r));
            let motion=&self.panel_motion[3];
            let content_alpha=motion.content_alpha();let surface_alpha=motion.surface_alpha();
            let offset=motion.offset();let optical_motion=(1.0-motion.progress()).abs().min(1.0) as f32;
            let r=if self.settings_host.is_none() {
                let displacement=(r.x+r.w+16.0)*(offset/14.0).clamp(0.0,1.0);
                let p3 = self.panel_elastics[3].pull();
                Box4::new(r.x - displacement + p3[0], r.y + p3[1], r.w, r.h)
            } else {
                let displacement=r.w*(offset/14.0).clamp(0.0,1.0);
                let direction=match self.settings_host.as_ref().map(|h|h.side){Some(crate::player::details::PanelSide::Left)=>1.,_=>-1.};
                Box4::new(r.x+direction*displacement,r.y,r.w,r.h)
            };
            if self.settings_host.is_none() && self.settings.open {
                panel_drag_targets.push((3, r));
            }
            let tab_held = matches!(self.pressed, Some(Hot::SettingsPage(_)));
            self.settings.step_tabs(r, tab_held, dt);
            self.settings.step_segmented(r, dt);
            let rect=GlassRect{left:r.x as f64,top:r.y as f64,width:r.w as f64,height:r.h as f64};
            let aim = self.cursor.map(|(x, y)| (((x - r.x) / r.w) as f64, ((y - r.y) / r.h) as f64));
            let (lx, ly, energy) = match aim {
                Some((x, y)) if (-0.15..=1.15).contains(&x) && (-0.15..=1.15).contains(&y) => (x, y, 0.65),
                _ => (0.25, 0.05, 0.35),
            };
            self.panel_motion[3].aim_light(lx, ly, energy);
            let light=self.panel_motion[3].light();
            if self.ensure_glass(window,PanelKind::Settings,(r.w as f64,r.h as f64),scale) {
                if let Some(panel)=self.settings_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::Settings, immersion, self.display.theme_mode);
                    panel.set_radius(panel_radius as f64 * scale as f64);
                    panel.set_liquid(scale,surface_alpha,optical_motion);panel.set_light(light);
                    panels.push(PlanPanel{content_alpha,surface_alpha,kind:PanelKind::Settings,rect,surface:panel.surface(),snapshot:panel.snapshot(),hook:panel.hook()});
                    if self.settings.open {
                        if let Some(modal) = &self.settings.modal {
                            let mp = self.panel_elastics[4].pull();
                            let (_, _, _, cancel, save) = Settings::modal_rects_with_offset(r, modal, mp);
                            hot.push((Hot::ModalCancel, cancel));
                            if save.w > 0.0 {
                                hot.push((Hot::ModalSave, save));
                            }
                        } else {
                            hot.push((Hot::SettingsClose,Settings::close_rect_for(self.settings.kind,r)));
                            for i in 0..self.settings.pages().len() as u8 {hot.push((Hot::SettingsPage(i),self.settings.tab_rect_for(r,i as usize)));}
                            let dp = self.panel_elastics[4].pull();
                            let mut drop_layout = self.settings.dropdown_layout(r);
                            if let Some(d) = drop_layout.as_mut() {
                                d.translate(dp[0], dp[1]);
                                panel_drag_targets.push((4, d.rect));
                            }
                            let hovered_opt = drop_layout.as_ref().and_then(|d| {
                                self.cursor.and_then(|(cx, cy)| {
                                    if !d.rect.contains(cx, cy) || d.options.is_empty() {
                                        return None;
                                    }
                                    let idx = ((cy - (d.rect.y + 5.0) + d.scroll) / 31.0).floor() as isize;
                                    Some(idx.clamp(0, d.options.len() as isize - 1) as usize)
                                })
                            });
                            if drop_layout.is_some() {
                                let held = matches!(self.pressed, Some(Hot::DropdownOption(_)));
                                self.settings.step_dropdown(r, hovered_opt, held, dt);
                            }
                            for (i,item,row) in self.settings.rows(r) {
                                hot.push((match item.control.slider_bounds() {Some(_)=>Hot::SettingSlider(i as u8),None=>Hot::SettingChoice(i as u8)},match item.control.slider_bounds(){Some(_)=>Box4::new(row.x+8.0,row.y+22.0,row.w-16.0,26.0),None=>row}));
                            }
                            if let Some(d) = drop_layout {
                                let opt_count = d.options.len();
                                for (opt_i, _, _, opt_r, _, _) in &d.options {
                                    let full_h = if *opt_i + 1 < opt_count { opt_r.h + 2.0 } else { opt_r.h };
                                    let top = opt_r.y.max(d.rect.y + 4.0);
                                    let bot = (opt_r.y + full_h).min(d.rect.bottom() - 4.0);
                                    if bot > top + 2.0 {
                                        hot.push((
                                            Hot::DropdownOption(*opt_i as u8),
                                            Box4::new(opt_r.x, top, opt_r.w, bot - top),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    settings_paint=Some(self.settings.presentation(r));
                    if let Some(sp) = settings_paint.as_mut() {
                        if let Some(modal) = &sp.modal {
                            let mp = self.panel_elastics[4].pull();
                            sp.modal_offset = mp;
                            let modal_box = Settings::modal_rects_with_offset(r, modal, mp).0;
                            panel_drag_targets.push((4, modal_box));
                            if self.ensure_glass(window, PanelKind::Dropdown, (modal_box.w as f64, modal_box.h as f64), scale)
                            {
                                if let Some(dp) = self.dropdown_glass.as_mut() {
                                    Self::style_popover_glass(dp, PanelKind::Picker, immersion, self.display.theme_mode);
                                    dp.set_radius(14.0 * scale as f64);
                                    dp.set_liquid(scale, content_alpha, 0.25);
                                    dp.set_light(light);
                                    sp.modal_glass = Some((
                                        GlassRect {
                                            left: modal_box.x as f64,
                                            top: modal_box.y as f64,
                                            width: modal_box.w as f64,
                                            height: modal_box.h as f64,
                                        },
                                        dp.surface(),
                                        dp.snapshot(),
                                        dp.hook(),
                                    ));
                                }
                            }
                        }
                        if let Some(d) = sp.dropdown.as_mut() {
                            let dp = self.panel_elastics[4].pull();
                            d.translate(dp[0], dp[1]);
                            if self.ensure_glass(window, PanelKind::Dropdown, (d.rect.w as f64, d.rect.h as f64), scale) {
                                if let Some(dp) = self.dropdown_glass.as_mut() {
                                    Self::style_popover_glass(dp, PanelKind::Picker, immersion, self.display.theme_mode);
                                    dp.set_radius(12.0 * scale as f64);
                                    dp.set_liquid(scale, content_alpha, 0.25);
                                    dp.set_light(light);
                                    d.glass = Some((
                                        GlassRect {
                                            left: d.rect.x as f64,
                                            top: d.rect.y as f64,
                                            width: d.rect.w as f64,
                                            height: d.rect.h as f64,
                                        },
                                        dp.surface(),
                                        dp.snapshot(),
                                        dp.hook(),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        if self.settings_host.is_some(){hot.retain(|(h,_)|matches!(h,Hot::SettingsClose|Hot::SettingsPage(_)|Hot::SettingSlider(_)|Hot::SettingChoice(_)|Hot::DropdownOption(_)|Hot::ModalCancel|Hot::ModalSave));}
        self.panel_drag_targets = panel_drag_targets;
        self.panel_bounds=panels.iter().map(|p|Box4::new(p.rect.left as f32,p.rect.top as f32,p.rect.width as f32,p.rect.height as f32)).collect();
        let hovered = self.cursor.and_then(|(x,y)| pointer_hit(&hot,&self.panel_bounds,x,y));
        self.feedback.retain(|(h,_)| hot.iter().any(|(key,_)| key == h));
        self.elastic.retain(|(h,_)|hot.iter().any(|(key,_)|key==h));
        let mut feedback_pending = false;
        for (key, _) in &hot {
            if !self.feedback.iter().any(|(h,_)| h == key) { self.feedback.push((*key, Feedback::default())); }
            let value = &mut self.feedback.iter_mut().find(|(h,_)| h == key).unwrap().1;
            let held=self.pressed==Some(*key);
            feedback_pending |= value.step(hovered == Some(*key), held, dt);
            if !self.elastic.iter().any(|(h,_)|h==key) {self.elastic.push((*key,Elastic::default()));}
            let response=&mut self.elastic.iter_mut().find(|(h,_)|h==key).unwrap().1;
            let raw_delta = self.press_gesture.delta();
            response.step(raw_delta, held, dt);
            value.pull=response.pull();value.squeeze=response.squeeze();value.press=response.pressure();
            if let Some((x,y))=self.cursor {
                let r=hot.iter().find(|(h,_)|h==key).unwrap().1;
                value.focus=[((x-r.x)/r.w.max(1.0)*2.0-1.0).clamp(-1.5,1.5),((y-r.y)/r.h.max(1.0)*2.0-1.0).clamp(-1.5,1.5)];
            }
            feedback_pending |= response.pending();
        }
        let chrome_visible = !matches!(render_mode, Mode::Lyrics | Mode::Album)
            || self.compact_chrome_alpha > 0.005;
        let hover_viewport = Box4::new(m.lyric_viewport.x, m.lyric_viewport.y,
            m.lyric_viewport.w, (m.lyric_viewport.h - if render_mode == Mode::Lyrics && chrome_visible {164.} else {0.}).max(0.));
        let hovered_lyric = if lyrics_alpha <= 0.01 {
            None
        } else {
            self.cursor.filter(|&(x,y)|
                hover_viewport.contains(x,y) && hovered.is_none()
                && !self.panel_bounds.iter().any(|b|b.contains(x,y)))
                .and_then(|(x,y)| plan_rows.iter().rev().filter_map(|r|r.hover_region)
                    .find(|(_,b)|b.contains(x,y)).map(|(i,_)|i))
        };
        feedback_pending |= self.lyric_hover.step(hovered_lyric, dt);
        let lyric_hover = self.lyric_hover.index.and_then(|i|
            plan_rows.iter().filter_map(|r|r.hover_region).find(|(j,_)|*j==i)
                .map(|(_,b)|(b,self.lyric_hover.alpha * self.display.hover_bg_alpha * chrome_alpha * lyrics_alpha)));
        if lyrics_alpha > 0.01 && (self.source != Source::Live || self.can_seek_live()) {
            let mut lyric_hots = Vec::new();
            for (idx, b) in plan_rows.iter().filter_map(|r| r.hover_region) {
                let x0 = b.x.max(hover_viewport.x);
                let y0 = b.y.max(hover_viewport.y);
                let x1 = b.right().min(hover_viewport.right());
                let y1 = b.bottom().min(hover_viewport.bottom());
                if x1 > x0 && y1 > y0 {
                    lyric_hots.push((Hot::LyricRow(idx), Box4::new(x0, y0, x1 - x0, y1 - y0)));
                }
            }
            if !lyric_hots.is_empty() {
                hot.splice(0..0, lyric_hots);
            }
        }
        self.hot = hot;
        if !panels.is_empty() {
            self.panels_painted += 1;
        }

        let fb_of = |key: Hot| {
            self.feedback
                .iter()
                .find(|(h, _)| *h == key)
                .map(|(_, f)| *f)
                .unwrap_or_default()
        };
        let scroll_line = if reveal {
            if thumb_held || pressure > 0.02 {
                let shape = crate::player::scrollbar::held_shape(
                    chrome.scroll_thumb.h,
                    pressure,
                    self.thumb_elastic.pull(),
                );
                Some((
                    Box4::new(
                        chrome.scroll_track.x - 14.0 + shape.x,
                        chrome.scroll_thumb.y - 10.0 + shape.y,
                        shape.w,
                        shape.h,
                    ),
                    (0.32 - 0.12 * pressure).max(0.16),
                ))
            } else {
                let hovered_thumb = hovered == Some(Hot::Thumb);
                Some((
                    crate::player::scrollbar::rest_line(
                        chrome.scroll_track,
                        chrome.scroll_thumb,
                        hovered_thumb,
                    ),
                    if hovered_thumb { 0.56 } else { 0.26 } * lyrics_alpha,
                ))
            }
        } else {
            None
        };
        let close_box = settings_paint
            .as_ref()
            .map(|sp| Settings::close_rect_for(sp.kind, sp.rect));
        let mut btn_candidates: Vec<(Hot, Box4, Feedback)> = Vec::new();
        if self.settings_host.is_none() && chrome_visible {
            btn_candidates.push((Hot::Play, chrome.play, fb_of(Hot::Play)));
            btn_candidates.push((Hot::Prev, chrome.prev, fb_of(Hot::Prev)));
            btn_candidates.push((Hot::Next, chrome.next, fb_of(Hot::Next)));
            btn_candidates.push((Hot::More, chrome.more, fb_of(Hot::More)));
            if self.tool.mode() != Some(Mode::Mini) {
                btn_candidates.push((Hot::Volume, chrome.volume, fb_of(Hot::Volume)));
            }
            if chrome.lyrics_toggle.w > 0.0 && chrome.lyrics_toggle.h > 0.0 {
                btn_candidates.push((
                    Hot::LyricsToggle,
                    chrome.lyrics_toggle,
                    fb_of(Hot::LyricsToggle),
                ));
            }
        }
        if let Some(cb) = close_box {
            btn_candidates.push((Hot::SettingsClose, cb, fb_of(Hot::SettingsClose)));
        }
        if self.settings_host.is_none() {
            if self.window_controls_alpha > 0.02 {
                btn_candidates.push((Hot::WindowPin, cap_pin, fb_of(Hot::WindowPin)));
                btn_candidates.push((Hot::WindowMinimize, cap_min, fb_of(Hot::WindowMinimize)));
                btn_candidates.push((Hot::WindowFullscreen, cap_fs, fb_of(Hot::WindowFullscreen)));
                btn_candidates.push((Hot::WindowClose, cap_close, fb_of(Hot::WindowClose)));
                if render_mode == Mode::Album {
                    btn_candidates.push((Hot::AlbumReturn, album_ret, fb_of(Hot::AlbumReturn)));
                }
            }
        }
        let active_btn = btn_candidates
            .into_iter()
            .filter(|(hot_key, r, fb)| {
                fb.press > 0.02
                    && self.pressed == Some(*hot_key)
                    && self.press_gesture.dragged()
                    && r.w > 0.0
                    && r.h > 0.0
            })
            .max_by(|a, b| a.2.press.total_cmp(&b.2.press));
        let mut active_slider_knob: Option<(Box4, Feedback)> = None;
        if let Some(vp) = panels.iter().find(|p| p.kind == PanelKind::Volume) {
            let vf = fb_of(Hot::VolumeTrack);
            if vf.press > 0.04 {
                let frac = (self.volume.value() / 100.0).clamp(0.0, 1.0) as f32;
                let kr = chrome::slider_knob_rect(
                    vp.rect.left as f32 + 14.0,
                    vp.rect.top as f32 + 62.0,
                    vp.rect.width as f32 - 28.0,
                    frac,
                    vf,
                );
                active_slider_knob = Some((kr, vf));
            }
        }
        if active_slider_knob.is_none() {
            if let Some(sp) = &settings_paint {
                for (i, item, row) in &sp.rows {
                    if let Some((min, max, _)) = item.control.slider_bounds() {
                        let sf = fb_of(Hot::SettingSlider(*i as u8));
                        if sf.press > 0.04 {
                            let v = sp.values[item.key].as_f64().unwrap_or(min);
                            let frac = ((v - min) / (max - min)).clamp(0.0, 1.0) as f32;
                            let kr = chrome::slider_knob_rect(
                                row.x + 8.0,
                                row.y + 34.0,
                                row.w - 16.0,
                                frac,
                                sf,
                            );
                            active_slider_knob = Some((kr, sf));
                            break;
                        }
                    }
                }
            }
        }
        let active_mode_fb = fb_of(Hot::MenuMode(active_mode_u8));
        let active_page_fb = settings_paint
            .as_ref()
            .map(|sp| fb_of(Hot::SettingsPage(sp.page as u8)))
            .unwrap_or_default();
        let close_fb = fb_of(Hot::WindowClose);
        let close_hover_lens = self.settings_host.is_none()
            && active_btn.is_none()
            && self.window_controls_alpha > 0.02
            && (close_fb.hover > 0.01 || close_fb.press > 0.01);
        let mut button_lens: Option<(Hot, PlanPanel, Box4, Feedback)> = None;
        let lens_target = active_btn.or_else(|| close_hover_lens.then_some((Hot::WindowClose, cap_close, close_fb)));
        if let Some((hot_key, r, bf)) = lens_target {
            let is_close_btn = hot_key == Hot::WindowClose;
            let press = bf.press.clamp(0.0, 1.0);
            let scale_btn = if is_close_btn && press <= 0.02 {
                1.0 + 0.06 * bf.hover
            } else {
                (0.90 + 0.14 * press).max(0.85)
            };
            let lw = r.w * scale_btn * (1.0 + bf.squeeze[0] * 0.72);
            let lh = r.h * scale_btn * (1.0 + bf.squeeze[1] * 0.72);
            let cx_b = r.x + r.w * 0.5 + bf.pull[0] * 0.55;
            let cy_b = r.y + r.h * 0.5 + bf.pull[1] * 0.55;
            let lens_box = Box4::new(cx_b - lw * 0.5, cy_b - lh * 0.5, lw, lh);
            let backing_dim = ((r.w.max(r.h) * 1.4 + 20.0).ceil()).max(56.0);
            let lens_size = (backing_dim as f64, backing_dim as f64);
            if self.ensure_glass(window, PanelKind::ButtonLens, lens_size, scale) {
                let left = (cx_b - backing_dim * 0.5)
                    .round()
                    .clamp(0.0, (m.win_w - backing_dim).max(0.0));
                let top = (cy_b - backing_dim * 0.5)
                    .round()
                    .clamp(0.0, (m.win_h - backing_dim).max(0.0));
                if let Some(panel) = self.button_lens_glass.as_mut() {
                    let local_x = (lens_box.x - left).max(0.0) as f64 * scale as f64;
                    let local_y = (lens_box.y - top).max(0.0) as f64 * scale as f64;
                    let local_w = lw as f64 * scale as f64;
                    let local_h = lh as f64 * scale as f64;
                    let radius = lw.min(lh) as f64 * 0.5 * scale as f64;
                    panel.set_shape(
                        GlassRect {
                            left: local_x,
                            top: local_y,
                            width: local_w,
                            height: local_h,
                        },
                        radius,
                    );
                    if is_close_btn {
                        let ht = (bf.hover * 0.85 + press * 0.15).clamp(0.0, 1.0);
                        panel.set_tint(
                            [0.90, 0.17, 0.22],
                            (0.42 + 0.18 * ht) as f64,
                            [1.0, 0.76, 0.78, 0.62],
                            [0.55, 0.08, 0.10, 0.28],
                        );
                        let swell_c = (bf.hover * 0.45 + press * 0.48).clamp(0.0, 0.98);
                        let lens_alpha = (self.window_controls_alpha * (0.90 * bf.hover).max(press * 1.15)).clamp(0.0, 0.96);
                        panel.set_lens_liquid(scale, lens_alpha, swell_c, false);
                    } else {
                        if self.display.theme_mode == model::ThemeMode::Light {
                            panel.set_tint(
                                [0.96, 0.975, 1.0],
                                0.09,
                                [1.0, 1.0, 1.0, 0.90],
                                [0.12, 0.17, 0.26, 0.50],
                            );
                        } else {
                            panel.set_tint(
                                [0.97, 0.985, 1.0],
                                0.04,
                                [1.0, 1.0, 1.0, 0.42],
                                [0.60, 0.68, 0.82, 0.16],
                            );
                        }
                        let swell_b = (press * 0.85).clamp(0.0, 1.0);
                        let lens_alpha = (press * 1.15).clamp(0.0, 0.96);
                        panel.set_lens_liquid(scale, lens_alpha, swell_b, false);
                    }
                    panel.set_light([(0.38 + bf.focus[0] * 0.25).clamp(0.0, 1.0), 0.16, 0.92]);
                    let p = PlanPanel {
                        content_alpha: 1.0,
                        surface_alpha: if is_close_btn {
                            (self.window_controls_alpha * (0.90 * bf.hover).max(press * 1.15)).clamp(0.0, 0.96)
                        } else {
                            (press * 1.15).clamp(0.0, 0.96)
                        },
                        kind: PanelKind::ButtonLens,
                        rect: GlassRect {
                            left: left as f64,
                            top: top as f64,
                            width: lens_size.0,
                            height: lens_size.1,
                        },
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    };
                    if hot_key == Hot::SettingsClose {
                        if let Some(sp) = settings_paint.as_mut() {
                            sp.close_lens = Some((p.rect, p.surface, p.snapshot, p.hook));
                        }
                    } else {
                        button_lens = Some((hot_key, p, lens_box, bf));
                    }
                }
            }
        }
        let mut slider_lens = None;
        if let Some((kr, sf)) = active_slider_knob {
            let lens_size = (40.0, 40.0);
            if self.ensure_glass(window, PanelKind::SliderLens, lens_size, scale) {
                let (cx, cy) = kr.centre();
                let left = (cx - 20.0).round();
                let top = (cy - 20.0).round();
                if let Some(panel) = self.slider_lens_glass.as_mut() {
                    let local_x = (kr.x - left).max(0.0) as f64 * scale as f64;
                    let local_y = (kr.y - top).max(0.0) as f64 * scale as f64;
                    let local_w = kr.w as f64 * scale as f64;
                    let local_h = kr.h as f64 * scale as f64;
                    let radius = kr.w.min(kr.h) as f64 * 0.5 * scale as f64;
                    panel.set_shape(
                        GlassRect {
                            left: local_x,
                            top: local_y,
                            width: local_w,
                            height: local_h,
                        },
                        radius,
                    );
                    let lens_alpha = (sf.press * 1.15).clamp(0.0, 0.92);
                    panel.set_liquid(scale * 1.25, lens_alpha, 1.0);
                    panel.set_light([
                        (0.35 + sf.focus[0] * 0.25).clamp(0.0, 1.0),
                        0.18,
                        0.88,
                    ]);
                    slider_lens = Some(PlanPanel {
                        content_alpha: 1.0,
                        surface_alpha: lens_alpha,
                        kind: PanelKind::SliderLens,
                        rect: GlassRect {
                            left: left as f64,
                            top: top as f64,
                            width: lens_size.0,
                            height: lens_size.1,
                        },
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
            }
        }

        let mut mode_lens_glass = None;
        if self.panel_motion[0].visible() {
            mode_lens_glass = self.build_segmented_lens_glass(
                window,
                PanelKind::SegmentedLens,
                mode_lens_rect,
                mode_swell,
                mode_content_alpha,
                active_mode_fb,
                scale,
                false,
            );
        } else if let Some(sp) = settings_paint.as_mut() {
            let settings_alpha = self.panel_motion[3].content_alpha();
            if sp.tab_swell > 0.005 {
                let vertical = sp.kind == settings::SettingsKind::Application;
                if let Some(panel) = self.build_segmented_lens_glass(
                    window,
                    PanelKind::TabLens,
                    sp.tab_lens_rect,
                    sp.tab_swell,
                    settings_alpha,
                    active_page_fb,
                    scale,
                    vertical,
                ) {
                    sp.tab_lens_glass = Some((panel.rect, panel.surface, panel.snapshot, panel.hook));
                }
            }
            if let Some((&seg_key, seg)) = sp
                .segmented
                .iter()
                .filter(|(_, s)| s.swell > 0.005)
                .max_by(|a, b| a.1.swell.total_cmp(&b.1.swell))
            {
                let lens_rect = seg.lens_rect;
                let swell = seg.swell;
                let seg_fb = sp
                    .rows
                    .iter()
                    .find(|(_, item, _)| item.key == seg_key)
                    .map(|(idx, _, _)| {
                        let key = Hot::SettingChoice(*idx as u8);
                        self.feedback
                            .iter()
                            .find(|(h, _)| *h == key)
                            .map(|(_, f)| *f)
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                if let Some(panel) = self.build_segmented_lens_glass(
                    window,
                    PanelKind::SegmentedLens,
                    lens_rect,
                    swell,
                    settings_alpha,
                    seg_fb,
                    scale,
                    false,
                ) {
                    if let Some(seg_mut) = sp.segmented.get_mut(seg_key) {
                        seg_mut.glass = Some((panel.rect, panel.surface, panel.snapshot, panel.hook));
                    }
                }
            }
        }

        // The reference has no status chip in the window chrome, so the live
        // window stays visually identical at rest. A real status (searching,
        // control refused, init failure) still surfaces in the same corner.
        let badge = match self.source {
            Source::Replay => Some(format!(
                "REPLAY · {:.1}x",
                if self.playing { 1.0 } else { 0.0 }
            )),
            Source::Live => None,
        };
        let status = if self.volume.status().is_editable() {
            format!("{}%", self.volume.value().round() as i32)
        } else {
            "—".into()
        };
        let volume_label = self.volume.status().label().to_string();
        let volume_available = self.source != Source::Live
            || self
                .live
                .as_ref()
                .and_then(|l| l.model.packet.as_ref())
                .is_some_and(|p| !p.idle && !p.app.trim().is_empty());
        let dt_s = (dt.clamp(0.0, 100.0)) / 1000.0;
        let motion_frame_ready = self.motion_surface.is_some()
            && self
                .scene
                .as_ref()
                .is_some_and(PlayerScene::has_motion_frame);
        if self.display.motion_cover {
            if motion_frame_ready {
                self.motion_fade = (self.motion_fade + (dt_s as f32) / 0.45).min(1.0);
            }
        } else if self.motion_surface.is_some() && self.motion_fade > 0.0 {
            self.motion_fade = (self.motion_fade - (dt_s as f32) / 0.36).max(0.0);
            if self.motion_fade <= 0.001 {
                self.motion_fade = 0.0;
                self.clear_motion_surface(window);
            }
        } else {
            self.motion_fade = 0.0;
            if self.motion_surface.is_some() {
                self.clear_motion_surface(window);
            }
        }
        let motion_alpha = {
            let s = self.motion_fade.clamp(0.0, 1.0);
            s * s * (3.0 - 2.0 * s)
        };
        let empty_notice = if render_mode == Mode::Window
            && self.rows.is_empty()
            && !self.lyrics_hidden()
            && lyrics_alpha > 0.01
        {
            if self.source == Source::Live {
                self.live.as_ref().map(|live| {
                    if live.track.is_none() {
                        "未在播放".to_string()
                    } else if !live.status.is_empty() {
                        live.status.clone()
                    } else {
                        "未获取到歌词".to_string()
                    }
                })
            } else {
                None
            }
        } else {
            None
        };
        let candidates = if self.panel_motion[2].visible() && self.picker_kind != PickerKind::Cover {
            self.candidates()
        } else {
            Vec::new()
        };
        let mut smart_switch_pending = false;
        let mut smart_switch_fractions = Vec::new();
        let mut cover_switch_fractions = [
            if self.settings.value("showCover").as_bool().unwrap_or(true) { 1.0 } else { 0.0 },
            if self.settings.value("motionCover").as_bool().unwrap_or(true) { 1.0 } else { 0.0 },
        ];
        if self.panel_motion[2].visible() {
            if self.picker_kind == PickerKind::Cover {
                for (idx, (spring_key, setting_key)) in [
                    ("cover:showCover", "showCover"),
                    ("cover:motionCover", "motionCover"),
                ]
                .into_iter()
                .enumerate()
                {
                    let on = self.settings.value(setting_key).as_bool().unwrap_or(true);
                    let target = if on { 1.0 } else { 0.0 };
                    let spring = self
                        .smart_switch_springs
                        .entry(spring_key.to_string())
                        .or_insert_with(|| crate::player::interaction::state_icon_spring(target));
                    let frac = if let Some(drag) = self
                        .inline_switch_drag
                        .as_ref()
                        .filter(|d| d.key == spring_key && d.moved)
                    {
                        let v = drag.fraction.clamp(0.0, 1.0);
                        spring.set_position(v as f64);
                        v
                    } else {
                        spring.set_target_position(target);
                        spring.update(dt_s);
                        smart_switch_pending |= !spring.arrived();
                        spring.position().clamp(0.0, 1.0) as f32
                    };
                    cover_switch_fractions[idx] = frac;
                }
            }
            for entry in &candidates {
                if !entry.has_translation && !entry.smart_translation_key.is_empty() {
                    let target = if entry.smart_translation { 1.0 } else { 0.0 };
                    let spring = self
                        .smart_switch_springs
                        .entry(entry.smart_translation_key.clone())
                        .or_insert_with(|| crate::player::interaction::state_icon_spring(target));
                    let frac = if let Some(drag) = self
                        .inline_switch_drag
                        .as_ref()
                        .filter(|d| d.key == entry.smart_translation_key && d.moved)
                    {
                        let v = drag.fraction.clamp(0.0, 1.0);
                        spring.set_position(v as f64);
                        v
                    } else {
                        spring.set_target_position(target);
                        spring.update(dt_s);
                        smart_switch_pending |= !spring.arrived();
                        spring.position().clamp(0.0, 1.0) as f32
                    };
                    smart_switch_fractions.push((entry.smart_translation_key.clone(), frac));
                }
            }
        }
        let mut display_for_plan = self.display;
        if display_for_plan.accent_auto {
            display_for_plan.accent_hex = self.cover_accent_hex.unwrap_or(display_for_plan.accent_hex);
        }
        if let Some(sp) = settings_paint.as_mut() {
            sp.accent_hex = display_for_plan.accent_hex;
        }
        let mut island_glass_panel = None;
        if let Some((ir, eased)) = island_plan {
            let size = (ir.w as f64, ir.h as f64);
            if self.ensure_glass(window, PanelKind::Island, size, scale) {
                if let Some(panel) = self.island_glass.as_mut() {
                    Self::style_popover_glass(panel, PanelKind::Island, immersion, self.display.theme_mode);
                    panel.set_radius(18.0 * scale as f64);
                    panel.set_liquid(scale, eased, (1.0 - eased).abs().min(1.0));
                    panel.set_light([0.25, 0.08, 0.45]);
                    island_glass_panel = Some(PlanPanel {
                        content_alpha: eased,
                        surface_alpha: eased,
                        kind: PanelKind::Island,
                        rect: GlassRect {
                            left: ir.x as f64,
                            top: ir.y as f64,
                            width: ir.w as f64,
                            height: ir.h as f64,
                        },
                        surface: panel.surface(),
                        snapshot: panel.snapshot(),
                        hook: panel.hook(),
                    });
                }
            }
        }
        if !self.panel_motion[0].visible() {
            if let Some(mut old) = self.more_glass.take() { old.destroy(window); }
        }
        if !self.panel_motion[1].visible() {
            if let Some(mut old) = self.volume_glass.take() { old.destroy(window); }
        }
        if !self.panel_motion[2].visible() {
            if let Some(mut old) = self.picker_glass.take() { old.destroy(window); }
        }
        if !self.panel_motion[3].visible() {
            if let Some(mut old) = self.settings_glass.take() { old.destroy(window); }
            if let Some(mut old) = self.dropdown_glass.take() { old.destroy(window); }
        } else if self.settings.open_choice.is_none() && self.settings.modal.is_none() {
            if let Some(mut old) = self.dropdown_glass.take() { old.destroy(window); }
        }
        if island_glass_panel.is_none() {
            if let Some(mut old) = self.island_glass.take() { old.destroy(window); }
        }
        if scroll_glass.is_none() {
            if let Some(mut old) = self.thumb_glass.take() { old.destroy(window); }
        }
        if slider_lens.is_none() {
            if let Some(mut old) = self.slider_lens_glass.take() { old.destroy(window); }
        }
        let close_lens_used = settings_paint.as_ref().is_some_and(|sp| sp.close_lens.is_some());
        if button_lens.is_none() && !close_lens_used {
            if let Some(mut old) = self.button_lens_glass.take() { old.destroy(window); }
        }
        let seg_lens_used = mode_lens_glass.is_some()
            || settings_paint
                .as_ref()
                .is_some_and(|sp| sp.segmented.values().any(|s| s.glass.is_some()));
        if !seg_lens_used {
            if let Some(mut old) = self.segmented_lens_glass.take() { old.destroy(window); }
        }
        let tab_lens_used = settings_paint.as_ref().is_some_and(|sp| sp.tab_lens_glass.is_some());
        if !tab_lens_used {
            if let Some(mut old) = self.tab_lens_glass.take() { old.destroy(window); }
        }
        self.last_fullscreen = window.is_fullscreen();
        let plan = Plan {settings_only:self.settings_host.is_some(),
            immersion,
            metrics: m.clone(),
            chrome: chrome.clone(),
            bg: if !immersion || !self.scene.as_ref().is_some_and(PlayerScene::has_background) { None } else { self.bg_surface },
            cover: self.cover_surface,
            prev_cover: self.prev_cover_surface,
            cover_fade: self.cover_fade,
            motion: if motion_alpha > 0.001 { self.motion_surface } else { None },
            motion_alpha,
            rows: plan_rows,
            lyric_hover,
            interlude: plan_interlude,
            bottom: plan_bottom,
            panels,
            position_ms: self.seek_preview_ms.unwrap_or_else(|| self.live.as_ref().map(|l|l.model.media_position(live_now)).unwrap_or(self.position_ms)),
            duration_ms: self.duration_ms,
            play_morph: self.play_morph.position() as f32,
            translation: self.display.show_translation,
            badge,
            status,
            track_title: self.live.as_ref().and_then(|l| l.model.packet.as_ref()).map(|p| p.title.clone()).or_else(|| self
                .doc
                .as_ref()
                .map(|d| d.title.clone()))
                .unwrap_or_else(|| if self.source == Source::Live { "未在播放".into() } else { "暂无歌词".into() }),
            // The cover caption uses `artistLabel` from `src/shared/track-labels.ts`:
            // providers repeat the album inside the artist field, and the
            // reference strips it for display only.
            track_artist: self
                .live
                .as_ref()
                .and_then(|l| l.model.packet.as_ref())
                .map(|p| chrome::artist_label(&p.artist, &p.album))
                .or_else(|| {
                    self.doc.as_ref().map(|d| {
                        let raw = if d.artist.is_empty() {
                            d.artists.join(" / ")
                        } else {
                            d.artist.clone()
                        };
                        chrome::artist_label(&raw, &d.album)
                    })
                })
                .unwrap_or_default(),
            cursor: self.cursor,
            feedback: self.feedback.clone(),
            volume_enabled: self.volume.status().is_editable(),
            lyric_position_ms:self.lyric_time(),sync_enabled:self.sync.enabled(),sync_label:offset_label(self.sync.offset_ms()),
            volume_label,
            volume_available,
            always_on_top: self.always_on_top,
            chrome_visible,
            content_alpha,
            chrome_alpha,
            compact_chrome_alpha: self.compact_chrome_alpha,
            album_meta_alpha: self.album_meta_alpha,
            cover_scale,
            cover_vis,
            picker_scroll_px: self.picker_scroll_spring.position().max(0.0) as f32,
            picker_kind: self.picker_kind,
            show_media_row: show_media,
            picker_refresh_angle: self.picker_refresh_angle,
            mode: render_mode,
            fullscreen: render_fullscreen,
            control_pending: self.live.as_ref().is_some_and(|l| l.model.control_pending()),
            lyrics_visible: !self.lyrics_hidden(),
            lyrics_alpha,
            smart_switch_fractions,
            cover_switch_fractions,
            empty_notice,
            island: island_plan,
            island_glass_panel,
            display: display_for_plan,
            settings_paint,
            scroll_glass,
            scroll_line,
            slider_lens,
            button_lens,
            window_controls_alpha: self.window_controls_alpha,
            mode_well_rect,
            mode_lens_rect,
            mode_swell,
            mode_lens_glass,
            active_mode: active_mode_u8,
            drag_thumb: self.drag_thumb.map(|(x, y)| (x, y as f32)),
        };

        let picked = self.picked;
        let volume_value = self.volume.value();
        let more_items = input::MORE_LABELS;

        // The frame must be recomputed while anything moves.
        // A paused player still needs frames while a scripted pass or a bounded
        // run is pending: the pass advances on wall time, so stopping the frame
        // loop would freeze the scenario (and the exit timer) mid-clip.
        let animating = self.settings.animating()
            || !self.picker_scroll_spring.arrived()
            || self.settings.status_alpha > 0.001
            || !self.lyrics_center.arrived()
            || (self.lyrics_reveal_alpha > 0.001 && self.lyrics_reveal_alpha < 0.999)
            || !self.cover_scale_spring.arrived()
            || !self.cover_vis_spring.arrived()
            || self.mode_lens.pending()
            || self.staged_mode.is_some()
            || self.pending_mode.is_some()
            || self.mode_transition.is_some()
            || self.volume.has_pending_write()
            || self.cover_fade < 1.0
            || (self.motion_fade > 0.001 && self.motion_fade < 0.999)
            || (self.island_drop > 0.001 && self.island_drop < 0.999)
            || (nocover_island && (now - self.island_wake_ms) < 3800.0)
            || self.cover_gone_at_ms.is_some_and(|t| now - t < 240.0)
            || (self.compact_chrome_alpha > 0.001 && self.compact_chrome_alpha < 0.999)
            || (self.album_meta_alpha > 0.001 && self.album_meta_alpha < 0.999)
            || (!self.display.motion_cover && self.motion_fade > 0.001)
            || self.scene.as_ref().is_some_and(|s| s.has_motion() && !s.has_motion_frame())
            || smart_switch_pending
            || self.pending_settings_window.is_some()
            || !self.play_morph.arrived()
            || feedback_pending
            || translation_pending
            || self.panel_motion.iter().any(PanelMotion::pending)
            || panel_elastic_pending
            || self.picker_refresh_spinning
            || self.picker_refresh_angle > 0.0
            || (self.browse_state.is_none() && self.follow.pending())
            || self.group_motion.iter().any(model::RowMotion::pending)
            || self.group_bg_slide.iter().any(model::RowMotion::pending)
            || self.main_scale.iter().any(model::RowMotion::pending)
            || self.bg_scale.iter().any(model::RowMotion::pending)
            || self.bottom_motion.as_ref().is_some_and(model::RowMotion::pending)
            || self.group_bg_alpha.iter().any(|&a| a > 0.001 && a < 0.999)
            || self.group_passed_alpha.iter().any(|&a| a > 0.001 && a < 0.999)
            || (self.bottom_alpha - self.bottom_target_alpha).abs() > 0.001
            || (!self.rows.is_empty() && self.interlude_dots.pending())
            || self.scene.as_ref().is_some_and(PlayerScene::has_background_work)
            || (immersion && (self.cover.is_some() || self.scene.as_ref().is_some_and(|s|s.has_background())))
            || self.playing
            || self.scenario.is_some()
            || self.cfg.seconds.is_some()
            || self.scroll.engine().is_interacting()
            || self.scroll.engine().has_inertia()
            || self.thumb_elastic.pending()
            || self.drag_thumb.is_some()
            || (cap_hovered && self.window_controls_alpha < 0.999)
            || (!cap_hovered && self.window_controls_alpha > 0.001);
        let sidecar_open = self
            .settings_child_view
            .as_ref()
            .is_some_and(|w| w.upgrade().is_some());
        if !sidecar_open && self.settings_child.is_some() {
            self.settings_child = None;
            self.settings_child_view = None;
        }
        let cursor_can_hide = self.settings_host.is_none()
            && !sidecar_open
            && matches!(render_mode, Mode::Window | Mode::Lyrics)
            && !self.tool.panels().any_open()
            && !self.settings.open
            && !self.picker_open
            && self.pressed.is_none()
            && self.drag_panel.is_none()
            && self.drag_thumb.is_none()
            && self.switch_drag.is_none()
            && self.settings.editing_key.is_none()
            && (self.cursor.is_some() || (window.is_fullscreen() && window.is_window_active()));
        if cursor_can_hide && now - self.cursor_idle_ms >= 3000.0 && cx.is_cursor_visible() {
            window.hide_cursor_until_mouse_moves(cx);
        }
        if self.settings_host.is_none()
            && self.frames == 1
            && self.cfg.capture_rgba.is_none()
            && self.cfg.scenario.is_none()
        {
            window.request_animation_frame();
        }
        if animating && window.is_visible() {
            window.request_animation_frame();
        } else {
            if self.source == Source::Live && !self.live_poll_pending {
                self.live_poll_pending = true;
                let timer = cx.background_executor().timer(std::time::Duration::from_millis(120));
                cx.spawn(async move |view, cx| {
                    timer.await;
                    let _ = view.update(cx, |view, cx| { view.live_poll_pending = false; cx.notify(); });
                }).detach();
            }
            if cursor_can_hide && now - self.cursor_idle_ms < 3000.0 && !self.cursor_hide_pending {
                self.cursor_hide_pending = true;
                let wait_ms = ((3050.0 - (now - self.cursor_idle_ms)).clamp(50.0, 3050.0)) as u64;
                let timer = cx.background_executor().timer(std::time::Duration::from_millis(wait_ms));
                cx.spawn(async move |view, cx| {
                    timer.await;
                    let _ = view.update(cx, |view, cx| { view.cursor_hide_pending = false; cx.notify(); });
                }).detach();
            }
        }

        let fold_obstacle = compact_fold_allowed
            && !self.tool.panels().any_open()
            && !self.settings.open
            && !self.picker_open;
        let mut drag_metrics = m.clone();
        if render_mode == Mode::Window && self.lyrics_hidden() {
            drag_metrics.lyric_viewport = Box4::default();
        }
        let drag_regions = if self.settings_host.is_some() {
            Vec::new()
        } else {
            input::drag_regions(
                &drag_metrics,
                self.hot
                    .iter()
                    .map(|(_, r)| *r)
                    .chain(self.panel_bounds.iter().copied())
                    .chain(fold_obstacle.then_some(fold_box))
                    .chain(island_plan.map(|(ir, _)| ir))
                    .chain(cap_allowed.then_some([cap_min, cap_fs, cap_close]).into_iter().flatten()),
            )
        };
        let drag_view = cx.entity().downgrade();
        let input_entity = cx.entity();
        let input_focus = self.focus.clone();
        let input_active = self.settings.open
            && (self.settings.modal.is_some() || self.settings.editing_key.is_some());
        let view_w = plan.metrics.win_w;
        let view_h = plan.metrics.win_h;
        let canvas = canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _state, window, cx| {
                if input_active {
                    window.handle_input(
                        &input_focus,
                        gpui_kit::ElementInputHandler::new(bounds, input_entity.clone()),
                        cx,
                    );
                }
                paint_frame(
                    window,
                    cx,
                    &plan,
                    &more_items,
                    &candidates,
                    picked,
                    volume_value,
                );
                let view = drag_view.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble || !ev.dragging() {
                        return;
                    }
                    let x = f32::from(ev.position.x);
                    let y = f32::from(ev.position.y);
                    if x >= 0.0 && y >= 0.0 && x < view_w && y < view_h {
                        return;
                    }
                    let _ = view.update(cx, |this, cx| {
                        if !this.has_active_press() {
                            return;
                        }
                        this.cursor = Some((x, y));
                        let now = this.now_ms();
                        this.record_cursor_activity(now);
                        this.on_drag(x, y);
                        cx.notify();
                    });
                });
            },
        )
        .size_full();

        let mut root = div()
            .id("player-root")
            .track_focus(&self.focus)
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if !*hovered && !this.has_active_press() { this.cursor = None; cx.notify(); }
            }))
            .size_full()
            .relative()
            .on_scroll(cx.listener(|this, ev: &ScrollEvent, _window, cx| {
                let now = this.now_ms();
                this.record_cursor_activity(now);
                let p=ev.position;
                if this.picker_open && this.panel_bounds.iter().any(|r|r.contains(p.x.into(),p.y.into())) {
                    let delta=match ev.delta{ScrollDelta::Pixels(d)=>f32::from(d.y),ScrollDelta::Lines(d)=>d.y*VERSION_ROW*0.85};
                    let max_px=this.candidates().len().saturating_sub(this.picker_capacity) as f32 * VERSION_ROW;
                    let next=((this.picker_scroll_spring.target() as f32) - delta).clamp(0.0, max_px);
                    this.picker_scroll_spring.set_target_position(next as f64);
                    cx.notify();return;
                }
                if this.settings.open {
                    if this.settings.modal.is_some() {
                        return;
                    }
                    let delta=match ev.delta {ScrollDelta::Pixels(d)=>f32::from(d.y),ScrollDelta::Lines(d)=>d.y*48.0};
                    if let Some(panel_r) = this.panel_bounds.last().copied() {
                        if let Some(d) = this.settings.dropdown_layout(panel_r) {
                            if d.rect.contains(p.x.into(), p.y.into()) {
                                this.settings.wheel_choice_scroll(delta, d.max_scroll);
                                cx.notify();
                                return;
                            }
                        }
                    }
                    if let Some(panel_r) = this.panel_bounds.last().copied().filter(|r| r.contains(p.x.into(), p.y.into())) {
                        this.settings.close_choice();
                        let max_s = this.settings.max_scroll(panel_r);
                        this.settings.wheel_scroll(delta, max_s);
                        cx.notify();return;
                    }
                }
                let x=f32::from(p.x);let y=f32::from(p.y);
                let viewport=this.metrics(_window,cx).lyric_viewport;
                if this.tool.mode()==Some(Mode::Mini)
                    || (this.tool.mode()==Some(Mode::Window) && this.lyrics_hidden())
                    || !viewport.contains(x,y)
                    || this.panel_bounds.iter().any(|r|r.contains(x,y))
                {return;}
                if this.scroll.on_scroll(ev, now) {
                    this.browse_idle_ms = now;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    let now = this.now_ms();
                    this.record_cursor_activity(now);
                    window.focus(&this.focus, cx);
                    this.on_down(ev.position.x.into(), ev.position.y.into(), window.is_window_active());
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    let now = this.now_ms();
                    this.record_cursor_activity(now);
                    window.focus(&this.focus, cx);
                    this.on_right_down(ev.position.x.into(), ev.position.y.into());
                    cx.notify();
                }),
            )
            .on_drop::<gpui_kit::ExternalPaths>(cx.listener(|this, paths: &gpui_kit::ExternalPaths, _window, cx| {
                if let Some(path) = paths
                    .0
                    .iter()
                    .find(|p| oly_engine::custom_lyrics::is_supported_lyric_file(p))
                {
                    this.import_lyric_file(path, true);
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                let next_pos = (f32::from(ev.position.x), f32::from(ev.position.y));
                let moved = this.cursor.is_none_or(|(px, py)| {
                    (px - next_pos.0).abs() > 0.5 || (py - next_pos.1).abs() > 0.5
                });
                this.cursor = Some(next_pos);
                let now = this.now_ms();
                if moved {
                    this.record_cursor_activity(now);
                    if this.browse_state.is_some() {
                        this.browse_idle_ms = now;
                    }
                }
                if moved
                    && this.mode_transition.is_none()
                    && next_pos.1 > f32::from(_window.viewport_size().height) - 190.
                {
                    this.chrome_wake_ms = now;
                }
                if ev.dragging() {
                    this.on_drag(next_pos.0, next_pos.1);
                }
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, _window, cx| {
                    let now = this.now_ms();
                    this.record_cursor_activity(now);
                    this.on_up(ev.position.x.into(),ev.position.y.into());
                    cx.notify();
                }),
            )
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, _window, cx| {
                let now = this.now_ms();
                this.record_cursor_activity(now);
                this.on_up(ev.position.x.into(), ev.position.y.into());
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                let now = this.now_ms();
                this.record_cursor_activity(now);
                if this.settings.modal.is_some() {
                    if ev.keystroke.modifiers.control && ev.keystroke.key == "v" {
                        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                            this.settings.insert_modal_text(&text);
                        }
                    } else if ev.keystroke.modifiers.control && ev.keystroke.key == "a" {
                        this.settings.clear_modal_field();
                    } else if matches!(
                        ev.keystroke.key.as_str(),
                        "enter" | "return" | "escape" | "backspace" | "delete" | "tab"
                    ) {
                        this.settings.handle_modal_key(&ev.keystroke.key, now);
                    } else if let Some(text) = ev.keystroke.key_char.as_deref() {
                        this.settings.insert_modal_text(text);
                    } else {
                        this.settings.handle_modal_key(&ev.keystroke.key, now);
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if this.settings.editing_key.is_some() {
                    if ev.keystroke.modifiers.control && ev.keystroke.key == "v" {
                        if let Some(text)=cx.read_from_clipboard().and_then(|c|c.text()){this.settings.insert_text(&text);}
                    } else if ev.keystroke.modifiers.control && ev.keystroke.key == "a" {this.settings.editing_draft.clear();}
                    else if let Some(text)=ev.keystroke.key_char.as_deref(){this.settings.insert_text(text);}
                    else {this.settings.edit_key(&ev.keystroke.key,this.now_ms());}
                    cx.stop_propagation();
                } else {this.on_key(&ev.keystroke.key);}
                cx.notify();
            }))
            .child(canvas)
            .children(drag_regions.into_iter().map(|r|div().absolute().left(px(r.x)).top(px(r.y)).w(px(r.w)).h(px(r.h)).window_control_area(WindowControlArea::Drag)))
            .children(fold_obstacle.then(|| {
                div()
                    .id("fold-hit")
                    .absolute()
                    .left(px(fold_box.x))
                    .top(px(fold_box.y))
                    .w(px(fold_box.w))
                    .h(px(fold_box.h))
                    .cursor_pointer()
            }));

        // Leaving the window drops hover feedback.
        root = root.on_mouse_down_out(cx.listener(|this, _ev, _window, cx| {
            this.cursor = None;
            this.cancel_press();
            cx.notify();
        }));
        root.into_any_element()
    }
}

impl gpui_kit::EntityInputHandler for PlayerView {
    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.settings.open
            && (self.settings.modal.is_some() || self.settings.editing_key.is_some())
    }

    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _adjusted_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui_kit::UTF16Selection> {
        Some(gpui_kit::UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.modal.is_some() {
            if self.settings.insert_modal_text(text) {
                cx.notify();
            }
        } else if self.settings.editing_key.is_some() {
            self.settings.insert_text(text);
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: gpui_kit::Bounds<gpui_kit::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui_kit::Bounds<gpui_kit::Pixels>> {
        if let Some(modal) = &self.settings.modal {
            let panel_r = self
                .panel_bounds
                .last()
                .copied()
                .unwrap_or(Box4::new(0.0, 0.0, 540.0, 440.0));
            let mp = self.panel_elastics[4].pull();
            let (box_r, f1, f2, _, _) = Settings::modal_rects_with_offset(panel_r, modal, mp);
            let r = match modal {
                settings::SettingsModal::CustomModel { .. } => f1,
                settings::SettingsModal::ArtistAlias { focus, .. } => {
                    if *focus == 1 {
                        f2.unwrap_or(f1)
                    } else {
                        f1
                    }
                }
                settings::SettingsModal::PrivacyPolicy => box_r,
            };
            return Some(gpui_kit::Bounds {
                origin: point(px(r.x + 12.0), px(r.y + 4.0)),
                size: gpui_kit::size(px(2.0), px(r.h - 8.0)),
            });
        }
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui_kit::Point<gpui_kit::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
