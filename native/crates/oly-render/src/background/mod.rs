//! The am_bg v36 immersion background, ported for the native renderer.
//!
//! The reference is `src/windows/lyrics/background/*` at commit 708f6b1, itself
//! an extraction of `am_bg/deliverable/apple_music_immersion_import.html`
//! (sha256 82706b03…). The module split follows the reference one-for-one so a
//! reviewer can diff each file:
//!
//! | reference        | this module        | stays on the CPU? |
//! |------------------|--------------------|-------------------|
//! | `colour.ts`      | [`colour`]         | yes               |
//! | `artwork.ts`     | [`artwork`]        | yes               |
//! | `lifecycle.ts`   | [`lifecycle`]      | yes               |
//! | `presentation.ts`| [`presentation`]   | yes               |
//! | `renderer.ts`    | [`scene`] + `gpu`  | split             |
//! | `shaders.ts`     | `shaders/background.hlsl` | GPU        |
//!
//! [`scene`] owns the state machine (warm-up, 550 ms cross-fade, pending cover,
//! present order) because those rules are testable without a device. The GPU
//! pass order and the four 200x200 RGBA16F targets live in the platform renderer
//! that consumes [`scene::FramePlan`]; nothing here creates a device.

pub mod artwork;
pub mod colour;
pub mod gpu;
pub mod lifecycle;
pub mod presentation;
pub mod scene;

pub use artwork::{Kernel, PreparedArtwork, RgbaImage, prepare_artwork};
pub use colour::{
    ColourParams, CoverSample, CoverStats, SAMPLE_SIZE, SIZE, colour_artwork, hsv_to_rgb, rgb_to_hsv,
};
pub use lifecycle::{
    BackgroundClock, LatestImageRequest, MAX_DELTA_MS, MAX_STEPS, STEP_MS,
};
pub use presentation::{DisplayOptions, ImmersionViewport, WindowMode, immersion_scrim, uses_immersion};
pub use scene::{
    BackgroundScene, FADE_MS, FramePlan, MAX_WARM_PER_TICK, PresentPlan, SceneSlot, WARM_STEPS,
};
