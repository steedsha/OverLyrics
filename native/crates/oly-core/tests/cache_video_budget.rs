//! Task 9: the video decode path takes its budget from `oly_core::cache`, not a
//! second accounting scheme. The pending-sample queue is bounded at 3 and the
//! NV12 frame size is derived from the texture dimensions, so the decoder can
//! charge exactly what it holds.
//!
//! Red state: the constants and helpers do not exist yet.

use oly_core::cache::{
    VIDEO_MAX_PENDING_SAMPLES, VIDEO_NV12_BYTES_1080P, video_nv12_bytes, video_sample_budget,
};

#[test]
fn pending_sample_queue_is_bounded_at_three() {
    assert_eq!(VIDEO_MAX_PENDING_SAMPLES, 3);
}

#[test]
fn nv12_bytes_are_planar_y_plus_packed_uv() {
    // 1080x1080 NV12: Y plane 1080*1080, UV plane 1080*540 rows of 1080 bytes.
    assert_eq!(VIDEO_NV12_BYTES_1080P, 1080 * 1080 * 3 / 2);
}

#[test]
fn nv12_size_formula_matches_the_1080_constant() {
    assert_eq!(video_nv12_bytes(1080, 1080), VIDEO_NV12_BYTES_1080P);
    // Height must include the chroma rows: 100x100 NV12 is Y 100*100 + UV 50*100.
    assert_eq!(video_nv12_bytes(100, 100), 15_000);
    assert_eq!(video_nv12_bytes(2, 2), 6);
}

#[test]
fn sample_budget_is_the_queue_bound_in_frame_bytes() {
    assert_eq!(video_sample_budget(1080, 1080), 3 * 1080 * 1080 * 3 / 2);
}
