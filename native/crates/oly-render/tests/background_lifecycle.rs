//! G1-3 lifecycle: twenty cover switches and teardown must not accumulate
//! either GPU scene resources or CPU scratch bitmaps.
//!
//! Two separate budgets are measured here and they are deliberately not
//! conflated:
//!
//! * **GPU side.** One `GpuScene` owns a 200x200 source texture and four
//!   200x200 render targets. At most three scenes are resident (`current`,
//!   `previous`, `pending`), so fifteen textures is the ceiling. `setArtwork`
//!   drops any earlier pending scene before building a new one, which is the
//!   path a fast double-switch takes — the second switch must not keep the
//!   first pending scene alive.
//! * **CPU side.** `prepare_artwork_tracked` reports every intermediate bitmap
//!   (probe, padded canvas, artwork square, downscale, blur) to a
//!   [`BitmapLedger`]. A finished pass must leave zero live scratch and the
//!   same peak on switch 20 as on switch 1: a growing peak is exactly the
//!   "accumulated temporary bitmap" the acceptance plan forbids.
//!
//! Run with:
//!   cargo test -p oly-render --test background_lifecycle -- --nocapture

use oly_core::cache::BitmapLedger;
use oly_render::background::artwork::{
    PREPARED_BITMAP_BYTES, RgbaImage, SCRATCH_PEAK_BUDGET, prepare_artwork_tracked,
};
use oly_render::background::gpu::{BackgroundRenderer, Sampling};

/// One live scene owns a working source plus acc/msh/b1/b2.
const TEXTURES_PER_SCENE: u64 = 5;
/// `current`, `previous` and `pending` can all be resident at once.
const MAX_LIVE_TEXTURES: u64 = 3 * TEXTURES_PER_SCENE;
/// `WARM_STEPS / MAX_WARM_PER_TICK` = 2700 / 90.
const WARM_FRAMES: u32 = 30;
/// The acceptance plan's switch count.
const SWITCHES: u8 = 20;
/// Scratch bitmaps one preparation pass allocates: probe, padded canvas,
/// artwork square, downscale, blur.
const SCRATCH_PER_PREPARATION: u64 = 5;

/// A framed cover that changes with the switch index.
///
/// The border is flat by construction, so the probe always reads this branch.
/// A constant image would make a stale-handle bug invisible — every switch
/// would produce byte-identical output — so the tint and the inner square move
/// with the index. [`full_bleed_cover`] covers the other probe branch.
fn cover(switch: u8) -> RgbaImage {
    let tint = switch.wrapping_mul(11);
    let mut image = RgbaImage::filled(512, 512, [tint / 2, tint, 255 - tint]);
    let (bright, mid) = (255 - tint, tint / 3);
    for y in 106..406 {
        for x in 106..406 {
            let i = ((y * 512 + x) * 4) as usize;
            image.pixels[i] = bright;
            image.pixels[i + 1] = 40;
            image.pixels[i + 2] = mid;
            image.pixels[i + 3] = 255;
        }
    }
    image
}

/// A full-bleed cover: the border itself varies, so the probe reads the
/// `inner = 800` branch and the pass has to hold the larger artwork square.
fn full_bleed_cover() -> RgbaImage {
    let mut image = RgbaImage::filled(512, 512, [10, 12, 16]);
    for y in 0..256 {
        for x in 0..512 {
            let i = ((y * 512 + x) * 4) as usize;
            image.pixels[i] = 220;
            image.pixels[i + 1] = 210;
            image.pixels[i + 2] = 190;
        }
    }
    image
}

/// Drives one pending scene all the way to current.
fn warm_up(renderer: &mut BackgroundRenderer, frame: &mut u64, sampling: Sampling) -> u64 {
    let mut peak = renderer.live_textures();
    for _ in 0..WARM_FRAMES {
        *frame += 1;
        let now = *frame as f64 * 1000.0 / 60.0;
        renderer.tick(&[], now, sampling).expect("warm-up tick");
        peak = peak.max(renderer.live_textures());
    }
    peak
}

#[test]
fn twenty_cover_switches_release_every_scene() {
    let mut renderer = BackgroundRenderer::new_own().expect("a D3D11 device");
    renderer.resize(640, 360).expect("present target");
    let sampling = Sampling::default();
    let mut frame = 0u64;
    let mut peak = 0u64;

    for switch in 0..SWITCHES {
        let mut ledger = BitmapLedger::default();
        let prepared = prepare_artwork_tracked(&cover(switch), &mut ledger);
        renderer.set_artwork(&prepared).expect("set artwork");

        // `setArtwork` drops the previous pending scene first, so a switch can
        // never hold more than one pending scene. It does *not* drop the
        // current one: the new cover warms up behind the old picture while the
        // old one is still on screen, which is the whole point of `pending`.
        let live = renderer.live_textures();
        assert_eq!(
            live % TEXTURES_PER_SCENE,
            0,
            "switch {switch}: {live} live textures is not a whole number of five-texture scenes"
        );
        assert!(
            live <= MAX_LIVE_TEXTURES,
            "switch {switch}: {live} textures live, ceiling is {MAX_LIVE_TEXTURES}"
        );
        if switch == 0 {
            assert_eq!(
                live, TEXTURES_PER_SCENE,
                "the first switch has no current scene to keep"
            );
        }

        peak = peak.max(warm_up(&mut renderer, &mut frame, sampling));
        assert!(
            renderer.is_ready(),
            "switch {switch}: the pending scene never became current"
        );
        peak = peak.max(renderer.live_textures());
        assert!(
            renderer.live_textures() <= MAX_LIVE_TEXTURES,
            "switch {switch}: {} textures live, ceiling is {MAX_LIVE_TEXTURES}",
            renderer.live_textures()
        );
    }

    assert_eq!(
        peak, MAX_LIVE_TEXTURES,
        "the run never held current+previous+pending at once, so the ceiling went untested"
    );

    // Teardown: every texture the renderer owns goes back to the device.
    renderer.clear();
    assert_eq!(renderer.live_textures(), 0, "clear() left textures behind");
    assert!(!renderer.is_ready());
    assert!(!renderer.has_work());
}

#[test]
fn a_fast_double_switch_does_not_keep_the_first_pending_scene() {
    let mut renderer = BackgroundRenderer::new_own().expect("a D3D11 device");
    renderer.resize(640, 360).expect("present target");

    // Two covers installed before a single tick: the second replaces the
    // first pending scene rather than joining it.
    renderer.set_artwork(&prepare_artwork_tracked(&cover(1), &mut BitmapLedger::default()))
        .expect("first artwork");
    renderer
        .set_artwork(&prepare_artwork_tracked(&cover(2), &mut BitmapLedger::default()))
        .expect("second artwork");
    assert_eq!(
        renderer.live_textures(),
        TEXTURES_PER_SCENE,
        "the replaced pending scene was not released"
    );
}

#[test]
fn twenty_preparations_leave_no_scratch_bitmaps_behind() {
    let mut ledger = BitmapLedger::default();
    let mut peak_after_first = 0u64;

    for switch in 0..SWITCHES {
        let prepared = prepare_artwork_tracked(&cover(switch), &mut ledger);

        assert_eq!(
            ledger.live(),
            0,
            "switch {switch}: {} scratch bitmaps still live",
            ledger.live()
        );
        assert_eq!(
            ledger.live_bytes(),
            0,
            "switch {switch}: scratch bytes still charged to the ledger"
        );
        assert_eq!(
            prepared.source.pixels.len() + prepared.sample.len(),
            PREPARED_BITMAP_BYTES as usize,
            "switch {switch}: the caller must receive exactly the documented bytes"
        );
        assert!(
            ledger.peak_bytes() <= SCRATCH_PEAK_BUDGET,
            "switch {switch}: scratch peak {} exceeds the {SCRATCH_PEAK_BUDGET}-byte budget",
            ledger.peak_bytes()
        );

        if switch == 0 {
            peak_after_first = ledger.peak_bytes();
        }
    }

    assert_eq!(
        ledger.peak_bytes(),
        peak_after_first,
        "the scratch peak grew across switches, so something is retained between covers"
    );
    assert_eq!(
        ledger.created(),
        u64::from(SWITCHES) * SCRATCH_PER_PREPARATION,
        "each preparation must allocate the same fixed scratch set"
    );
    assert_eq!(
        ledger.already_freed(),
        ledger.created(),
        "every allocated scratch bitmap must be freed exactly once"
    );
}

#[test]
fn the_scratch_peak_stays_under_its_budget() {
    // The documented peak is the padded canvas plus the artwork square, and
    // the square is 400 or 800 px depending on the framed probe. Asserting
    // against `prepared.inner` keeps the bound tied to the cover actually
    // measured rather than to one of the two branches.
    let mut ledger = BitmapLedger::default();
    let prepared = prepare_artwork_tracked(&cover(0), &mut ledger);
    let canvas = 1_160u64 * 1_160 * 4;
    let square = u64::from(prepared.inner) * u64::from(prepared.inner) * 4;
    assert_eq!(
        ledger.peak_bytes(),
        canvas + square,
        "peak should be the padded canvas plus the {}px artwork square",
        prepared.inner
    );
    assert!(ledger.peak_bytes() < SCRATCH_PEAK_BUDGET);
}

#[test]
fn a_full_bleed_cover_holds_the_larger_scratch_square() {
    // The framed branch composes a 400px square, the full-bleed branch an 800px
    // one. Only this test reaches the 800px peak, which is the number the
    // budget in `SCRATCH_PEAK_BUDGET` is set against.
    let mut ledger = BitmapLedger::default();
    let prepared = prepare_artwork_tracked(&full_bleed_cover(), &mut ledger);

    assert!(!prepared.framed, "a split border cannot read as framed");
    assert_eq!(prepared.inner, 800);

    let canvas = 1_160u64 * 1_160 * 4;
    let square = 800u64 * 800 * 4;
    assert_eq!(
        ledger.peak_bytes(),
        canvas + square,
        "peak should be the padded canvas plus the 800px artwork square"
    );
    assert!(
        ledger.peak_bytes() < SCRATCH_PEAK_BUDGET,
        "the largest branch must still fit the budget"
    );
    assert_eq!(ledger.live(), 0, "the full-bleed pass left scratch behind");
}

#[test]
fn a_torn_down_renderer_can_be_replaced() {
    // Dropping the renderer runs `clear()`; a second device must come up with
    // no scene state inherited from the first. This is the observable half of
    // "device/window destroy releases textures": nothing survives the drop
    // that would keep the next renderer from starting empty.
    let mut first = BackgroundRenderer::new_own().expect("a D3D11 device");
    first.resize(640, 360).expect("present target");
    let sampling = Sampling::default();
    let mut frame = 0u64;
    first
        .set_artwork(&prepare_artwork_tracked(&cover(3), &mut BitmapLedger::default()))
        .expect("set artwork");
    warm_up(&mut first, &mut frame, sampling);
    assert!(first.is_ready());
    drop(first);

    let mut second = BackgroundRenderer::new_own().expect("a second D3D11 device");
    second.resize(640, 360).expect("present target");
    assert_eq!(second.live_textures(), 0, "a new renderer starts empty");
    assert!(!second.is_ready());
}
