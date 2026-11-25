//! Offscreen glow regions, allocated per *visible* area and reused across frames.
//!
//! AMLL draws a held syllable's glow as a `text-shadow`: a blurred white copy of
//! the glyph, behind the glyph, with the radius and alpha the emphasis curve
//! asks for. Reproducing that on the GPU means rendering the glyph silhouette
//! into an offscreen target, blurring it, and adding it back -- so the target has
//! to exist somewhere.
//!
//! The obvious implementation gives every word its own texture and keeps it for
//! the whole song. A four-minute chart has a few hundred words, so that is a few
//! hundred allocations that are invisible for 95% of the playback, and the cost
//! grows with song length rather than with what is on screen. This pool is the
//! other shape: a region is acquired for a *visible* glow and released as soon
//! as the scroll pushes it out, and the same slot is handed back out for the next
//! word.
//!
//! The pool never grows past its byte budget. When the budget is exhausted the
//! caller gets `None` and draws the word without a glow: a missing halo on one
//! nearly-off-screen word is a far better failure than an unbounded allocator on
//! a long chart, and the alternative -- silently evicting a region that the
//! current frame is still reading -- would flicker a visible one.

use std::collections::BTreeMap;

/// Identifies one glow region across frames.
///
/// The key is the region's *identity*, not its geometry: a word that stays on
/// screen keeps the same key while the playhead moves, so its texture is reused
/// rather than reallocated every frame. Two words never share a key, so two
/// simultaneous glows can never land in one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionKey(pub u64);

/// A pool slot. The renderer uses it to look up the offscreen texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GlowSlot(pub u32);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlowPoolStats {
    /// Regions currently holding a texture.
    pub live: u32,
    /// Bytes those textures occupy.
    pub bytes: u64,
    /// Requests served from an existing region with the same key.
    pub reuses: u64,
    /// Slots that were rebuilt for a different key, reusing a released slot.
    pub recycled: u64,
    /// Textures created (one per `new` allocation, never per frame).
    pub allocations: u64,
    /// Regions released because nothing asked for them again.
    pub releases: u64,
    /// Requests refused because the budget was exhausted.
    pub refusals: u64,
}

#[derive(Clone, Copy, Debug)]
struct LiveRegion {
    key: RegionKey,
    width: u32,
    height: u32,
    /// The frame this region was last asked for; anything older is stale.
    last_frame: u64,
}

/// A bounded pool of offscreen glow targets.
#[derive(Debug)]
pub struct GlowPool {
    budget_bytes: u64,
    bytes_per_pixel: u64,
    live: BTreeMap<GlowSlot, LiveRegion>,
    /// Reverse index so a keyed hit is O(log n) instead of a scan.
    by_key: BTreeMap<RegionKey, GlowSlot>,
    free: Vec<GlowSlot>,
    next_slot: u32,
    frame: u64,
    stats: GlowPoolStats,
}

impl GlowPool {
    /// `bytes_per_pixel` is 4 for an RGBA8 target and 8 for an RGBA16F one. The
    /// budget is a hard ceiling on the sum of every live texture, not a
    /// per-region limit: one huge region can legitimately use most of it.
    pub fn new(budget_bytes: u64, bytes_per_pixel: u64) -> Self {
        Self {
            budget_bytes,
            bytes_per_pixel: bytes_per_pixel.max(1),
            live: BTreeMap::new(),
            by_key: BTreeMap::new(),
            free: Vec::new(),
            next_slot: 0,
            frame: 0,
            stats: GlowPoolStats::default(),
        }
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    pub fn stats(&self) -> &GlowPoolStats {
        &self.stats
    }

    /// Start a frame. Regions from the previous frame stay live until
    /// [`Self::end_frame`], so a region that is still visible is reused rather
    /// than torn down and rebuilt.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    fn surface_bytes(&self, width: u32, height: u32) -> u64 {
        u64::from(width) * u64::from(height) * self.bytes_per_pixel
    }

    /// Ask for a target for `key`, growing the slot if the region needs more
    /// room than it had.
    ///
    /// A region that is already live keeps its slot even when it grew, because
    /// the alternative -- releasing and reallocating mid-animation -- would drop
    /// a frame of glow exactly when a long note is at its brightest.
    pub fn acquire(&mut self, key: RegionKey, width: u32, height: u32) -> Option<GlowSlot> {
        let width = width.max(1);
        let height = height.max(1);
        let needed = self.surface_bytes(width, height);

        // A region that was already asked for keeps its slot.
        if let Some(&slot) = self.by_key.get(&key) {
            let before = self
                .live
                .get(&slot)
                .map(|region| self.surface_bytes(region.width, region.height))
                .unwrap_or(0);
            if needed > before && self.stats.bytes - before + needed > self.budget_bytes {
                self.stats.refusals += 1;
                return None;
            }
            if let Some(region) = self.live.get_mut(&slot) {
                region.width = width;
                region.height = height;
                region.last_frame = self.frame;
            }
            self.stats.bytes = self.stats.bytes - before + needed;
            self.stats.reuses += 1;
            return Some(slot);
        }

        // A slot released earlier, or one whose region has not been asked for
        // yet this frame. The second case is what keeps a chart that scrolls by
        // a word every frame from allocating a texture per frame: the old
        // region is provably not in use this frame (every acquire happens
        // before `end_frame`), so its texture can be handed over at once
        // instead of waiting for the sweep.
        let slot = match self.free.pop().or_else(|| self.reclaim_oldest_stale()) {
            Some(slot) => slot,
            None => {
                if self.stats.bytes + needed > self.budget_bytes {
                    self.stats.refusals += 1;
                    return None;
                }
                let slot = GlowSlot(self.next_slot);
                self.next_slot += 1;
                self.install(slot, key, width, height);
                self.stats.bytes += needed;
                self.stats.allocations += 1;
                return Some(slot);
            }
        };

        if self.stats.bytes + needed > self.budget_bytes {
            self.free.push(slot);
            self.stats.refusals += 1;
            return None;
        }
        self.install(slot, key, width, height);
        self.stats.bytes += needed;
        self.stats.recycled += 1;
        Some(slot)
    }

    fn install(&mut self, slot: GlowSlot, key: RegionKey, width: u32, height: u32) {
        self.live.insert(
            slot,
            LiveRegion {
                key,
                width,
                height,
                last_frame: self.frame,
            },
        );
        self.by_key.insert(key, slot);
        self.stats.live = self.live.len() as u32;
    }

    /// Take the least recently used region that has not been touched this frame.
    ///
    /// Returns `None` when every live region has already been asked for in this
    /// frame: those are on screen and must not be stolen.
    fn reclaim_oldest_stale(&mut self) -> Option<GlowSlot> {
        let victim = self
            .live
            .iter()
            .filter(|(_, region)| region.last_frame < self.frame)
            .min_by_key(|(_, region)| region.last_frame)
            .map(|(&slot, region)| (slot, region.key, region.width, region.height))?;
        let (slot, key, width, height) = victim;
        self.live.remove(&slot);
        self.by_key.remove(&key);
        self.stats.bytes -= self.surface_bytes(width, height);
        self.stats.releases += 1;
        self.stats.live = self.live.len() as u32;
        Some(slot)
    }

    /// The size a live slot currently has, for the renderer's viewport.
    pub fn size(&self, slot: GlowSlot) -> Option<(u32, u32)> {
        self.live.get(&slot).map(|r| (r.width, r.height))
    }

    /// Drop every region that was not asked for this frame.
    ///
    /// Called once per frame *after* all visible words have acquired their
    /// regions. Releasing here rather than on a timer is what keeps the pool
    /// proportional to the visible set: a word that scrolled off stops being
    /// asked for and loses its texture at the end of that frame.
    pub fn end_frame(&mut self) {
        let frame = self.frame;
        let stale: Vec<GlowSlot> = self
            .live
            .iter()
            .filter(|(_, region)| region.last_frame < frame)
            .map(|(&slot, _)| slot)
            .collect();
        for slot in stale {
            if let Some(region) = self.live.remove(&slot) {
                self.by_key.remove(&region.key);
                self.stats.bytes -= self.surface_bytes(region.width, region.height);
                self.free.push(slot);
                self.stats.releases += 1;
            }
        }
        self.stats.live = self.live.len() as u32;
    }

    /// Release everything, for a document change or a device loss.
    pub fn clear(&mut self) {
        self.live.clear();
        self.by_key.clear();
        self.free.clear();
        self.next_slot = 0;
        self.stats = GlowPoolStats::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_song_only_allocates_what_is_visible() {
        // Twelve words on screen, a new set every frame (the chart scrolls).
        // The pool must mint twelve textures once, not twelve per frame.
        let mut pool = GlowPool::new(8 * 1024 * 1024, 4);
        let visible = 12;
        for frame in 0..40u64 {
            pool.begin_frame();
            for i in 0..visible {
                let key = RegionKey(frame * 1000 + i as u64);
                assert!(
                    pool.acquire(key, 256, 64).is_some(),
                    "frame {frame} word {i}"
                );
            }
            pool.end_frame();
            assert_eq!(pool.stats().live, visible, "frame {frame}");
        }
        assert_eq!(
            pool.stats().live,
            visible,
            "the live count follows visibility, not chart length"
        );
        assert_eq!(
            pool.stats().allocations,
            visible as u64,
            "each slot is minted once and then handed to the next word: {}",
            pool.stats().allocations
        );
    }

    #[test]
    fn a_stationary_word_reuses_its_slot_across_frames() {
        let mut pool = GlowPool::new(1024 * 1024, 4);
        pool.begin_frame();
        let first = pool.acquire(RegionKey(7), 128, 32).expect("first frame");
        pool.end_frame();

        pool.begin_frame();
        let second = pool.acquire(RegionKey(7), 128, 32).expect("second frame");
        pool.end_frame();

        assert_eq!(first, second, "the same region must keep its slot");
        assert_eq!(pool.stats().allocations, 1, "one texture, not one per frame");
        assert!(pool.stats().reuses >= 1);
    }

    #[test]
    fn a_scrolled_out_region_is_released_and_reused() {
        let mut pool = GlowPool::new(1024 * 1024, 4);
        pool.begin_frame();
        let old = pool.acquire(RegionKey(1), 128, 32).expect("visible");
        pool.end_frame();

        // Next frame the word is gone and a different one is visible. The old
        // slot is reclaimed inside this frame, not one frame later.
        pool.begin_frame();
        let new = pool.acquire(RegionKey(2), 128, 32).expect("visible");
        pool.end_frame();

        assert_eq!(old, new, "the released slot is handed to the new region");
        assert_eq!(pool.stats().live, 1);
        assert_eq!(pool.stats().allocations, 1, "no second texture was created");
        assert!(pool.stats().recycled >= 1);
    }

    #[test]
    fn the_pool_never_exceeds_its_budget() {
        // 64 KiB at 4 bytes per pixel holds exactly 16384 pixels: sixteen
        // 32x32 regions. The seventeenth request must be refused, not served.
        let mut pool = GlowPool::new(64 * 1024, 4);
        pool.begin_frame();
        for i in 0..16u64 {
            assert!(pool.acquire(RegionKey(i), 32, 32).is_some(), "region {i}");
        }
        assert!(
            pool.acquire(RegionKey(99), 32, 32).is_none(),
            "the budget is a ceiling, not a hint"
        );
        assert_eq!(pool.stats().bytes, 64 * 1024);
        assert_eq!(pool.stats().refusals, 1);
        assert!(
            pool.stats().bytes <= pool.budget_bytes(),
            "accounting must never overshoot"
        );
    }

    #[test]
    fn a_region_that_grows_is_reallocated_in_place() {
        let mut pool = GlowPool::new(1024 * 1024, 4);
        pool.begin_frame();
        let slot = pool.acquire(RegionKey(5), 64, 32).expect("small");
        pool.end_frame();
        assert_eq!(pool.size(slot), Some((64, 32)));

        // The same word is now drawn at a larger size (font change): the slot
        // survives, the accounting grows.
        pool.begin_frame();
        assert_eq!(pool.acquire(RegionKey(5), 128, 64), Some(slot));
        pool.end_frame();
        assert_eq!(pool.size(slot), Some((128, 64)));
        assert_eq!(pool.stats().allocations, 1);
        assert_eq!(pool.stats().bytes, 128 * 64 * 4);
    }

    #[test]
    fn two_simultaneous_glows_never_share_a_slot() {
        let mut pool = GlowPool::new(1024 * 1024, 4);
        pool.begin_frame();
        let a = pool.acquire(RegionKey(1), 64, 64).expect("first glow");
        let b = pool.acquire(RegionKey(2), 64, 64).expect("second glow");
        pool.end_frame();
        assert_ne!(a, b, "two visible glows must not alias one texture");
        assert_eq!(pool.stats().live, 2);
        assert_eq!(pool.stats().bytes, 2 * 64 * 64 * 4);
    }

    #[test]
    fn a_visible_glow_is_never_reclaimed_by_a_newcomer() {
        // Twelve regions is the budget exactly. A thirteenth word asking for a
        // slot in the same frame must be refused rather than stealing one of
        // the twelve that are on screen.
        let mut pool = GlowPool::new(12 * 32 * 32 * 4, 4);
        pool.begin_frame();
        for i in 0..12u64 {
            assert!(pool.acquire(RegionKey(i), 32, 32).is_some(), "region {i}");
        }
        assert!(
            pool.acquire(RegionKey(100), 32, 32).is_none(),
            "a live region is not a candidate for reclamation"
        );
        assert_eq!(pool.stats().live, 12);
    }

    #[test]
    fn releasing_everything_returns_to_the_zero_state() {
        let mut pool = GlowPool::new(1024 * 1024, 4);
        pool.begin_frame();
        pool.acquire(RegionKey(1), 64, 64);
        pool.acquire(RegionKey(2), 64, 64);
        pool.end_frame();
        assert!(pool.stats().bytes > 0);

        pool.clear();
        assert_eq!(pool.stats().live, 0);
        assert_eq!(pool.stats().bytes, 0);
        assert_eq!(pool.size(GlowSlot(0)), None);
    }
}
