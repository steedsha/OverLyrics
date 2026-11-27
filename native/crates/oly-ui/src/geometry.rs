//! Basic 2D geometry primitives in window CSS pixels.

/// A rectangle in window CSS pixels, top-left origin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Box4 {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Box4 {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}
