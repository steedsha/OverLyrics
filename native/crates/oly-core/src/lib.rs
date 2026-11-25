//! Playback clock, timeline mapping, scroll model, and cache accounting.
//!
//! Task 5 ports the legacy `ProgressClock` and the adapter's transport state
//! here; Task 6 the spring scroll model; Task 7/9 the texture caches. Until
//! then this crate holds the timeline that decides which rows are playing and
//! which are merely still lit, plus the shared unit-safety surface so
//! downstream crates can be wired up.

#![forbid(unsafe_code)]

pub mod budget;
pub mod cache;
pub mod clock;
pub mod config;
pub mod easing;
pub mod interlude;
pub mod mapping;
pub mod offsets;
pub mod scroll;
pub mod timeline;
pub mod transport;

pub use cache::{
    BitmapLedger, COVER_CACHE_BYTES, COVER_CACHE_ITEMS, CacheStats, TempBitmap, TextureCache,
    cover_cache,
};
pub use clock::{AnchorOutcome, ProgressClock};
pub use easing::BezierEasing;
pub use interlude::{InterludeDots, InterludeSnapshot};
pub use mapping::{
    APPLE_PROFILE, DEFAULT_PROFILE, FRAME_DT_CLAMP_MS, MODE_COLLAPSE_DELAY_MS, SPOTIFY_PROFILE,
    STATUS_OVERRIDE_MS, STATUS_PLAYING, STALL_NOPKT_MS, SyncProfile, is_playing,
    player_display_name, player_offset_key, seek_decision, sync_profile_for,
};
pub use oly_contracts::{Epoch, MediaMs, MonoMs, PlaybackRate};
pub use scroll::{
    AlignAnchor, Browsing, GeometryGeneration, InteractionKind, LayoutFrame, ScrollBounds,
    ScrollEngine, ScrollMetrics, ScrollReturn, ScrollStats, Spring, SpringParams, VisibleRange,
    align_fraction, pos_y_spring_policy, snap_settled,
};
pub use timeline::{
    FocalTarget, FocusController, Interlude, LineWindow, MIN_INTERLUDE_GAP_MS, SeekDetector,
    Timeline, TimelineDiff, TimelineSnapshot,
};
pub use transport::{
    FeedOutcome, FrameTick, LastPacket, TransportClock, TransportSnapshot, packet_is_playing,
};
