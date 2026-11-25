//! Windows platform integration: SMTC, volume, window placement, tray, and the
//! Media Foundation decoder.
//!
//! Task 9 adds the Media Foundation path, Task 12 the SMTC/window/tray surface.

// `deny` rather than `forbid`: the Media Foundation decoder is COM/D3D11 heavy
// and has no safe bindings. The unsafe is confined to `media` (declared with a
// module-level `allow(unsafe_code)`), so the rest of the crate stays protected.
#![deny(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(windows)]
pub mod audio;
pub mod file_dialog;
pub mod hotkey;
pub mod media;
pub mod smtc;
pub mod tray;
pub mod volume;
pub mod window;

#[cfg(windows)]
pub mod http;
