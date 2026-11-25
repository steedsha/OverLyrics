//! Behavioural tests for the scroll model.
//!
//! `scroll_parity.rs` proves the port reproduces the recorded trace. This file
//! covers the situations Task 6 names that a short trace cannot: a long burst
//! of wheel events in both directions, a thumb dragged across the whole song,
//! an interaction held far past the idle timeout, and the guarantee that a row
//! which has been recycled never receives another write.

use oly_core::scroll::{
    AlignAnchor, Browsing, GeometryGeneration, LayoutFrame, ScrollBounds, ScrollEngine, ScrollReturn,
    VisibleRange, align_fraction, snap_settled,
};

const CONTAINER: f64 = 800.0;

fn engine_with_document(total_height: f64) -> ScrollEngine {
    let mut engine = ScrollEngine::new();
    // A tall document: the focal row sits a third of the way down the content,
    // exactly where a mid-song line would be.
    let focal_top = total_height / 3.0;
    let bounds = ScrollBounds::compute(&LayoutFrame {
        focal_top,
        target_height: 120.0,
        total_height,
        interlude_height: 0.0,
        container_height: CONTAINER,
        align_position: 0.28,
        align_anchor: AlignAnchor::Top,
    });
    engine.update_boundary(bounds.min, bounds.max);
    engine
}

#[test]
fn rapid_bidirectional_wheeling_stays_inside_the_bounds_and_keeps_the_sum() {
    let mut engine = engine_with_document(20_000.0);
    let mut now = 0.0;
    // Forty alternating notches, the way a user scrubs back and forth looking
    // for a line. Each notch is inside the wheel debounce, so this is one
    // gesture and must not announce itself forty times.
    let mut expected = 0.0;
    for step in 0..40 {
        let delta = if step % 3 == 0 { -260.0 } else { 180.0 };
        now += 12.0;
        engine.wheel(delta, true, now);
        expected = (expected + delta).clamp(engine.min_offset(), engine.max_offset());
        assert_eq!(engine.offset(), expected, "notch {step}");
        assert!(
            engine.offset() >= engine.min_offset() && engine.offset() <= engine.max_offset(),
            "notch {step} escaped the bounds"
        );
    }
    assert_eq!(
        engine.stats().start_events,
        1,
        "a burst inside the debounce is one gesture, not forty"
    );
    assert_eq!(engine.stats().wheel_events, 40);
}

#[test]
fn a_thumb_drag_across_the_whole_document_tracks_the_pointer_exactly() {
    let mut engine = engine_with_document(20_000.0);
    let lo = engine.min_offset();
    let hi = engine.max_offset();
    assert!(lo < 0.0 && hi > 0.0, "a mid-song document reaches both ways");
    // Start at the top of the song: `lo` is the most negative offset, which is
    // what `metrics().p` reports as 0. Drag to `hi` in eight steps. The
    // reference dispatches a synthetic wheel for a thumb drag precisely so the
    // move stays one-to-one; a spring here would make the thumb lag the pointer.
    engine.reset(lo);
    assert!((engine.metrics().p - 0.0).abs() < 1e-12);
    for step in 1..=8 {
        let target = lo + (hi - lo) * step as f64 / 8.0;
        let moved = engine.scroll_by(-(target - engine.offset()));
        assert_eq!(
            moved, target,
            "thumb step {step} did not land where the pointer went"
        );
    }
    assert_eq!(engine.offset(), hi);
    let metrics = engine.metrics();
    assert!(metrics.has_overflow);
    assert!((metrics.p - 1.0).abs() < 1e-12, "thumb reached the end");
}

#[test]
fn a_held_thumb_never_times_out_however_long_the_user_holds_it() {
    let mut browsing = Browsing::default();
    browsing.set_interaction_active(true, 0.0);
    assert!(browsing.is_active());
    assert_eq!(
        browsing.idle_deadline(),
        None,
        "a held thumb disables the idle deadline entirely"
    );
    // Ten minutes of holding still.
    assert!(!browsing.expire(600_000.0));
    assert!(browsing.is_active());
}

#[test]
fn the_idle_timeout_starts_when_the_thumb_is_released_not_when_it_was_pressed() {
    let mut browsing = Browsing::default();
    browsing.set_interaction_active(true, 0.0);
    browsing.set_interaction_active(false, 1_000.0);
    assert_eq!(browsing.idle_deadline(), Some(5_000.0));
    assert!(!browsing.expire(4_999.0));
    assert!(browsing.expire(5_000.0));
    assert!(!browsing.expire(5_001.0), "expiry must be one-shot");
}

#[test]
fn the_return_lands_on_the_row_where_it_actually_is_now_not_where_it_was() {
    // The playing row advances while the return runs. The reference re-measures
    // the baseline and restarts the eased distance, so the row must not be
    // dragged back to a position it no longer occupies.
    let mut ret = ScrollReturn::new(200.0, 0.0, 0.0);
    assert!(!ret.update(300.0, 0.0));
    let half_way = ret.offset;
    assert!(half_way > 0.0 && half_way < 200.0);

    // Now the row has moved 50 px up under the return.
    assert!(!ret.update(320.0, 50.0), "re-basing must not finish it");
    assert!(ret.from > half_way, "from must grow with the baseline");

    let mut now = 320.0;
    let mut frames = 0;
    while !ret.update(now, 50.0) {
        now += 1.0;
        frames += 1;
        assert!(frames < 1_000, "return never finished");
    }
    assert_eq!(ret.offset, 0.0);
    assert!(frames > 100, "the remaining distance was eased, not jumped");
}

#[test]
fn a_row_outside_the_draw_window_is_not_drawn_but_a_tall_row_is_never_trimmed() {
    // Row 0 is a held syllable spanning 3 000 px; rows 1.. are normal.
    let mut heights = vec![60.0; 200];
    heights[0] = 3_000.0;
    let mut tops = vec![0.0];
    for index in 1..200 {
        tops.push(tops[index - 1] + heights[index - 1]);
    }

    // Scrolled deep into the song: row 0 is far above the viewport top but its
    // bottom is still inside the buffer, so it must stay drawn (its mask is
    // still animating).
    let offset = 2_500.0;
    let range = VisibleRange::compute(&tops, &heights, offset, CONTAINER, 120.0);
    assert_eq!(range.first, 0, "the held syllable must not be trimmed");

    // A row far below the buffer is not drawn at all.
    assert!(
        range.last < 150,
        "row {} should not be in the draw window",
        range.last
    );
    let drawn = range.len();
    assert!(
        drawn < 80,
        "virtualisation drew {drawn} of 200 rows; the buffer is too large"
    );
}

#[test]
fn a_recycled_row_cannot_receive_a_write_from_an_older_measurement() {
    let first = GeometryGeneration::default();
    let second = first.next();
    // The layout changed (weight, wrap mode, translation) and the old
    // measurement came back late: it must be dropped rather than applied to
    // whichever row now occupies that slot.
    assert!(!second.accepts(first));
    assert!(second.accepts(second));
    assert_eq!(first.next(), second);
    assert_ne!(second.next(), second);
}

#[test]
fn the_resting_offset_puts_the_focal_row_on_the_anchor_line() {
    let frame = LayoutFrame {
        focal_top: 500.0,
        target_height: 120.0,
        total_height: 4_000.0,
        interlude_height: 0.0,
        container_height: CONTAINER,
        align_position: 0.28,
        align_anchor: AlignAnchor::Top,
    };
    let bounds = ScrollBounds::compute(&frame);
    // `min` is the far reach: the first row pulled all the way to the anchor.
    assert_eq!(bounds.min, -500.0);
    // The resting offset is zero, not `min`. The focal row is already placed on
    // the anchor by `basePosWithoutScroll`; the scroll offset only carries the
    // user's displacement on top of that. Asserting `resting == min` would
    // claim the view rests scrolled to the first line of the song.
    assert_eq!(bounds.resting, 0.0);
    assert!(bounds.resting >= bounds.min && bounds.resting <= bounds.max);
}

#[test]
fn the_anchor_fraction_follows_the_morphology() {
    // window morphology passes an explicit playingLineTopOffset of 28 %.
    assert!(
        (align_fraction(Some(28.0), None, None).unwrap() - 0.28).abs() < 1e-6,
        "an explicit top offset wins"
    );
    // Anchor rows are centres; the top edge is half a row above.
    assert!(
        (align_fraction(None, Some(1.5), Some(12.0)).unwrap() - 0.083_333_333).abs() < 1e-6
    );
    // No anchor rows at all: the in-mode default of 42 %, never zero.
    assert_eq!(align_fraction(None, None, Some(12.0)), Some(0.42));
    // Anchor rows but no measurable container: refuse rather than guess.
    assert_eq!(align_fraction(None, Some(1.5), None), None);
}

#[test]
fn only_a_settled_row_is_snapped_to_the_device_grid() {
    // Snapping a moving row quantises its trajectory; at 239 Hz that reads as
    // judder, which is exactly the artefact this guard avoids.
    assert_eq!(snap_settled(10.4, 1.0), 10.0);
    // 10.4 * 1.5 = 15.6 device px, which rounds to 16, and 16 / 1.5 = 10.6667.
    assert!((snap_settled(10.4, 1.5) - 10.666_666_666_666_666).abs() < 1e-9);
    assert_eq!(snap_settled(-0.4, 1.0), -0.0);
    // A zero scale factor is a bug upstream; dividing by it would produce NaN.
    assert_eq!(snap_settled(10.4, 0.0), 10.4);
}
