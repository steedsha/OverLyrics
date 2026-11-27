//! Reusable Liquid Glass GPUI component, motion, geometry, color, and Win32 overlay toolkit.
//!
//! This crate intentionally has zero dependency on music/lyrics business logic
//! (`oly-engine`, `oly-contracts`, `oly-platform-win`) so any GPUI desktop app can reuse it.

pub mod color;
pub mod elastic;
pub mod geometry;
pub mod glass;
pub mod interaction;
pub mod panel_motion;
pub mod scrollbar;
pub mod win32_window;
