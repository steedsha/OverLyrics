use super::*;

mod color;
mod more;
mod picker;
mod volume;

pub(super) use color::DesktopColorPopover;
pub(super) use more::{
    DesktopDisplayPopover, DesktopLinesPopover, DesktopMorePopover, PaletteDrag, paint_hsv_palette,
};
pub(crate) use picker::DesktopLyricsPickerPopover;
pub(crate) use volume::{DesktopVolumePopover, VOLUME_POPOVER_H, VOLUME_POPOVER_W};