use super::*;

/// One fixture file: the envelope keeps the document under `doc`.
pub(super) fn load_fixture(path: &std::path::Path) -> anyhow::Result<(LyricDoc,String)> {
    let value:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let doc=value.get("doc").unwrap_or(&value);
    let provider=doc.get("provider").and_then(|p|p.as_str()).unwrap_or("fixture").to_string();
    Ok((serde_json::from_value(doc.clone())?,provider))
}

pub(super) fn paint_frame(
    window: &mut Window,
    cx: &mut App,
    plan: &Plan,
    more_items: &[&str],
    candidates: &[PickerEntry],
    _picked: usize,
    volume_value: f64,
) {
    let light_window = plan.display.theme_mode == model::ThemeMode::Light && !plan.immersion && plan.mode == Mode::Window;
    let light_panel = plan.display.theme_mode == model::ThemeMode::Light;
    chrome::set_light_theme(if plan.settings_only { light_panel } else { light_window });
    let m = &plan.metrics;
    let feedback = |key: Hot| plan.feedback.iter().find(|(h,_)| *h == key).map(|(_,f)| *f).unwrap_or_default();
    let win_radius = if plan.fullscreen { 0.0 } else if !plan.immersion {8.0} else { plan.display.corner_radius };
    let panel_radius = if plan.settings_only{8.0}else{plan.display.corner_radius.clamp(8.0, 28.0)};

    if !plan.settings_only {
    // 1. Immersion background, full bleed, behind everything.
    if let Some(bg) = plan.bg {
        window.paint_custom_surface_rounded(
            gpui_kit::Bounds {
                origin: point(px(0.0), px(0.0)),
                size: gpui_kit::size(px(m.win_w), px(m.win_h)),
            },
            px(win_radius),
            bg,
        );
    } else {
        let bg_color = match plan.display.theme_mode {
            model::ThemeMode::Light if !plan.immersion && plan.mode == Mode::Window => {
                Hsla::from(rgb(0xFA_FC_FF)).opacity(0.90)
            }
            model::ThemeMode::Dark if !plan.immersion => {
                Hsla::from(rgb(0x14_16_1A)).opacity(0.84)
            }
            _ => Hsla::from(rgb(0x18_19_1C)).opacity(if plan.immersion { 1.0 } else { 0.29 }),
        };
        chrome::fill_round(
            window,
            0.0,
            0.0,
            m.win_w,
            m.win_h,
            win_radius,
            bg_color,
        );
    }

    let fg_alpha = plan.content_alpha.clamp(0.0, 1.0);
    let cg_alpha = plan.chrome_alpha.clamp(0.0, 1.0);
    if fg_alpha > 0.005 {
    // 2. Cover: motion first (it carries its own colour), else the still, else
    //    the reference's `.cover-blank` placeholder.
    let cover_vis = if plan.mode == Mode::Window { plan.cover_vis } else { 1.0 };
    let cover_alpha = (fg_alpha * cover_vis).clamp(0.0, 1.0);
    if m.art_height > 0.0 && cover_alpha > 0.01 {
        let cover = if plan.mode == Mode::Window && (plan.cover_scale - 1.0).abs() > 0.0005 {
            let s = plan.cover_scale.clamp(0.5, 1.15);
            let (cx_c, cy_c) = m.cover.centre();
            let cw = m.cover.w * s;
            let ch = m.cover.h * s;
            Box4::new(cx_c - cw * 0.5, cy_c - ch * 0.5, cw, ch)
        } else {
            m.cover
        };
        let scale_r = if m.cover.w > 1.0 { cover.w / m.cover.w } else { 1.0 };
        let radius = if plan.mode == Mode::Album {
            win_radius
        } else if matches!(plan.mode, Mode::Mini | Mode::Lyrics) {
            crate::player::details::compact_cover_radius(plan.display.cover_radius)
        } else {
            (plan.display.cover_radius * scale_r * m.ui_scale).min(cover.w * 0.5)
        };
        let shadows: &[BoxShadow] = if plan.mode == Mode::Album {
            &[]
        } else if matches!(plan.mode, Mode::Mini | Mode::Lyrics) {
            &[BoxShadow::new(
                px(0.0),
                px(cover.w * 0.02),
                Hsla::from(rgb(0x000000)).opacity(0.24 * cover_alpha),
            )
            .blur_radius(px(cover.w * 0.035))]
        } else {
            &[
                BoxShadow::new(
                    px(0.0),
                    px(cover.w * 0.0367),
                    Hsla::from(rgb(0x000000)).opacity(0.52 * cover_alpha),
                )
                .blur_radius(px(cover.w * 0.05)),
                BoxShadow::new(
                    px(0.0),
                    px(cover.w * 0.0133),
                    Hsla::from(rgb(0x000000)).opacity(0.42 * cover_alpha),
                )
                .blur_radius(px(cover.w * 0.02165)),
            ]
        };
        window.paint_drop_shadows(
            gpui_kit::Bounds {
                origin: point(px(cover.x), px(cover.y)),
                size: gpui_kit::size(px(cover.w), px(cover.h)),
            },
            Corners::all(px(radius)),
            shadows,
        );
        let still_needed = plan.cover.is_some()
            && (plan.motion.is_none() || plan.motion_alpha < 0.999);
        if still_needed {
            if let Some(still) = plan.cover {
                let b = gpui_kit::Bounds {
                    origin: point(px(cover.x), px(cover.y)),
                    size: gpui_kit::size(px(cover.w), px(cover.h)),
                };
                if let Some(prev) = plan.prev_cover.filter(|_| plan.cover_fade < 0.999) {
                    if cover_alpha < 0.999 {
                        window.paint_custom_surface_rounded_opacity(b, px(radius), prev, cover_alpha);
                    } else {
                        window.paint_custom_surface_rounded(b, px(radius), prev);
                    }
                    let fade = oly_ui::color::smoothstep01(plan.cover_fade);
                    let top_alpha = (fade * cover_alpha).clamp(0.0, 1.0);
                    if top_alpha > 0.002 {
                        window.paint_custom_surface_rounded_opacity(b, px(radius), still, top_alpha);
                    }
                } else if cover_alpha < 0.999 {
                    window.paint_custom_surface_rounded_opacity(b, px(radius), still, cover_alpha);
                } else {
                    window.paint_custom_surface_rounded(b, px(radius), still);
                }
            }
        }
        if let Some(motion) = plan.motion.filter(|_| plan.motion_alpha > 0.001) {
            window.paint_custom_surface_rounded_opacity(
                gpui_kit::Bounds {
                    origin: point(px(cover.x), px(cover.y)),
                    size: gpui_kit::size(px(cover.w), px(cover.h)),
                },
                px(radius),
                motion,
                (plan.motion_alpha * cover_alpha).clamp(0.0, 1.0),
            );
        } else if plan.cover.is_none() && cover_alpha > 0.01 {
            chrome::cover_blank(window, cx, cover, radius, cover_alpha);
        }
        if fg_alpha < 0.995 && cover_vis > 0.99 {
            chrome::fill_round(
                window,
                cover.x,
                cover.y,
                cover.w,
                cover.h,
                radius,
                Hsla::from(rgb(0x18_19_1C)).opacity(((1.0 - fg_alpha) * 0.9).clamp(0.0, 1.0)),
            );
        }
        if plan.mode == Mode::Mini {
            let h = feedback(Hot::AlbumEnter).hover.clamp(0.0, 1.0);
            if h > 0.01 {
                chrome::fill_round(
                    window,
                    cover.x,
                    cover.y,
                    cover.w,
                    cover.h,
                    radius,
                    Hsla::from(rgb(0x00_00_00)).opacity(0.42 * h * fg_alpha),
                );
                let (cx_c, cy_c) = cover.centre();
                icons::paint(
                    window,
                    cx,
                    Icon::CoverExpand,
                    Box4::new(cx_c - 10.0, cy_c - 10.0, 20.0, 20.0),
                    chrome::text(0.92 * h * fg_alpha),
                );
            }
        }
        chrome::stroke_round(
            window,
            cover.x,
            cover.y,
            cover.w,
            cover.h,
            radius,
            0.85,
            chrome::text(0.14 * cover_alpha),
        );
    }

    // 3. Lyrics, clipped to the lyric column so a row never spills over the
    //    cover or the chrome.
    let lyric_bottom=m.lyrics.bottom()-if plan.mode==Mode::Lyrics && plan.chrome_visible{164.}else{0.};
    let feather_top = m.lyrics.y + if m.feather_px > 4.0 { 4.0 } else { 0.0 };
    let glow_pad = (m.font * 0.45).min(18.0);
    let mask_left = (m.lyric_viewport.x - glow_pad).max(0.0);
    let mask_right = (m.lyric_viewport.right() + glow_pad).min(m.win_w);
    let mask = ContentMask {
        bounds: gpui_kit::Bounds {
            origin: point(px(mask_left), px(m.lyric_viewport.y)),
            size: gpui_kit::size(px((mask_right - mask_left).max(1.0)), px((m.lyric_viewport.h-if plan.mode==Mode::Lyrics && plan.chrome_visible{164.}else{0.}).max(1.))),
        },
    };
    if plan.lyrics_alpha > 0.005 {
    window.with_content_mask(Some(mask), |window| {
        if let Some(notice) = &plan.empty_notice {
            chrome::paint_text_box_centred(
                window,
                notice,
                m.lyrics,
                15.0,
                500.0,
                chrome::text(0.45 * plan.lyrics_alpha * fg_alpha),
            );
        }
        if let Some((r, alpha)) = plan.lyric_hover {
            chrome::fill_round(window, r.x, r.y, r.w, r.h, m.font * plan.display.line_radius, chrome::text(alpha));
        }
        for row in &plan.rows {
            paint::paint_row(
                window,
                &row.lines,
                row.bounds.x,
                row.top,
                row.lines.line_height,
                row.lines.font_size,
                &row.lines.words,
                &row.light,
                plan.lyric_position_ms,
                row.scale,
                row.wrapper,
                feather_top,
                lyric_bottom,
                m.feather_px,
                row.bounds.w,
                row.align,
                row.blur,
            );
            if plan.translation {
                let top = row.top
                    + row.lines.lines.len() as f32 * row.lines.line_height
                    + row.lines.trans_gap;
                for (i, trans) in row.lines.trans_lines.iter().enumerate() {
                    let x = row.bounds.x + row.align.offset(row.bounds.w, f32::from(trans.width));
                    paint::paint_translation(
                        window,
                        trans,
                        x,
                        top + i as f32 * row.lines.trans_line_height,
                        row.lines.trans_font,
                        row.translation_alpha,
                        [feather_top, lyric_bottom, m.feather_px],
                        row.scale,
                        row.align.pivot_x(row.bounds.x, row.bounds.w),
                        row.top + row.lines.height * 0.5,
                        row.wrapper,
                        row.blur,
                    );
                }
            }
            let top = row.top
                + row.lines.lines.len() as f32 * row.lines.line_height
                + if row.lines.trans_lines.is_empty() {
                    0.0
                } else {
                    row.lines.trans_gap + row.lines.trans_height()
                };
            for (i, roman) in row.lines.roman_lines.iter().enumerate() {
                let x = row.bounds.x + row.align.offset(row.bounds.w, f32::from(roman.width));
                paint::paint_translation(
                    window,
                    roman,
                    x,
                    top + i as f32 * row.lines.roman_line_height,
                    row.lines.roman_font,
                    row.roman_alpha,
                    [feather_top, lyric_bottom, m.feather_px],
                    row.scale,
                    row.align.pivot_x(row.bounds.x, row.bounds.w),
                    row.top + row.lines.height * 0.5,
                    row.wrapper,
                    row.blur,
                );
            }
        }
        if let Some(inter) = &plan.interlude {
            let d = inter.dot_diameter * inter.scale;
            let g = inter.dot_gap * inter.scale;
            let strip_w = inter.dot_diameter * 3.0 + inter.dot_gap * 2.0;
            let cx_i = inter.x + strip_w * 0.5;
            let cy_i = inter.y + inter.dot_diameter * 0.5;
            let feather = paint::feather(cy_i, feather_top, lyric_bottom, m.feather_px);
            paint::paint_interlude_strip(
                window,
                cx_i,
                cy_i,
                d,
                g,
                inter.dot_opacities,
                inter.opacity * feather,
                false,
                None,
                None,
            );
        }
        if let Some(bottom) = &plan.bottom {
            for (i, line) in bottom.lines.lines.iter().enumerate() {
                let x = bottom.x_base + bottom.align.offset(bottom.wrap_width, f32::from(line.width));
                paint::paint_translation(
                    window,
                    line,
                    x,
                    bottom.top + i as f32 * bottom.lines.line_height,
                    bottom.lines.font_size,
                    bottom.alpha,
                    [feather_top, lyric_bottom, m.feather_px],
                    1.0,
                    bottom.align.pivot_x(bottom.x_base, bottom.wrap_width),
                    bottom.top + bottom.lines.height * 0.5,
                    paint::WrapperTransform::default(),
                    bottom.blur,
                );
            }
        }
    });
    } // end lyrics_alpha > 0.005

    // 4. Cover caption, under the artwork.
    //
    //    `.cover` is a 20px-gap flex column and `.cover-meta` is shifted down
    //    another `--wide-controls-drop` (12px), so the title's line box starts
    //    32px under the artwork. Both lines are 22px at `line-height: 1.25`,
    //    the artist 4px under the title and in plain weight. `Cover` returns
    //    `null` only when both `showCover` and `showMeta` are off, so the
    //    caption still paints with the artwork hidden; `metrics` carries the
    //    right top edge for both cases.
        let album_scrim_alpha = (plan.compact_chrome_alpha.max(plan.album_meta_alpha) * cg_alpha).clamp(0.0, 1.0);
        if plan.mode == Mode::Album && album_scrim_alpha > 0.005 {
            let scrim_h = 196.0_f32.min(m.win_h);
            let scrim_top = m.win_h - scrim_h;
            let bottom_r = win_radius.min(scrim_h * 0.5);
            window.paint_quad(gpui_kit::quad(
                gpui_kit::bounds(
                    gpui_kit::point(px(0.0), px(scrim_top)),
                    gpui_kit::size(px(m.win_w), px(scrim_h)),
                ),
                gpui_kit::Corners {
                    top_left: px(0.0),
                    top_right: px(0.0),
                    bottom_left: px(bottom_r),
                    bottom_right: px(bottom_r),
                },
                gpui_kit::linear_gradient(
                    180.0,
                    gpui_kit::linear_color_stop(Hsla::from(rgb(0x00_00_00)).opacity(0.0), 0.0),
                    gpui_kit::linear_color_stop(
                        Hsla::from(rgb(0x00_00_00)).opacity((0.78 * album_scrim_alpha).clamp(0.0, 0.85)),
                        1.0,
                    ),
                ),
                gpui_kit::Edges::default(),
                Hsla::default(),
                gpui_kit::BorderStyle::Solid,
            ));
        }
        let album_meta_a = (plan.album_meta_alpha * cg_alpha).clamp(0.0, 1.0);
        if plan.mode == Mode::Album && m.column > 0.0 && album_meta_a > 0.005 {
            let max_w = (m.win_w - 32.0).max(1.0);
            let title_w = chrome::measure(window, &plan.track_title, 18.0, 650.0).0.min(max_w);
            let title_x = (m.win_w - title_w) * 0.5;
            chrome::paint_text_fitted(window, &plan.track_title, title_x, m.cover_meta_top, max_w, 18.0, 650., chrome::text(0.96 * album_meta_a));
            let artist_w = chrome::measure(window, &plan.track_artist, 13.0, 500.0).0.min(max_w);
            let artist_x = (m.win_w - artist_w) * 0.5;
            chrome::paint_text_fitted(window, &plan.track_artist, artist_x, m.cover_meta_top + 24.0, max_w, 13.0, 500., chrome::text(0.66 * album_meta_a));
        } else if plan.mode != Mode::Album && m.column > 0.0 && cover_alpha > 0.01 && !(plan.mode == Mode::Window && !plan.display.show_cover && cover_vis <= 0.01) {
        let compact=matches!(plan.mode,Mode::Mini|Mode::Album|Mode::Lyrics);
        let ws = if plan.mode == Mode::Window { m.ui_scale } else { 1.0 };
        let x=if compact{m.cover.x+m.art_height+if m.art_height>0.{13.}else{0.}}else{m.cover.x};
        let title_size=if compact{19.}else{22. * ws};let artist_size=if compact{13.5}else{17. * ws};
        let width=if compact{m.win_w-x-16.}else{m.column};
        let title_y = if compact { m.cover_meta_top } else { m.cover_meta_top - 3.0 * ws };
        let artist_y = if compact { m.cover_meta_top + 28.0 } else { m.cover_meta_top + 25.0 * ws };
        let meta_a = if plan.mode == Mode::Window { cover_alpha } else { fg_alpha };
        if plan.wallpaper_mode && plan.mode == Mode::Window {
            let center_x = m.cover.x + m.cover.w * 0.5;
            let max_w = (m.cover.w * 1.15).max(width);
            let title_w = chrome::measure(window, &plan.track_title, title_size, 650.0).0.min(max_w);
            let artist_w = chrome::measure(window, &plan.track_artist, artist_size, 420.0).0.min(max_w);
            chrome::paint_text_fitted(window, &plan.track_title, center_x - title_w * 0.5, title_y, max_w, title_size, 650., chrome::text(0.96 * meta_a));
            chrome::paint_text_fitted(window, &plan.track_artist, center_x - artist_w * 0.5, artist_y, max_w, artist_size, 420., chrome::text(0.66 * meta_a));
        } else {
            chrome::paint_text_fitted(window,&plan.track_title,x,title_y,width,title_size,650.,chrome::text(0.96 * meta_a));
            chrome::paint_text_fitted(window,&plan.track_artist,x,artist_y,width,artist_size,if compact{500.}else{420.},chrome::text((if compact{0.60}else{0.66}) * meta_a));
        }
    }

    // 5. Playbar: one 6px rail across the column and the two time labels under
    //    it.
    //
    //    `.window-progress .track .lg-slider` widens the slider box to
    //    `calc(100% + 26px)` with `margin-inline: -13px`, but
    //    `.lg-slider-track` then sets `inset-inline: 13px`: the two cancel, and
    //    the visible rail is exactly the column. The overhang only exists to
    //    give the hit target room, which is why `Hot::Progress` is the wider
    //    box while the paint is not. The tail rule
    //    `.window-progress .time.now, .time.total { margin-inline: 0 }` also
    //    clears the base 13px label margins, so both labels sit flush with the
    //    column ends. The window form shows the time remaining on the right,
    //    with the reference's leading minus.
    if let Some((ir, ia)) = plan.island {
        chrome::set_light_theme(light_panel);
        let ia = (ia * cg_alpha).clamp(0.0, 1.0);
        if ia > 0.005 {
            if let Some(gp) = &plan.island_glass_panel {
                window.paint_custom_surface_with_scene(
                    gpui_kit::Bounds {
                        origin: point(px(gp.rect.left as f32), px(gp.rect.top as f32)),
                        size: gpui_kit::size(px(gp.rect.width as f32), px(gp.rect.height as f32)),
                    },
                    gp.surface,
                    gp.snapshot,
                    gp.hook,
                );
            } else {
                chrome::fill_round(
                    window,
                    ir.x,
                    ir.y,
                    ir.w,
                    ir.h,
                    18.0,
                    chrome::themed_surface(0.86 * ia),
                );
            }
            chrome::stroke_round(
                window,
                ir.x,
                ir.y,
                ir.w,
                ir.h,
                18.0,
                1.0,
                chrome::text(0.16 * ia),
            );
            let has_thumb = plan.cover.is_some();
            if let Some(still) = plan.cover {
                let thumb = gpui_kit::Bounds {
                    origin: point(px(ir.x + 10.0), px(ir.y + 8.0)),
                    size: gpui_kit::size(px(36.0), px(36.0)),
                };
                window.paint_custom_surface_rounded(thumb, px(8.0), still);
                chrome::stroke_round(window, ir.x + 10.0, ir.y + 8.0, 36.0, 36.0, 8.0, 0.75, chrome::text(0.16 * ia));
            }
            let text_x = if has_thumb { ir.x + 54.0 } else { ir.x + 16.0 };
            let meta_w = (ir.right() - 182.0 - text_x).max(60.0);
            chrome::paint_text_fitted(
                window,
                &plan.track_title,
                text_x,
                ir.y + 8.0,
                meta_w,
                13.0,
                600.0,
                chrome::text(0.95 * ia),
            );
            chrome::paint_text_fitted(
                window,
                &plan.track_artist,
                text_x,
                ir.y + 25.0,
                meta_w,
                11.0,
                450.0,
                chrome::text(0.62 * ia),
            );
        }
        chrome::set_light_theme(light_window);
    }
    let compact_cg_alpha = if matches!(plan.mode, Mode::Lyrics | Mode::Album) {
        (cg_alpha * plan.compact_chrome_alpha).clamp(0.0, 1.0)
    } else {
        cg_alpha
    };
    let show_bottom_progress = plan.display.show_progress
        && plan.chrome_visible
        && compact_cg_alpha > 0.005
        && (plan.mode != Mode::Window || cover_vis > 0.01 || plan.island.is_some());
    if show_bottom_progress {
    let progress = plan.chrome.progress;
    let fraction = if plan.duration_ms > 0.0 {
        (plan.position_ms / plan.duration_ms).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let bar_alpha = if let Some((_, ia)) = plan.island {
        (ia * cg_alpha).clamp(0.0, 1.0)
    } else if plan.mode == Mode::Window {
        cover_alpha
    } else {
        compact_cg_alpha
    };
    chrome::progress_rail_interactive_alpha(window, progress, fraction, feedback(Hot::Progress), bar_alpha);
    if plan.island.is_none() {
    // `.window-progress` is a 12px/4px/20px grid: the label row starts 16px
    // below the block, and a 13px line lands its ink 5px under that. The
    // measured reference ink is rows 633..642 at a block top of 611.
    let compact=matches!(plan.mode,Mode::Mini|Mode::Album|Mode::Lyrics);
    let ws = if plan.mode == Mode::Window { m.ui_scale } else { 1.0 };
    let time_size = if plan.mode == Mode::Album { 12.0 } else if compact { 13.0 } else { 14.0 * ws };
    let time_top = if plan.mode == Mode::Album { progress.y + 15.0 } else if compact { progress.y - 2.0 } else { progress.y + 9.0 + 7.0 * ws };
    let time_alpha = if plan.mode == Mode::Album { 0.68 } else { 0.54 } * bar_alpha;
    let elapsed = chrome::clock_text(plan.position_ms);
    let (elapsed_w, _) = chrome::measure(window, &elapsed, time_size, 400.0);
    chrome::paint_text(
        window,
        &elapsed,
        if compact{m.progress.x}else{progress.x},
        time_top,
        time_size,
        400.0,
        chrome::text(time_alpha),
    );
    let remaining = format!(
        "-{}",
        chrome::clock_text((plan.duration_ms - plan.position_ms).max(0.0))
    );
    let (remaining_w, _) = chrome::measure(window, &remaining, time_size, 400.0);
    let avail_w = if compact { m.progress.w } else { progress.w };
    if avail_w >= elapsed_w + remaining_w + 12.0 {
        chrome::paint_text(
            window,
            &remaining,
            if compact{m.progress.right()-remaining_w}else{progress.x+progress.w-remaining_w},
            time_top,
            time_size,
            400.0,
            chrome::text(time_alpha),
        );
    }
    }

    }

    // The scrollbar stays a thin hairline at rest and thickens into a liquid capsule while held.
    if cg_alpha > 0.005 {
        if let Some(panel)=&plan.scroll_glass {
            window.paint_custom_surface_with_scene(gpui_kit::Bounds{origin:point(px(panel.rect.left as f32),px(panel.rect.top as f32)),size:gpui_kit::size(px(panel.rect.width as f32),px(panel.rect.height as f32))},panel.surface,panel.snapshot,panel.hook);
        }
        if let Some((line, alpha)) = plan.scroll_line {
            chrome::fill_round(
                window,
                line.x,
                line.y,
                line.w,
                line.h,
                line.w * 0.5,
                chrome::text(alpha * cg_alpha),
            );
        }
    }

    // 6. Transport: clean icons at rest, subtle hover hop without a circle,
    //    lively toggle bounce on state change, and a clear refractive liquid lens while held/dragged.
    let paint_btn_lens_alpha = |window: &mut Window, target: Hot, alpha: f32| {
        if let Some((hot, panel, lens_box, bf)) = &plan.button_lens {
            if *hot == target {
                window.paint_custom_surface_with_scene(
                    gpui_kit::Bounds {
                        origin: point(px(panel.rect.left as f32), px(panel.rect.top as f32)),
                        size: gpui_kit::size(px(panel.rect.width as f32), px(panel.rect.height as f32)),
                    },
                    panel.surface,
                    panel.snapshot,
                    panel.hook,
                );
                let radius = lens_box.w.min(lens_box.h) * 0.5;
                let swell = if target == Hot::WindowClose {
                    (bf.hover * 0.45 + bf.press * 0.48).clamp(0.0, 1.0)
                } else {
                    (bf.press * 0.85).clamp(0.0, 1.0)
                };
                chrome::paint_segmented_lens(window, *lens_box, radius, swell, *bf, alpha);
            }
        }
    };
    let btn_alpha = if plan.mode == Mode::Window {
        if let Some((_, ia)) = plan.island {
            (ia * cg_alpha).clamp(0.0, 1.0)
        } else {
            cover_alpha
        }
    } else {
        compact_cg_alpha
    };
    if plan.chrome_visible && btn_alpha > 0.005 {
    let play = plan.chrome.play;
    let (pcx, pcy) = play.centre();
    let play_feedback = feedback(Hot::Play);
    paint_btn_lens_alpha(window, Hot::Play, btn_alpha);
    icons::paint_toggle(
        window,
        cx,
        Icon::Play,
        Icon::Pause,
        {let d=if plan.island.is_some(){22.}else{match plan.mode{Mode::Mini=>28.,Mode::Album=>32.,Mode::Lyrics=>34.,_=>36. * plan.metrics.ui_scale}};Box4::new(pcx-d/2.,pcy-d/2.,d,d)},
        chrome::text(0.98 * btn_alpha),
        play_feedback,
        plan.play_morph,
    );

    let prev = plan.chrome.prev;
    let prev_feedback = feedback(Hot::Prev);
    paint_btn_lens_alpha(window, Hot::Prev, btn_alpha);
    let (x, y) = prev.centre();
    icons::paint_elastic(
        window,
        cx,
        Icon::Prev,
        {let d=if plan.island.is_some(){18.}else{match plan.mode{Mode::Mini=>22.,Mode::Album=>24.,Mode::Lyrics=>26.,_=>28. * plan.metrics.ui_scale}};Box4::new(x-d/2.,y-d/2.,d,d)},
        chrome::text(0.90 * btn_alpha),
        prev_feedback,
    );

    if plan.control_pending {let p=plan.chrome.play;chrome::stroke_round(window,p.x+1.0,p.y+1.0,p.w-2.0,p.h-2.0,(p.w-2.0)*0.5,1.0,chrome::text(0.35 * btn_alpha));}
    let next = plan.chrome.next;
    let next_feedback = feedback(Hot::Next);
    paint_btn_lens_alpha(window, Hot::Next, btn_alpha);
    let (x, y) = next.centre();
    icons::paint_elastic(
        window,
        cx,
        Icon::Next,
        {let d=if plan.island.is_some(){18.}else{match plan.mode{Mode::Mini=>22.,Mode::Album=>24.,Mode::Lyrics=>26.,_=>28. * plan.metrics.ui_scale}};Box4::new(x-d/2.,y-d/2.,d,d)},
        chrome::text(0.90 * btn_alpha),
        next_feedback,
    );

    let more = plan.chrome.more;
    let aux_d = if plan.island.is_some() { 18.0 } else if plan.mode == Mode::Mini { 21.0 } else if plan.mode == Mode::Window { 24.0 * plan.metrics.ui_scale } else { 24.0 };
    if more.w > 0.0 && more.h > 0.0 {
        let (mx, my) = more.centre();
        let more_feedback = feedback(Hot::More);
        paint_btn_lens_alpha(window, Hot::More, btn_alpha);
        icons::paint_elastic(
            window,
            cx,
            Icon::More,
            Box4::new(mx - aux_d * 0.5, my - aux_d * 0.5, aux_d, aux_d),
            chrome::text((0.68 + 0.27 * more_feedback.hover) * btn_alpha),
            more_feedback,
        );
    }

    if plan.mode!=Mode::Mini {
    let volume = plan.chrome.volume;
    let (vx, vy) = volume.centre();
    let volume_feedback = feedback(Hot::Volume);
    paint_btn_lens_alpha(window, Hot::Volume, btn_alpha);
    icons::paint_elastic(
        window,
        cx,
        Icon::Volume,
        Box4::new(vx - aux_d * 0.5, vy - aux_d * 0.5, aux_d, aux_d),
        chrome::text((0.68 + 0.27 * volume_feedback.hover) * (if plan.volume_available { 1.0 } else { 0.45 }) * btn_alpha),
        volume_feedback,
    );

    let lyrics_btn = plan.chrome.lyrics_toggle;
    if lyrics_btn.w > 0.0 && lyrics_btn.h > 0.0 {
        let (lx, ly) = lyrics_btn.centre();
        let lf = feedback(Hot::LyricsToggle);
        paint_btn_lens_alpha(window, Hot::LyricsToggle, btn_alpha);
        let icon_color = if plan.lyrics_visible {
            chrome::text((0.95 + 0.05 * lf.hover) * btn_alpha)
        } else {
            chrome::text((0.38 + 0.22 * lf.hover) * btn_alpha)
        };
        icons::paint_elastic(
            window,
            cx,
            Icon::Lyrics,
            Box4::new(lx - aux_d * 0.5, ly - aux_d * 0.5, aux_d, aux_d),
            icon_color,
            lf,
        );
    }

    // 7. Replay badge: the experiment never pretends the buttons reach a real
    //    player.
    }
    }
    let fold_fb = feedback(Hot::Fold);
    let fold_shown = !plan.settings_only
        && cg_alpha > 0.005
        && matches!(plan.mode, Mode::Mini | Mode::Lyrics)
        && (crate::player::details::fold_visible(plan.cursor, m.win_w, m.win_h)
            || fold_fb.hover > 0.01
            || fold_fb.press > 0.01);
    if fold_shown {
        let swell = fold_fb.hover.clamp(0.0, 1.0);
        let press = fold_fb.press.clamp(0.0, 1.0);
        if swell > 0.01 || press > 0.01 {
            chrome::fill_round_subpixel(
                window,
                m.win_w * 0.5 - 36.0,
                m.win_h - 14.0,
                72.0,
                13.0,
                6.5,
                chrome::text(((0.08 * swell + 0.06 * press) * cg_alpha).clamp(0.0, 0.2)),
            );
        }
        let lw = ((32.0 + 12.0 * swell - 3.0 * press) * (1.0 + fold_fb.squeeze[0] * 0.35)).max(24.0);
        let lh = ((3.0 + 2.0 * swell - 0.3 * press) * (1.0 + fold_fb.squeeze[1] * 0.35)).max(2.5);
        let lx = m.win_w * 0.5 - lw * 0.5 + fold_fb.pull[0] * 0.35;
        let ly = m.win_h - 7.5 - lh * 0.5 + fold_fb.pull[1] * 0.18;
        chrome::fill_round_subpixel(
            window,
            lx,
            ly,
            lw,
            lh,
            lh * 0.5,
            chrome::text(((0.52 + 0.40 * swell + 0.06 * press) * cg_alpha).clamp(0.0, 0.98)),
        );
    }
    let compact = matches!(plan.mode, Mode::Mini | Mode::Album | Mode::Lyrics);
    if !plan.settings_only && plan.window_controls_alpha > 0.01 {
        let wca = (plan.window_controls_alpha * fg_alpha).clamp(0.0, 1.0);
        let (_, [cap_pin, cap_min, cap_fs, cap_close]) = window_caption_rects(m.win_w, compact);
        let icon_d = if compact { 11.5 } else { 14.0 };
        let mut items = vec![
            (
                Hot::WindowPin,
                cap_pin,
                if plan.always_on_top {
                    Icon::PinFilled
                } else {
                    Icon::Pin
                },
            ),
            (Hot::WindowMinimize, cap_min, Icon::Minimize),
            (
                Hot::WindowFullscreen,
                cap_fs,
                if plan.fullscreen {
                    Icon::FullscreenExit
                } else {
                    Icon::Fullscreen
                },
            ),
            (Hot::WindowClose, cap_close, Icon::Close),
        ];
        if plan.mode == Mode::Album {
            items.push((Hot::AlbumReturn, album_return_rect(), Icon::CoverCollapse));
        }
        for (hot_key, box_r, icon) in items {
            let fb = feedback(hot_key);
            paint_btn_lens_alpha(window, hot_key, wca);
            let (bcx, bcy) = box_r.centre();
            let color = if hot_key == Hot::WindowPin && plan.always_on_top {
                chrome::accent_rgb(plan.display.accent_hex, (0.94 + 0.06 * fb.hover) * wca)
            } else {
                chrome::text((0.78 + 0.22 * fb.hover) * wca)
            };
            icons::paint_elastic(
                window,
                cx,
                icon,
                Box4::new(bcx - icon_d * 0.5, bcy - icon_d * 0.5, icon_d, icon_d),
                color,
                fb,
            );
        }
    }
    if let Some(badge) = plan.badge.as_ref() {
        let (w, _) = chrome::measure(window, badge, 11.0, 600.0);
        let shift = if matches!(plan.mode, Mode::Mini | Mode::Lyrics) {
            110.0 * plan.window_controls_alpha
        } else {
            144.0 * plan.window_controls_alpha
        };
        let bx = m.win_w - w - 24.0 - shift;
        chrome::fill_round(window, bx - 8.0, 14.0, w + 16.0, 20.0, 6.0, chrome::text(0.10 * fg_alpha));
        chrome::paint_text(window, badge, bx, 18.0, 11.0, 600.0, chrome::accent_rgb(plan.display.accent_hex, 0.9 * fg_alpha));
    }

    } // end fg_alpha > 0.005
    }
    // 8. Panels last: each one samples the frame behind it in the same pass.
    chrome::set_light_theme(light_panel);
    for panel in &plan.panels {
        let bounds = gpui_kit::Bounds {
            origin: point(px(panel.rect.left as f32), px(panel.rect.top as f32)),
            size: gpui_kit::size(px(panel.rect.width as f32), px(panel.rect.height as f32)),
        };
        if !plan.settings_only && plan.immersion {chrome::elevation(window, Box4::new(panel.rect.left as f32, panel.rect.top as f32, panel.rect.width as f32, panel.rect.height as f32), panel_radius, panel.surface_alpha);}
        // Sidecar windows rely on DWM directly; main-window popovers run the
        // alpha-preserving glass shader so desktop areas stay translucent while
        // underlying cover/lyrics still receive frosted Gaussian blur.
        if plan.settings_only && matches!(panel.kind, PanelKind::Settings | PanelKind::Dropdown) {
            let r = Box4::new(
                panel.rect.left as f32,
                panel.rect.top as f32,
                panel.rect.width as f32,
                panel.rect.height as f32,
            );
            paint_sidecar_glass_frame(
                window,
                r,
                panel_radius,
                plan.display.theme_mode,
                panel.surface_alpha,
            );
        } else {
            window.paint_custom_surface_with_scene(bounds, panel.surface, panel.snapshot, panel.hook);
        }
        let text = |alpha: f32| chrome::text(alpha*panel.content_alpha);
        match panel.kind {
            PanelKind::Thumb | PanelKind::SliderLens | PanelKind::ButtonLens | PanelKind::SegmentedLens | PanelKind::TabLens | PanelKind::Dropdown => {},
            PanelKind::Settings => {
                if let Some(s) = &plan.settings_paint {
                    settings::paint(
                        window,
                        cx,
                        s,
                        panel.content_alpha,
                        |i| feedback(Hot::SettingSlider(i)),
                        |i| feedback(Hot::SettingChoice(i)),
                        |i| feedback(Hot::SettingsPage(i)),
                        |i| feedback(Hot::DropdownOption(i)),
                        feedback(Hot::SettingsClose),
                        [feedback(Hot::ModalCancel), feedback(Hot::ModalSave)],
                    );
                }
            }
            PanelKind::More => {
                let x = panel.rect.left as f32;
                let top = panel.rect.top as f32;
                let r=Box4::new(x,top,panel.rect.width as f32,panel.rect.height as f32);
                let swell = plan.mode_swell.max(0.0);
                chrome::paint_segmented_well(
                    window,
                    plan.mode_well_rect,
                    plan.mode_well_rect.h * 0.5,
                    panel.content_alpha,
                );
                let active_mode = plan.active_mode;
                let active_fb = feedback(Hot::MenuMode(active_mode));
                let lens_box = Box4::new(
                    plan.mode_lens_rect.x + active_fb.pull[0] * 0.35,
                    plan.mode_lens_rect.y + active_fb.pull[1] * 0.25,
                    plan.mode_lens_rect.w,
                    plan.mode_lens_rect.h,
                );
                let lens_radius = lens_box.h * 0.5;
                let (rest_alpha, _) = chrome::segmented_crossfade(swell);
                chrome::paint_segmented_rest_pill(
                    window,
                    lens_box,
                    lens_radius,
                    plan.display.accent_hex,
                    rest_alpha,
                    panel.content_alpha,
                );
                let mode_font_sz = 12.0 * (1.0 + 0.10 * swell);
                let base_track_cx = r.x + r.w * 0.5;
                let spread = if r.w > 20.0 {
                    plan.mode_well_rect.w / (r.w - 16.0)
                } else {
                    1.0
                };
                let scaled_mode_slot = |i: u8| {
                    let b_base = input::mode_rect(r, i);
                    let cx = base_track_cx + (b_base.centre().0 - base_track_cx) * spread;
                    Box4::new(cx - b_base.w * 0.5, b_base.y, b_base.w, b_base.h)
                };
                // 1. Paint labels over the soft rest pill; the selected label carries the active accent.
                for i in 0..4u8 {
                    let b = scaled_mode_slot(i);
                    let selected = i == active_mode;
                    let mf = feedback(Hot::MenuMode(i));
                    if !selected && (mf.hover > 0.01 || mf.press > 0.01) && swell < 0.1 {
                        chrome::control_surface_alpha(window, b, mf, false, panel.content_alpha * 0.45);
                    }
                    chrome::paint_text_box_centred(
                        window,
                        input::MODE_LABELS[i as usize],
                        b,
                        mode_font_sz,
                        if selected { 650.0 } else { 550.0 },
                        if selected {
                            chrome::segmented_active_text_blended(plan.display.accent_hex, rest_alpha, 0.96 * panel.content_alpha)
                        } else {
                            text(0.62 + 0.22 * mf.hover)
                        },
                    );
                }
                // 2. While dragging/sliding, composite the transparent liquid droplet with edge blur & refraction.
                if swell > 0.005 {
                    if let Some(lens_glass) = &plan.mode_lens_glass {
                        window.paint_custom_surface_with_scene(
                            gpui_kit::Bounds {
                                origin: point(px(lens_glass.rect.left as f32), px(lens_glass.rect.top as f32)),
                                size: gpui_kit::size(px(lens_glass.rect.width as f32), px(lens_glass.rect.height as f32)),
                            },
                            lens_glass.surface,
                            lens_glass.snapshot,
                            lens_glass.hook,
                        );
                    }
                    chrome::paint_segmented_lens(
                        window,
                        lens_box,
                        lens_radius,
                        plan.mode_swell,
                        active_fb,
                        panel.content_alpha,
                    );
                }
                let mut rows_spec: Vec<(&str, Icon)> = vec![
                    (more_items[0], Icon::Sync),
                    (more_items[1], Icon::List),
                    (more_items[2], Icon::MotionCover),
                    (input::MORE_DESKTOP_LABEL, Icon::DesktopLyrics),
                ];
                if plan.show_wallpaper_row {
                    rows_spec.push((input::MORE_WALLPAPER_LABEL, Icon::Fullscreen));
                }
                if plan.show_media_row {
                    rows_spec.push((input::MORE_MEDIA_LABEL, Icon::Play));
                }
                rows_spec.push((more_items[3], Icon::Settings));
                let row_count = rows_spec.len();
                for (i, (item, icon)) in rows_spec.into_iter().enumerate() {
                    let b = input::row_rect_count(r, i as u8, row_count);
                    let f = feedback(Hot::MoreRow(i as u8));
                    if i == 0 || i == row_count - 1 {
                        chrome::fill_round(
                            window,
                            r.x + 12.0,
                            b.y - 3.0,
                            (r.w - 24.0).max(20.0),
                            0.75,
                            0.35,
                            text(0.08),
                        );
                    }
                    if i > 0 {
                        chrome::control_surface_alpha_radius(window, b, f, false, panel.content_alpha, 13.5);
                        chrome::chevron(window, b.right() - 16.0, b.y + (b.h - 8.0) * 0.5, text(0.36 + 0.30 * f.hover));
                    }
                    icons::paint(window,cx,icon,Box4::new(x+14.0,b.y+(b.h-16.0)*0.5,16.0,16.0),text(0.55 + 0.25 * f.hover));
                    chrome::paint_text_vcentred(window,item,x+38.0,b.centre().1,12.0,500.0,text(0.88 + 0.08 * f.hover));
                    if i == 0 {
                        let minus_b = input::sync_rect_count(r, false, row_count);
                        let plus_b = input::sync_rect_count(r, true, row_count);
                        chrome::paint_stepper(
                            window,
                            minus_b,
                            plus_b,
                            feedback(Hot::SyncMinus),
                            feedback(Hot::SyncPlus),
                            &plan.sync_label,
                            plan.sync_enabled,
                            panel.content_alpha,
                        );
                    }
                }
            }
            PanelKind::Volume => {
                paint_volume_panel_contents(
                    window,
                    panel.rect,
                    volume_value,
                    plan.volume_enabled,
                    plan.status.as_str(),
                    plan.volume_label.as_str(),
                    plan.display.accent_hex,
                    feedback(Hot::VolumeTrack),
                    panel.content_alpha,
                );
            }
            PanelKind::Picker => {
                let x=panel.rect.left as f32+14.;let top=panel.rect.top as f32;
                let picker_title = match plan.picker_kind {
                    PickerKind::Media => "媒体",
                    PickerKind::Cover => "封面状态",
                    PickerKind::Lyrics => "歌词版本",
                };
                if plan.picker_kind == PickerKind::Cover {
                    chrome::paint_text(window,picker_title,x,top+12.,13.5,650.,text(0.95));
                    let cover_rows = [
                        ("显示封面", Icon::CoverExpand, plan.display.show_cover, plan.cover_switch_fractions[0]),
                        ("动态封面", Icon::MotionCover, plan.display.motion_cover, plan.cover_switch_fractions[1]),
                    ];
                    for (idx, (label, icon, on, frac)) in cover_rows.into_iter().enumerate() {
                        let r = cover_switch_row_rect(
                            panel.rect.left as f32,
                            panel.rect.top as f32,
                            panel.rect.width as f32,
                            idx as u8,
                        );
                        let f = feedback(Hot::CoverSwitch(idx as u8));
                        if f.hover > 0.01 {
                            chrome::fill_round(window, r.x, r.y, r.w, r.h, 10.0, text(0.045 * f.hover));
                        }
                        icons::paint(
                            window,
                            cx,
                            icon,
                            Box4::new(r.x + 8.0, r.y + (r.h - 16.0) * 0.5, 16.0, 16.0),
                            text(if on { 0.78 + 0.16 * f.hover } else { 0.50 + 0.24 * f.hover }),
                        );
                        chrome::paint_text_vcentred(
                            window,
                            label,
                            r.x + 32.0,
                            r.centre().1,
                            12.5,
                            500.0,
                            text(if on { 0.94 } else { 0.78 + 0.14 * f.hover }),
                        );
                        let track = cover_switch_track_rect(r);
                        chrome::paint_switch_with_accent(
                            window,
                            track.x,
                            track.y,
                            track.w,
                            track.h,
                            on,
                            frac,
                            f,
                            plan.display.accent_hex,
                            panel.content_alpha,
                        );
                    }
                } else {
                    paint_lyrics_picker_contents(
                        window,
                        cx,
                        panel.rect,
                        picker_title,
                        plan.picker_kind == PickerKind::Lyrics,
                        candidates,
                        plan.picker_scroll_px,
                        &plan.smart_switch_fractions,
                        plan.picker_refresh_angle,
                        plan.display.accent_hex,
                        light_panel,
                        panel.content_alpha,
                        plan.track_title.as_str(),
                        feedback(Hot::PickerImport),
                        feedback(Hot::PickerRefresh),
                        |i| feedback(Hot::Candidate(i)),
                        |i| feedback(Hot::SmartTranslation(i)),
                    );
                }
            }
            PanelKind::Island => {}
        }
    }
    if let Some(lens) = &plan.slider_lens {
        window.paint_custom_surface_with_scene(
            gpui_kit::Bounds {
                origin: point(px(lens.rect.left as f32), px(lens.rect.top as f32)),
                size: gpui_kit::size(px(lens.rect.width as f32), px(lens.rect.height as f32)),
            },
            lens.surface,
            lens.snapshot,
            lens.hook,
        );
    }
    if !plan.settings_only && !plan.fullscreen && win_radius > 0.5 {
        chrome::stroke_round(
            window,
            0.0,
            0.0,
            m.win_w.max(1.0),
            m.win_h.max(1.0),
            win_radius,
            1.0,
            chrome::text(0.14),
        );
    }

    // 9. Cursor: the pointer is drawn by the OS; this only feeds hover.
    let _ = plan.cursor;
    let _ = plan.drag_thumb;
    let _ = panel_anchor_hint(plan.chrome.more);
    chrome::set_light_theme(false);
}

fn panel_anchor_hint(r: Box4) -> f32 {
    r.y
}

pub(in crate::player) fn paint_sidecar_glass_frame(
    window: &mut Window,
    r: Box4,
    radius: f32,
    theme_mode: model::ThemeMode,
    surface_alpha: f32,
) {
    let sidecar_alpha = match theme_mode {
        model::ThemeMode::Light => 0.88,
        model::ThemeMode::Dark => 0.84,
        model::ThemeMode::LiquidGlass => 0.29,
    };
    chrome::fill_round(
        window,
        r.x,
        r.y,
        r.w,
        r.h,
        radius,
        chrome::themed_surface(sidecar_alpha * surface_alpha),
    );
    chrome::stroke_round(
        window,
        r.x,
        r.y,
        r.w,
        r.h,
        radius,
        1.0,
        chrome::themed_border(0.18 * surface_alpha),
    );
}

#[allow(clippy::too_many_arguments)]
pub(in crate::player) fn paint_volume_panel_contents(
    window: &mut Window,
    panel_rect: GlassRect,
    volume_value: f64,
    volume_enabled: bool,
    status_str: &str,
    volume_label: &str,
    accent_hex: u32,
    track_feedback: Feedback,
    content_alpha: f32,
) {
    let text = |alpha: f32| chrome::text(alpha * content_alpha);
    let accent = |alpha: f32| chrome::accent_rgb(accent_hex, alpha * content_alpha);
    let x = panel_rect.left as f32 + 14.0;
    let right_x = panel_rect.left as f32 + panel_rect.width as f32 - 14.0;
    let top = panel_rect.top as f32 + 12.0;
    chrome::paint_text(window, "音量", x, top, 13.0, 600.0, text(0.9));
    let status_w = chrome::measure(window, status_str, 12.0, 600.0).0.min(96.0);
    chrome::paint_text_fitted(
        window,
        status_str,
        (right_x - status_w).max(x + 48.0),
        top,
        96.0,
        12.0,
        600.0,
        text(0.72),
    );
    let f = (volume_value / 100.0).clamp(0.0, 1.0) as f32;
    chrome::paint_slider(
        window,
        x,
        panel_rect.top as f32 + 62.0,
        panel_rect.width as f32 - 28.0,
        f,
        if volume_enabled { accent(0.95) } else { text(0.35) },
        text(0.18),
        if volume_enabled {
            track_feedback
        } else {
            Feedback::default()
        },
        content_alpha,
    );
    chrome::paint_text(window, volume_label, x, top + 23.0, 11.0, 400.0, text(0.50));
    if volume_enabled && track_feedback.hover > 0.0 {
        chrome::stroke_round(
            window,
            x + (panel_rect.width as f32 - 28.0) * f - 11.0,
            panel_rect.top as f32 + 51.0,
            22.0,
            22.0,
            11.0,
            1.0,
            accent(track_feedback.hover * 0.55),
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::player) fn paint_lyrics_picker_contents(
    window: &mut Window,
    cx: &App,
    panel_rect: GlassRect,
    picker_title: &str,
    show_lyrics_actions: bool,
    candidates: &[PickerEntry],
    picker_scroll_px: f32,
    smart_switch_fractions: &[(String, f32)],
    picker_refresh_angle: f32,
    accent_hex: u32,
    light_panel: bool,
    content_alpha: f32,
    track_title: &str,
    import_fb: Feedback,
    refresh_fb: Feedback,
    candidate_fb: impl Fn(usize) -> Feedback,
    smart_fb: impl Fn(usize) -> Feedback,
) {
    let text = |alpha: f32| chrome::text(alpha * content_alpha);
    let accent = |alpha: f32| chrome::accent_rgb(accent_hex, alpha * content_alpha);
    let x = panel_rect.left as f32 + 14.0;
    let top = panel_rect.top as f32;
    chrome::paint_text(window, picker_title, x, top + 12.0, 13.5, 650.0, text(0.95));
    if show_lyrics_actions {
        let import_box = picker_import_rect(panel_rect);
        if import_fb.hover > 0.01 || import_fb.press > 0.01 {
            let (icx, icy) = import_box.centre();
            chrome::fill_round(
                window,
                icx - 12.0,
                icy - 12.0,
                24.0,
                24.0,
                12.0,
                text(0.08 * import_fb.hover + 0.06 * import_fb.press),
            );
        }
        let (icx, icy) = import_box.centre();
        icons::paint_elastic(
            window,
            cx,
            Icon::Plus,
            Box4::new(icx - 8.0, icy - 8.0, 16.0, 16.0),
            text(0.72 + 0.24 * import_fb.hover),
            import_fb,
        );
        let refresh_box = picker_refresh_rect(panel_rect);
        if refresh_fb.hover > 0.01 || refresh_fb.press > 0.01 {
            let (rcx, rcy) = refresh_box.centre();
            chrome::fill_round(
                window,
                rcx - 12.0,
                rcy - 12.0,
                24.0,
                24.0,
                12.0,
                text(0.08 * refresh_fb.hover + 0.06 * refresh_fb.press),
            );
        }
        let (rcx, rcy) = refresh_box.centre();
        icons::paint_elastic_rotated(
            window,
            cx,
            Icon::Refresh,
            Box4::new(rcx - 8.0, rcy - 8.0, 16.0, 16.0),
            text(0.72 + 0.24 * refresh_fb.hover),
            refresh_fb,
            picker_refresh_angle,
        );
    }
    let capacity = (((panel_rect.height as f32 - VERSION_HEADER) / VERSION_ROW).floor() as usize).min(4);
    let paint_badge = |window: &mut Window, bx: f32, cy: f32, label: &str, bg: Hsla, border: Hsla, fg: Hsla| -> f32 {
        let tw = chrome::measure(window, label, 10.5, 600.0).0;
        let bw = tw + 12.0;
        let by = cy - 9.0;
        chrome::fill_round(window, bx, by, bw, 18.0, 5.5, bg);
        chrome::stroke_round(window, bx, by, bw, 18.0, 5.5, 0.85, border);
        chrome::paint_text_vcentred(window, label, bx + 6.0, cy, 10.5, 600.0, fg);
        bw + 5.0
    };
    let list_top = top + VERSION_HEADER;
    let list_h = (capacity as f32 * VERSION_ROW).max(0.0);
    let list_bot = list_top + list_h;
    let max_scroll_px = (candidates.len().saturating_sub(capacity) as f32 * VERSION_ROW).max(0.0);
    let scroll_px = picker_scroll_px.clamp(0.0, max_scroll_px);
    window.with_content_mask(
        Some(ContentMask {
            bounds: gpui_kit::bounds(
                point(px(panel_rect.left as f32), px(list_top)),
                gpui_kit::size(px(panel_rect.width as f32), px(list_h)),
            ),
        }),
        |window| {
    for (i, entry) in candidates.iter().enumerate() {
        let y = list_top + i as f32 * VERSION_ROW - scroll_px;
        if y + VERSION_ROW < list_top - 4.0 || y > list_bot + 4.0 {
            continue;
        }
        let r = Box4::new(
            panel_rect.left as f32 + 8.0,
            y,
            panel_rect.width as f32 - 16.0,
            VERSION_ROW - 4.0,
        );
        let f = candidate_fb(i);
        if entry.selected {
            chrome::fill_round(window, r.x, r.y, r.w, r.h, 10.0, accent(0.11));
            chrome::stroke_round(window, r.x, r.y, r.w, r.h, 10.0, 1.25, accent(0.86));
        } else if f.hover > 0.01 {
            chrome::fill_round(window, r.x, r.y, r.w, r.h, 10.0, text(0.045 * f.hover));
        }
        let ry = y;
        let top_cy = ry + 16.5;
        let title_alpha = if entry.selected { 0.98 } else { 0.88 + 0.12 * f.hover };
        let sub_alpha = 0.54 + 0.18 * f.hover;
        let has_smart_btn = !entry.has_translation && !entry.smart_translation_key.is_empty();
        let check_w = if entry.selected { 22.0 } else { 0.0 };
        let match_w = if entry.match_badge.is_empty() {
            0.0
        } else {
            chrome::measure(window, &entry.match_badge, 11.5, 550.0).0 + 6.0
        };
        let top_right_reserve = check_w + match_w + 8.0;
        let bot_right_reserve = if has_smart_btn { 104.0 } else { 8.0 };
        let row_mask = ContentMask {
            bounds: gpui_kit::bounds(
                point(px(r.x + 3.0), px(r.y + 3.0)),
                size(px((r.w - 6.0).max(20.0)), px(r.h - 6.0)),
            ),
        };
        window.with_content_mask(Some(row_mask), |window| {
            let tx = x + 3.0;
            let title_max = (r.right() - top_right_reserve - tx).max(40.0);
            let title_w = chrome::measure(window, &entry.title, 13.5, 650.0).0.min(title_max);
            chrome::paint_text_fitted_vcentred(window, &entry.title, tx, top_cy, title_max, 13.5, 650.0, text(title_alpha));
            let mut bx = tx + title_w + 8.0;
            if entry.source == "custom" {
                bx += paint_badge(window, bx, top_cy, "本地", accent(0.18), accent(0.45), text(0.92));
            }
            if let Some(badge) = entry.timing_badge {
                let (bg, border, fg) = if badge == "逐字" {
                    (accent(0.24), accent(0.58), text(0.96))
                } else if badge == "逐行" {
                    (text(0.12), text(0.26), text(0.86))
                } else {
                    (text(0.08), text(0.18), text(0.66))
                };
                bx += paint_badge(window, bx, top_cy, badge, bg, border, fg);
            }
            if entry.has_translation {
                let green = Hsla::from(rgb(0x30D158)).opacity((if light_panel { 0.14 } else { 0.20 }) * content_alpha);
                let green_border = Hsla::from(rgb(0x30D158)).opacity((if light_panel { 0.38 } else { 0.52 }) * content_alpha);
                let green_text = Hsla::from(rgb(if light_panel { 0x15692C } else { 0x7CE89A })).opacity(0.94 * content_alpha);
                paint_badge(window, bx, top_cy, "翻译", green, green_border, green_text);
            }
            let sub_max_w = (r.right() - bot_right_reserve - tx - 4.0).max(40.0);
            chrome::paint_text_fitted_vcentred(window, &entry.subtitle, tx, ry + 40.0, sub_max_w, 11.0, 450.0, text(sub_alpha));
        });
        if !entry.match_badge.is_empty() {
            let mx = r.right() - 8.0 - check_w - (match_w - 6.0);
            chrome::paint_text_vcentred(window, &entry.match_badge, mx, top_cy, 11.5, 550.0, text(0.58));
        }
        if entry.selected {
            let cx_chk = r.right() - 22.0;
            let cy_chk = top_cy - 8.0;
            chrome::fill_round(window, cx_chk, cy_chk, 16.0, 16.0, 8.0, accent(0.95));
            chrome::paint_text_box_centred(window, "✓", Box4::new(cx_chk, cy_chk, 16.0, 16.0), 11.0, 700.0, Hsla::from(rgb(0xFFFFFF)).opacity(0.98 * content_alpha));
        }
        if has_smart_btn {
            let btn = picker_smart_btn_rect(r);
            let sf = smart_fb(i);
            let on = entry.smart_translation;
            let frac = smart_switch_fractions
                .iter()
                .find(|(k, _)| k == &entry.smart_translation_key)
                .map(|(_, v)| *v)
                .unwrap_or(if on { 1.0 } else { 0.0 });
            let label_alpha = if on { 0.88 } else { 0.56 + 0.20 * sf.hover };
            chrome::paint_text(window, "智能翻译", btn.x, btn.y + 3.5, 11.0, 500.0, text(label_alpha));
            chrome::paint_switch_with_accent(
                window,
                btn.right() - 34.0,
                btn.y + 1.5,
                34.0,
                19.0,
                on,
                frac,
                sf,
                accent_hex,
                content_alpha,
            );
        }
    }
        },
    );
    if capacity > 0 && candidates.len() > capacity {
        let track_x = panel_rect.left as f32 + panel_rect.width as f32 - 7.0;
        let track_y = top + VERSION_HEADER + 4.0;
        let track_h = (capacity as f32 * VERSION_ROW - 12.0).max(16.0);
        let thumb_h = ((capacity as f32 / candidates.len() as f32) * track_h).clamp(20.0, track_h);
        let frac = if max_scroll_px > 0.5 {
            (scroll_px / max_scroll_px).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let thumb_y = track_y + (track_h - thumb_h) * frac;
        chrome::fill_round(window, track_x, thumb_y, 2.4, thumb_h, 1.2, text(0.26));
    }
    if capacity == 0 && !candidates.is_empty() {
        chrome::paint_text(window, "当前空间不足，请调大窗口或切换模式", x, top + VERSION_HEADER + 5.0, 11.0, 500.0, text(0.55));
    }
    if candidates.is_empty() {
        if show_lyrics_actions && track_title != "未在播放" {
            let row_y = top + VERSION_HEADER + 6.0;
            let row_h = 34.0_f32;
            chrome::paint_text_vcentred(
                window,
                "本地歌词扫描目录",
                x,
                row_y + row_h * 0.5,
                12.5,
                500.0,
                text(0.82),
            );
            let btn = picker_offline_folder_btn_rect(panel_rect);
            chrome::fill_round(window, btn.x, btn.y, btn.w, btn.h, btn.h * 0.5, text(0.09));
            chrome::stroke_round(window, btn.x, btn.y, btn.w, btn.h, btn.h * 0.5, 0.9, text(0.22));
            let lbl = "选择目录";
            let (lw, _) = chrome::measure(window, lbl, 11.5, 550.0);
            chrome::paint_text_vcentred(
                window,
                lbl,
                btn.x + (btn.w - lw) * 0.5,
                btn.centre().1,
                11.5,
                550.0,
                text(0.90),
            );
        } else {
            let empty_msg = if track_title == "未在播放" { "未在播放" } else { "暂无匹配歌词" };
            chrome::paint_text(window, empty_msg, x, top + VERSION_HEADER + 12.0, 12.0, 500.0, text(0.55));
        }
    }
}


pub(in crate::player) fn offset_label(ms: i64) -> String {
    if ms > 0 {
        format!("+{ms}ms")
    } else {
        format!("{ms}ms")
    }
}

pub(super) fn any_lyric_source_enabled(config: &serde_json::Value) -> bool {
    config
        .pointer("/lyrics/enabledSources")
        .and_then(serde_json::Value::as_object)
        .is_none_or(|sources| sources.values().any(|v| v.as_bool().unwrap_or(true)))
}
