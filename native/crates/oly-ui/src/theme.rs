//! Explicit UI theme value object for reusable multi-window components.

use gpui_kit::{Hsla, rgb};

use crate::chrome::{ACCENT_HEX, LIGHT_TEXT_HEX, TEXT_HEX};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub light: bool,
    pub accent_hex: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            light: false,
            accent_hex: ACCENT_HEX,
        }
    }
}

impl Theme {
    pub const fn dark() -> Self {
        Self {
            light: false,
            accent_hex: ACCENT_HEX,
        }
    }

    pub const fn light() -> Self {
        Self {
            light: true,
            accent_hex: ACCENT_HEX,
        }
    }

    pub const fn with_accent(mut self, accent_hex: u32) -> Self {
        self.accent_hex = accent_hex & 0x00FF_FFFF;
        self
    }

    pub fn text_rgb(self) -> u32 {
        if self.light { LIGHT_TEXT_HEX } else { TEXT_HEX }
    }

    pub fn text(self, alpha: f32) -> Hsla {
        Hsla::from(rgb(self.text_rgb())).opacity(alpha)
    }

    pub fn surface(self, alpha: f32) -> Hsla {
        let hex = if self.light { 0xFAFC_FF } else { 0x1618_1D };
        Hsla::from(rgb(hex)).opacity(alpha)
    }

    pub fn border(self, alpha: f32) -> Hsla {
        let hex = if self.light { 0x0000_00 } else { TEXT_HEX };
        Hsla::from(rgb(hex)).opacity(alpha)
    }

    pub fn accent(self, alpha: f32) -> Hsla {
        Hsla::from(rgb(self.accent_hex)).opacity(alpha)
    }
}
