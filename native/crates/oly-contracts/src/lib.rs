//! Shared, UI-free data contracts between the legacy truth exporters, the
//! engine, and the renderer.
//!
//! Units are part of the contract: [`MonoMs`] is a monotonic millisecond
//! instant, [`MediaMs`] is a media-timeline millisecond position, and
//! [`Epoch`] is a wall-clock millisecond timestamp. Mixing them is a bug, so
//! they are distinct newtypes rather than bare integers.

#![forbid(unsafe_code)]

use std::fmt;

pub mod chunks;
pub mod config;
pub mod lyrics;
pub mod optimize;
pub mod state;

pub use chunks::{ChunkAtom, WordChunk, chunk_and_split_lyric_words, is_chunker_cjk};
pub use config::{AI_PRESETS, AiModel, AiPreset, TranslationSettings, translation_settings};
pub use optimize::{OptimizeOptions, optimize_lyric_lines};
pub use state::{ClockState, PlayerPacket, PlaybackStatus, TransportControls, is_idle_track_title};

pub use lyrics::{
    AmllLine, AmllWord, BgField, LineDisplayOptions, LyricDoc, LyricLine, Word, credit_names,
    detect_space_convention, doc_to_lines, gaps_from_text, gaps_latin, is_cjk,
    line_breaks_between_chars,
};

macro_rules! scalar_ms {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(pub i64);

        impl $name {
            pub const ZERO: Self = Self(0);

            pub const fn from_millis(value: i64) -> Self {
                Self(value)
            }

            pub const fn millis(self) -> i64 {
                self.0
            }

            pub fn max(self, other: Self) -> Self {
                Self(self.0.max(other.0))
            }

            pub fn min(self, other: Self) -> Self {
                Self(self.0.min(other.0))
            }

            pub fn saturating_add(self, rhs: i64) -> Self {
                Self(self.0.saturating_add(rhs))
            }

            pub fn saturating_sub(self, rhs: i64) -> Self {
                Self(self.0.saturating_sub(rhs))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({}ms)", stringify!($name), self.0)
            }
        }

        // Task 9: hiding/restore boundaries are expressed as
        // `hide_time + RELEASE_AFTER_MS`, so the millisecond units support
        // adding a plain millisecond count. Subtraction stays explicit
        // (`saturating_sub`) because a media position has no natural negative.
        impl core::ops::Add<i64> for $name {
            type Output = Self;

            fn add(self, rhs: i64) -> Self {
                Self(self.0 + rhs)
            }
        }
    };
}

scalar_ms!(MonoMs, "A monotonic-clock instant in milliseconds.");
scalar_ms!(MediaMs, "A position on a media timeline in milliseconds.");
scalar_ms!(Epoch, "A wall-clock (Unix epoch) timestamp in milliseconds.");

/// Media playback rate multiplier. Always strictly positive after
/// [`PlaybackRate::sanitize`].
#[derive(Copy, Clone, PartialEq)]
pub struct PlaybackRate(pub f32);

impl PlaybackRate {
    pub const NORMAL: Self = Self(1.0);

    /// The legacy clock treated any non-positive rate as 1.0 and never
    /// extrapolated with a zero rate.
    pub fn sanitize(raw: f32) -> Self {
        if raw.is_finite() && raw > 0.0 {
            Self(raw)
        } else {
            Self::NORMAL
        }
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

impl Default for PlaybackRate {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl fmt::Debug for PlaybackRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PlaybackRate({}x)", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_do_not_mix_implicitly() {
        // Compile-time proof: the three millisecond units are distinct types,
        // so a `MediaMs` cannot be passed where a `MonoMs` is expected.
        let mono = MonoMs::from_millis(10);
        let media = MediaMs::from_millis(20);
        let epoch = Epoch::from_millis(30);
        assert_eq!(mono.millis() + media.millis() + epoch.millis(), 60);
    }

    #[test]
    fn playback_rate_rejects_non_positive_and_non_finite() {
        assert_eq!(PlaybackRate::sanitize(0.0).get(), 1.0);
        assert_eq!(PlaybackRate::sanitize(-2.0).get(), 1.0);
        assert_eq!(PlaybackRate::sanitize(f32::NAN).get(), 1.0);
        assert_eq!(PlaybackRate::sanitize(1.5).get(), 1.5);
    }
}
