//! Frame-level contracts: how a renderer gets the GPUI device, how it names a
//! render target, and how resource accounting is reported.

use oly_contracts::MonoMs;

/// A borrowed handle to the single D3D11 device GPUI created for the process.
///
/// The renderer never creates a device of its own: every window, atlas and
/// custom pass must live on this one device so textures can be shared without
/// a copy and so GPU memory is attributable to a single owner.
#[derive(Clone, Copy, Debug)]
pub struct DeviceAccess {
    _private: (),
}

impl DeviceAccess {
    /// Placeholder until the fork exposes the real device handle.
    pub fn unavailable() -> Self {
        Self { _private: () }
    }
}

/// Identifies the surface a frame is encoded into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderTarget {
    /// Target width in physical pixels.
    pub width: u32,
    /// Target height in physical pixels.
    pub height: u32,
}

impl RenderTarget {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// Counters that must return to their baseline when a window closes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceStats {
    pub textures: u64,
    pub buffers: u64,
    pub samplers: u64,
    pub shaders: u64,
}

impl ResourceStats {
    pub fn baseline() -> Self {
        Self::default()
    }
}

/// Monotonic timestamp supplied by the caller's single clock.
#[derive(Clone, Copy, Debug)]
pub struct FrameTime(pub MonoMs);
