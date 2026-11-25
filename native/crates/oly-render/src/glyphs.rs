//! Glyph atlas bookkeeping: what a rasterized glyph is keyed by, how much it
//! costs, and which tiles may be reused this frame.
//!
//! The atlas never rasterizes anything itself. It owns the accounting rules the
//! plan requires: a tile is keyed by the full rendering configuration, the CPU
//! and GPU copies are counted separately, eviction is LRU over *unpinned* tiles
//! only, and total bytes stay under a caller-set budget no matter how many
//! songs, fonts or sizes a session touches.

use std::collections::HashMap;

/// Everything that changes a glyph's rasterized pixels.
///
/// Two tiles may only be shared when every field matches. `mode` separates the
/// monochrome mask the lyric pass samples from a color (emoji) tile, which is a
/// different pipeline and a different texture format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// Platform font id, which already folds in family, weight, style and the
    /// host font file the id resolved to.
    pub font_id: u64,
    pub glyph_id: u32,
    /// Font size in 1/64 px, so a float size cannot alias a nearby one.
    pub size_64ths: u32,
    /// Subpixel offset in 1/4 px, matching the four-phase raster the atlas uses.
    pub subpixel_x: u8,
    pub subpixel_y: u8,
    /// Device scale factor in 1/1000, so DPR 1.25 and 1.5 never collide.
    pub scale_milli: u32,
    pub mode: GlyphMode,
}

impl GlyphKey {
    pub fn new(
        font_id: u64,
        glyph_id: u32,
        font_size: f32,
        subpixel: (u8, u8),
        scale_factor: f32,
        mode: GlyphMode,
    ) -> Self {
        Self {
            font_id,
            glyph_id,
            size_64ths: (font_size * 64.0).round().max(0.0) as u32,
            subpixel_x: subpixel.0,
            subpixel_y: subpixel.1,
            scale_milli: (scale_factor * 1000.0).round().max(0.0) as u32,
            mode,
        }
    }
}

/// Which pipeline consumes the tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlyphMode {
    /// 8-bit coverage mask, one channel, tinted by the text color.
    Mask,
    /// Straight RGBA, used for color emoji.
    Color,
}

impl GlyphMode {
    /// Bytes per pixel of the tile.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            GlyphMode::Mask => 1,
            GlyphMode::Color => 4,
        }
    }
}

/// One rasterized tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphTile {
    pub key: GlyphKey,
    pub width: u32,
    pub height: u32,
    /// CPU-side pixels, kept only while `keep_cpu` is set.
    pub cpu: Option<Vec<u8>>,
    /// Set once the tile has been uploaded into an atlas texture.
    pub gpu_uploaded: bool,
    /// Number of frames that have referenced this tile.
    pub last_used: u64,
    /// Pinned tiles are in flight this frame and must not be evicted.
    pub pins: u32,
}

impl GlyphTile {
    pub fn cpu_bytes(&self) -> u64 {
        self.cpu.as_ref().map(|c| c.len() as u64).unwrap_or(0)
    }

    pub fn gpu_bytes(&self) -> u64 {
        if self.gpu_uploaded {
            self.width as u64 * self.height as u64 * self.key.mode.bytes_per_pixel() as u64
        } else {
            0
        }
    }
}

/// Counters for the resource ledger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlyphAtlasStats {
    pub tiles: u64,
    pub pinned: u64,
    pub cpu_bytes: u64,
    pub gpu_bytes: u64,
    pub evictions: u64,
    /// Requests that had to rasterize because no tile matched.
    pub rasterizations: u64,
    /// Requests served from an existing tile.
    pub hits: u64,
}

/// Byte-budgeted LRU of rasterized glyphs.
pub struct GlyphAtlas {
    tiles: HashMap<GlyphKey, GlyphTile>,
    /// Total bytes, CPU plus GPU, that eviction is measured against.
    budget_bytes: u64,
    clock: u64,
    stats: GlyphAtlasStats,
}

impl GlyphAtlas {
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            tiles: HashMap::new(),
            budget_bytes: budget_bytes.max(1),
            clock: 0,
            stats: GlyphAtlasStats::default(),
        }
    }

    pub fn stats(&self) -> GlyphAtlasStats {
        let mut stats = self.stats;
        stats.tiles = self.tiles.len() as u64;
        stats.pinned = self.tiles.values().filter(|t| t.pins > 0).count() as u64;
        stats.cpu_bytes = self.tiles.values().map(|t| t.cpu_bytes()).sum();
        stats.gpu_bytes = self.tiles.values().map(|t| t.gpu_bytes()).sum();
        stats
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// Start a frame. Every tile's `last_used` older than this frame becomes
    /// eviction-eligible unless it is pinned.
    pub fn begin_frame(&mut self) {
        self.clock += 1;
    }

    /// Look a tile up and mark it used. Pins it for the current frame.
    pub fn get(&mut self, key: &GlyphKey) -> Option<&GlyphTile> {
        let clock = self.clock;
        match self.tiles.get_mut(key) {
            Some(tile) => {
                tile.last_used = clock;
                tile.pins += 1;
                self.stats.hits += 1;
                Some(tile)
            }
            None => None,
        }
    }

    /// Insert a freshly rasterized tile. `keep_cpu` retains the pixels for a
    /// later upload; the GPU copy is counted only after [`Self::mark_uploaded`].
    pub fn insert(
        &mut self,
        key: GlyphKey,
        width: u32,
        height: u32,
        pixels: Option<Vec<u8>>,
        keep_cpu: bool,
    ) -> &GlyphTile {
        let clock = self.clock;
        self.stats.rasterizations += 1;
        let cpu = if keep_cpu { pixels } else { None };
        let tile = GlyphTile {
            key,
            width,
            height,
            cpu,
            gpu_uploaded: false,
            last_used: clock,
            pins: 1,
        };
        self.tiles.insert(key, tile);
        self.evict();
        self.tiles.get(&key).expect("tile was just inserted")
    }

    /// Record that the tile's bytes now live in a GPU atlas texture.
    pub fn mark_uploaded(&mut self, key: &GlyphKey) {
        if let Some(tile) = self.tiles.get_mut(key) {
            tile.gpu_uploaded = true;
        }
    }

    /// Drop a tile. Pinned tiles are refused, because a frame in flight still
    /// samples them.
    pub fn remove(&mut self, key: &GlyphKey) -> bool {
        match self.tiles.get(key) {
            Some(tile) if tile.pins > 0 => false,
            Some(_) => {
                self.tiles.remove(key);
                true
            }
            None => false,
        }
    }

    /// Release this frame's pins. Called once after the frame's batches are built.
    pub fn end_frame(&mut self) {
        for tile in self.tiles.values_mut() {
            tile.pins = 0;
        }
        self.evict();
    }

    /// Evict least-recently-used unpinned tiles until the budget is met.
    fn evict(&mut self) {
        loop {
            let total: u64 = self
                .tiles
                .values()
                .map(|t| t.cpu_bytes() + t.gpu_bytes())
                .sum();
            if total <= self.budget_bytes {
                return;
            }
            let victim = self
                .tiles
                .values()
                .filter(|t| t.pins == 0)
                .min_by_key(|t| t.last_used)
                .map(|t| t.key);
            match victim {
                Some(key) => {
                    self.tiles.remove(&key);
                    self.stats.evictions += 1;
                }
                // Everything left is in flight. The caller finds out through
                // `over_budget` instead of us evicting a tile it is drawing.
                None => return,
            }
        }
    }

    /// True when pinned tiles alone exceed the budget: a caller that sees this
    /// must split the frame rather than let the atlas grow without bound.
    pub fn over_budget(&self) -> bool {
        let total: u64 = self
            .tiles
            .values()
            .map(|t| t.cpu_bytes() + t.gpu_bytes())
            .sum();
        total > self.budget_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(glyph: u32, size: f32, scale: f32) -> GlyphKey {
        GlyphKey::new(7, glyph, size, (0, 0), scale, GlyphMode::Mask)
    }

    #[test]
    fn dpr_and_size_are_part_of_the_key() {
        assert_ne!(key(1, 32.64, 1.0), key(1, 32.64, 1.25));
        assert_ne!(key(1, 32.64, 1.0), key(1, 32.0, 1.0));
        assert_eq!(key(1, 32.64, 1.0), key(1, 32.64, 1.0));
    }

    #[test]
    fn eviction_never_touches_a_pinned_tile() {
        let mut atlas = GlyphAtlas::new(16);
        atlas.begin_frame();
        let a = key(1, 10.0, 1.0);
        atlas.insert(a, 4, 4, Some(vec![0; 16]), true);
        // Both tiles are pinned inside this frame, so the atlas reports the
        // overrun instead of evicting either one.
        let b = key(2, 10.0, 1.0);
        atlas.insert(b, 4, 4, Some(vec![0; 16]), true);
        assert!(atlas.over_budget(), "pinned tiles exceed the budget");
        assert!(atlas.get(&a).is_some(), "pinned tile must survive");
        assert!(atlas.get(&b).is_some(), "pinned tile must survive");
        atlas.end_frame();

        // Pins clear at the end of the frame, so LRU can bring the atlas back
        // under budget without dropping a tile that was still in use.
        assert!(!atlas.over_budget());
        assert_eq!(atlas.stats().tiles, 1);
    }

    #[test]
    fn unpinned_tiles_are_evicted_in_lru_order() {
        let mut atlas = GlyphAtlas::new(32);
        atlas.begin_frame();
        let a = key(1, 10.0, 1.0);
        let b = key(2, 10.0, 1.0);
        atlas.insert(a, 4, 4, Some(vec![0; 16]), true);
        atlas.end_frame();
        atlas.begin_frame();
        atlas.insert(b, 4, 4, Some(vec![0; 16]), true);
        atlas.end_frame();
        atlas.begin_frame();
        // Touch `b`, then insert a third tile: `a` is the oldest untouched.
        assert!(atlas.get(&b).is_some());
        atlas.end_frame();
        atlas.begin_frame();
        let c = key(3, 10.0, 1.0);
        atlas.insert(c, 4, 4, Some(vec![0; 16]), true);
        atlas.end_frame();
        assert!(atlas.get(&a).is_none(), "oldest unpinned tile must go");
        assert!(atlas.get(&b).is_some());
    }

    #[test]
    fn cpu_and_gpu_copies_are_counted_separately() {
        let mut atlas = GlyphAtlas::new(1 << 20);
        atlas.begin_frame();
        let a = key(1, 10.0, 1.0);
        atlas.insert(a, 8, 8, Some(vec![0; 64]), true);
        assert_eq!(atlas.stats().cpu_bytes, 64);
        assert_eq!(atlas.stats().gpu_bytes, 0);
        atlas.mark_uploaded(&a);
        assert_eq!(atlas.stats().gpu_bytes, 64);
    }

    #[test]
    fn a_tile_under_budget_survives_many_frames() {
        let mut atlas = GlyphAtlas::new(1 << 20);
        let a = key(1, 10.0, 1.0);
        atlas.begin_frame();
        atlas.insert(a, 4, 4, Some(vec![0; 16]), true);
        atlas.end_frame();
        for _ in 0..120 {
            atlas.begin_frame();
            assert!(atlas.get(&a).is_some());
            atlas.end_frame();
        }
        assert_eq!(atlas.stats().tiles, 1);
        assert_eq!(atlas.stats().evictions, 0);
    }
}
