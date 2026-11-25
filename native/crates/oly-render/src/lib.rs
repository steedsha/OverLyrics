//! GPU scene renderer for the experiment: one D3D11 device shared with GPUI,
//! custom text/effect/glass/video passes, and the QA capture path.
//!
//! Task 2 establishes the device-access contract and the custom surface
//! primitive; Tasks 3-9 fill in the individual passes.

// `deny` rather than `forbid`: D3D11 has no safe bindings, so
// `background::gpu` opts in with a module-level `allow(unsafe_code)`. Every
// other module stays safe, and `deny` still fails the build on an accidental
// opt-in anywhere else.
#![deny(unsafe_code)]

pub mod background;
pub mod emphasis;
pub mod frame;
pub mod glass;
pub mod glow;
pub mod glyphs;
pub mod layout;
pub mod mask;
pub mod text;
pub mod video;
pub mod video_gpu;

// `glass::Rect` is not re-exported: `layout::Rect` predates it and the
// popover placement geometry stays namespaced under `glass`.
pub use glass::{
    DURATION_PRESS_MS, DURATION_RELEASE_MS, DURATION_SPRING_MS, Entrance, Material, Side,
    composite_glass, place_popover, rounded_rect_sdf, spring_curve,
};

pub use emphasis::{
    CharAnimation, EmphasisParams, EmphasisPlan, FloatKeyframe, GlowKeyframe, GLOW_ALPHA_SCALE,
    float_keyframes, glow_keyframes, sample_float, sample_glow, should_emphasize,
};
pub use frame::{DeviceAccess, RenderTarget, ResourceStats};
pub use glyphs::{GlyphAtlas, GlyphAtlasStats, GlyphKey, GlyphMode, GlyphTile};
pub use layout::{LineBreakMode, Rect, SourceWord};
pub use mask::{MaskKeyframe, MaskSpec, MaskWord, alpha_at_x, position_at, word_keyframes};
pub use text::{FontSpec, LineLayout, TextLayoutCache, TextLayoutInput};
