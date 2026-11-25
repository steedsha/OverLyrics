//! Bounded caches and temporary-bitmap accounting.
//!
//! Two different budgets live here and they must never be conflated:
//!
//! * [`CoverCache`] bounds the *encoded* cover images (the legacy limit is 64
//!   entries and 24 MiB). It says nothing about decoded pixels or GPU textures.
//! * [`TextureCache`] bounds *GPU texture bytes* keyed by a content signature.
//!   Task 9's video decoder and Task 10's scene renderer take a budget from
//!   here rather than inventing a second accounting scheme.
//!
//! [`BitmapLedger`] exists so a preparation pass can prove it released every
//! CPU bitmap it allocated: the background path decodes, pads, blurs and
//! downsamples a cover, and a missed `free` there is exactly the kind of
//! "accumulated temporary bitmap" the acceptance plan forbids.

use std::collections::BTreeMap;

/// Legacy cover-cache limits, kept identical to the shipped application.
pub const COVER_CACHE_ITEMS: usize = 64;
pub const COVER_CACHE_BYTES: u64 = 24 * 1024 * 1024;

/// Maximum number of decoded-but-unpresented video samples the Task 9
/// decoder may hold. The plan sets the initial target at ≤3; a 240 Hz
/// display must never drive 240 pending frames.
pub const VIDEO_MAX_PENDING_SAMPLES: usize = 3;

/// Size of one 1080x1080 NV12 frame in bytes (Y plane + packed UV plane).
pub const VIDEO_NV12_BYTES_1080P: u64 = 1080 * 1080 * 3 / 2;

/// NV12 is planar Y followed by a packed interleaved UV plane at half
/// resolution: `w*h + (w/2)*(h/2)*2`.
pub fn video_nv12_bytes(width: u32, height: u32) -> u64 {
    let w = width as u64;
    let h = height as u64;
    w * h + (w / 2) * (h / 2) * 2
}

/// The pending-sample byte budget for a video of this size: the queue bound
/// expressed in NV12 bytes. Task 10 wires this into the scene renderer's
/// allocation; the decoder charges exactly this against [`TextureCache`].
pub fn video_sample_budget(width: u32, height: u32) -> u64 {
    video_nv12_bytes(width, height) * VIDEO_MAX_PENDING_SAMPLES as u64
}

/// Counters shared by every cache in this module.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub items: usize,
    pub bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub insertions: u64,
    /// Insertions refused because a single entry exceeded the whole byte
    /// budget. An insert that cannot evict anything (every resident entry
    /// pinned) is not refused — it leaves the cache observably over budget.
    pub refusals: u64,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    bytes: u64,
    last_used: u64,
    pins: u32,
}

/// A byte- and item-bounded LRU cache.
///
/// Pinning keeps an entry resident across a frame even when it is the least
/// recently used one: a glyph tile or a video frame that the current frame is
/// about to sample must not disappear from under the draw call.
#[derive(Debug, Default)]
pub struct TextureCache {
    max_items: usize,
    max_bytes: u64,
    used_bytes: u64,
    clock: u64,
    entries: BTreeMap<u64, Entry>,
    stats: CacheStats,
}

impl TextureCache {
    /// `max_items == 0` means "no item limit"; `max_bytes == 0` means "no byte
    /// limit". A cache with both at zero is therefore unbounded, which only
    /// makes sense for a table the caller sizes by construction.
    pub fn new(max_items: usize, max_bytes: u64) -> Self {
        Self {
            max_items,
            max_bytes,
            ..Self::default()
        }
    }

    pub fn stats(&self) -> CacheStats {
        let mut stats = self.stats;
        stats.items = self.entries.len();
        stats.bytes = self.used_bytes;
        stats
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub fn contains(&self, key: u64) -> bool {
        self.entries.contains_key(&key)
    }

    /// Records a hit and marks the entry as most recently used. Returns the
    /// stored size so the caller can rebuild its own bookkeeping.
    pub fn touch(&mut self, key: u64) -> Option<u64> {
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = self.clock;
            self.stats.hits += 1;
            Some(entry.bytes)
        } else {
            self.stats.misses += 1;
            None
        }
    }

    pub fn pin(&mut self, key: u64) -> bool {
        match self.entries.get_mut(&key) {
            Some(entry) => {
                entry.pins += 1;
                true
            }
            None => false,
        }
    }

    pub fn unpin(&mut self, key: u64) -> bool {
        match self.entries.get_mut(&key) {
            Some(entry) => {
                entry.pins = entry.pins.saturating_sub(1);
                true
            }
            None => false,
        }
    }

    /// Inserts `bytes` under `key`, evicting unpinned LRU entries until the
    /// budget fits. An existing key is replaced in place.
    pub fn insert(&mut self, key: u64, bytes: u64) -> bool {
        self.clock += 1;
        if let Some(previous) = self.entries.remove(&key) {
            self.used_bytes -= previous.bytes;
        }
        if self.max_bytes > 0 && bytes > self.max_bytes {
            self.stats.refusals += 1;
            return false;
        }
        self.used_bytes += bytes;
        self.entries.insert(
            key,
            Entry {
                bytes,
                last_used: self.clock,
                pins: 0,
            },
        );
        self.stats.insertions += 1;
        // The newcomer must not be a victim of its own eviction pass: when
        // every other entry is pinned, the only unpinned candidate would be
        // the entry we just inserted, and the caller could never observe the
        // overflow. Pin it across the pass, then release. An all-pinned cache
        // therefore stays over budget *with* the newcomer present — visible
        // in `stats()` rather than silently dropped.
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.pins += 1;
        }
        self.evict_to_budget();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.pins = entry.pins.saturating_sub(1);
        }
        self.entries.contains_key(&key)
    }

    /// Drops unpinned entries, least recently used first, until both limits
    /// hold. Pinned entries are never touched, so an over-budget cache with
    /// pinned entries stays over budget until they are unpinned.
    pub fn evict_to_budget(&mut self) {
        loop {
            let over_items = self.max_items > 0 && self.entries.len() > self.max_items;
            let over_bytes = self.max_bytes > 0 && self.used_bytes > self.max_bytes;
            if !over_items && !over_bytes {
                return;
            }
            let victim = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.pins == 0)
                .min_by_key(|(key, entry)| (entry.last_used, **key))
                .map(|(key, _)| *key);
            match victim {
                Some(key) => {
                    if let Some(entry) = self.entries.remove(&key) {
                        self.used_bytes -= entry.bytes;
                        self.stats.evictions += 1;
                    }
                }
                None => return,
            }
        }
    }

    pub fn remove(&mut self, key: u64) -> Option<u64> {
        let entry = self.entries.remove(&key)?;
        self.used_bytes -= entry.bytes;
        Some(entry.bytes)
    }

    /// Drops every entry, pinned or not. Used at teardown; the returned byte
    /// count is what the caller had outstanding.
    pub fn clear(&mut self) -> u64 {
        let bytes = self.used_bytes;
        self.entries.clear();
        self.used_bytes = 0;
        bytes
    }
}

/// The encoded-cover cache with the shipped limits.
pub fn cover_cache() -> TextureCache {
    TextureCache::new(COVER_CACHE_ITEMS, COVER_CACHE_BYTES)
}

/// Handle to one ledger allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TempBitmap(pub u64);

/// Counts live CPU bitmaps and their bytes.
///
/// The renderer must not retain decoded covers or intermediate blur canvases
/// between frames; this ledger is how the background tests assert that.
#[derive(Debug, Default)]
pub struct BitmapLedger {
    next: u64,
    open: BTreeMap<u64, u64>,
    live_bytes: u64,
    peak_count: usize,
    peak_bytes: u64,
    created: u64,
    freed: u64,
}

impl BitmapLedger {
    pub fn alloc(&mut self, bytes: u64) -> TempBitmap {
        self.next += 1;
        self.open.insert(self.next, bytes);
        self.live_bytes += bytes;
        self.peak_count = self.peak_count.max(self.open.len());
        self.peak_bytes = self.peak_bytes.max(self.live_bytes);
        self.created += 1;
        TempBitmap(self.next)
    }

    /// Returns false when the handle was already freed, so a double free is
    /// visible instead of silently shrinking the counter twice.
    pub fn free(&mut self, handle: TempBitmap) -> bool {
        match self.open.remove(&handle.0) {
            Some(bytes) => {
                self.live_bytes -= bytes;
                self.freed += 1;
                true
            }
            None => false,
        }
    }

    pub fn live(&self) -> usize {
        self.open.len()
    }

    pub fn live_bytes(&self) -> u64 {
        self.live_bytes
    }

    pub fn peak(&self) -> usize {
        self.peak_count
    }

    pub fn peak_bytes(&self) -> u64 {
        self.peak_bytes
    }

    pub fn created(&self) -> u64 {
        self.created
    }

    pub fn already_freed(&self) -> u64 {
        self.freed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cover_cache_keeps_the_legacy_limits() {
        let cache = cover_cache();
        assert_eq!(cache.max_items, 64);
        assert_eq!(cache.max_bytes, 24 * 1024 * 1024);
    }

    #[test]
    fn the_least_recently_used_entry_is_evicted_first() {
        let mut cache = TextureCache::new(4, 0);
        for key in 0..4 {
            assert!(cache.insert(key, 10));
        }
        assert!(cache.touch(0).is_some());
        assert!(cache.insert(4, 10));
        assert!(!cache.contains(1), "the untouched oldest entry should go");
        assert!(cache.contains(0));
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn pinned_entries_survive_a_newcomer_and_the_overflow_is_visible() {
        let mut cache = TextureCache::new(2, 0);
        cache.insert(1, 10);
        cache.insert(2, 10);
        assert!(cache.pin(1));
        assert!(cache.pin(2));
        assert!(cache.insert(3, 10));
        assert_eq!(cache.len(), 3, "nothing unpinned was available to drop");
        assert_eq!(cache.used_bytes(), 30);
        assert!(cache.unpin(1));
        assert!(cache.unpin(2));
        cache.evict_to_budget();
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(3));
    }

    #[test]
    fn an_entry_larger_than_the_whole_budget_is_refused() {
        let mut cache = TextureCache::new(0, 100);
        assert!(!cache.insert(1, 101));
        assert_eq!(cache.used_bytes(), 0);
        assert_eq!(cache.stats().refusals, 1);
    }

    #[test]
    fn replacing_a_key_does_not_double_count_its_bytes() {
        let mut cache = TextureCache::new(0, 1000);
        cache.insert(7, 100);
        cache.insert(7, 40);
        assert_eq!(cache.used_bytes(), 40);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn clearing_returns_the_outstanding_byte_count() {
        let mut cache = TextureCache::new(0, 1000);
        cache.insert(1, 10);
        cache.insert(2, 20);
        assert_eq!(cache.clear(), 30);
        assert_eq!(cache.used_bytes(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn the_bitmap_ledger_tracks_peak_and_returns_to_zero() {
        let mut ledger = BitmapLedger::default();
        let a = ledger.alloc(100);
        let b = ledger.alloc(200);
        assert_eq!(ledger.live(), 2);
        assert_eq!(ledger.peak_bytes(), 300);
        assert!(ledger.free(a));
        assert!(!ledger.free(a), "a double free must not count twice");
        assert!(ledger.free(b));
        assert_eq!(ledger.live(), 0);
        assert_eq!(ledger.live_bytes(), 0);
        assert_eq!(ledger.created(), 2);
        assert_eq!(ledger.already_freed(), 2);
    }
}
