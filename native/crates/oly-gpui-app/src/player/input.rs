//! Native drag exclusions and the legacy more-menu geometry.
use super::model::{Box4, Metrics};
pub const MORE_LABELS: [&str; 4] = ["歌词同步", "歌词版本", "封面状态", "设置"];
pub const MORE_MEDIA_LABEL: &str = "媒体";
pub const MORE_DESKTOP_LABEL: &str = "桌面歌词";
pub const MORE_WALLPAPER_LABEL: &str = "壁纸模式";
pub const MODE_LABELS: [&str; 4] = ["迷你", "歌词", "窗口", "全屏"];
pub fn more_size(view_height: f32) -> (f64, f64) {
    more_size_for_rows(view_height, 5)
}
pub fn more_size_with_media(view_height: f32, show_media: bool) -> (f64, f64) {
    more_size_for_rows(view_height, if show_media { 6 } else { 5 })
}
pub fn more_size_for_rows(view_height: f32, row_count: usize) -> (f64, f64) {
    let base = 44.0_f32 + (row_count.max(1) as f32) * 36.0;
    (252., base.min((view_height - 16.).max(112.)) as f64)
}
pub fn mode_rect(r: Box4, i: u8) -> Box4 {
    let cell_w = (r.w - 20.) / 4.;
    Box4::new(
        r.x + 10. + i as f32 * cell_w + 1.,
        r.y + 8.,
        (cell_w - 2.).max(16.),
        28.,
    )
}
pub fn row_rect(r: Box4, i: u8) -> Box4 {
    row_rect_count(r, i, MORE_LABELS.len())
}
pub fn row_rect_count(r: Box4, i: u8, count: usize) -> Box4 {
    let h = (r.h - 44.) / count.max(1) as f32;
    Box4::new(r.x + 6., r.y + 40. + i as f32 * h, r.w - 12., h - 1.)
}
pub fn sync_rect(r: Box4, plus: bool) -> Box4 {
    sync_rect_count(r, plus, MORE_LABELS.len())
}
pub fn sync_rect_count(r: Box4, plus: bool, count: usize) -> Box4 {
    let row = row_rect_count(r, 0, count);
    let btn_h = (row.h - 10.).clamp(18., 25.);
    let btn_y = row.y + (row.h - btn_h) * 0.5;
    Box4::new(
        r.right() - if plus { 32. } else { 119. },
        btn_y,
        24.,
        btn_h,
    )
}
fn subtract(r: Box4, b: Box4) -> Vec<Box4> {
    let l = r.x.max(b.x);
    let t = r.y.max(b.y);
    let right = r.right().min(b.right());
    let bottom = r.bottom().min(b.bottom());
    if l >= right || t >= bottom {
        return vec![r];
    }
    [
        Box4::new(r.x, r.y, r.w, t - r.y),
        Box4::new(r.x, bottom, r.w, r.bottom() - bottom),
        Box4::new(r.x, t, l - r.x, bottom - t),
        Box4::new(right, t, r.right() - right, bottom - t),
    ]
    .into_iter()
    .filter(|r| r.w > 1. && r.h > 1.)
    .collect()
}
pub fn drag_regions(m: &Metrics, obstacles: impl IntoIterator<Item = Box4>) -> Vec<Box4> {
    let mut regions = vec![Box4::new(0., 0., m.win_w, m.win_h)];
    for obstacle in std::iter::once(m.lyric_viewport).chain(obstacles) {
        regions = regions
            .into_iter()
            .flat_map(|r| subtract(r, obstacle))
            .collect();
    }
    regions
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_drag_preserves_controls_lyrics_and_popup_input() {
        let m =
            super::super::model::metrics(1272., 843., (1920., 1080.), false, &Default::default());
        let button = Box4::new(250., 660., 56., 56.);
        let popup = Box4::new(210., 450., 252., 192.);
        let rs = drag_regions(&m, [button, popup]);
        assert!(rs.iter().any(|r| r.contains(600., 20.)));
        assert!(rs.iter().any(|r| r.contains(m.cover.centre().0, m.cover.centre().1)));
        for (x, y) in [
            (280., 680.),
            (220., 460.),
            m.lyrics.centre(),
        ] {
            assert!(!rs.iter().any(|r| r.contains(x, y)));
        }
    }
    #[test]
    fn menu_sync_and_modes_have_distinct_hits_even_in_short_windows() {
        let size = more_size(176.);
        let r = Box4::new(0., 0., size.0 as f32, size.1 as f32);
        assert!(row_rect(r, 3).bottom() <= r.bottom());
        for i in 0..4 {
            assert!(mode_rect(r, i).bottom() < row_rect(r, 0).y);
        }
        assert!(sync_rect(r, false).right() < sync_rect(r, true).x);
        let with_media = more_size_with_media(800., true);
        let mr = Box4::new(0., 0., with_media.0 as f32, with_media.1 as f32);
        assert!(sync_rect_count(mr, false, 5).h <= 25.0);
    }
}

pub fn picker_capacity(panel_height:f64)->usize {(((panel_height-16.)/64.).floor() as usize).clamp(1,4)}
#[cfg(test)] mod picker_tests {
 #[test]fn short_mini_list_can_reach_its_last_candidate(){let visible=super::picker_capacity(160.);assert_eq!(visible,2);let total=4usize;let last_start=total.saturating_sub(visible);assert_eq!(last_start+visible,total);assert_eq!(super::picker_capacity(272.),4);}
}
