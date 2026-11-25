//! Library half of the experiment binary.
//!
//! The binary is a thin shell around these modules so the bindings can be unit
//! tested without opening a window.

pub mod commands;
pub mod player;
pub mod ui;

pub mod live;
