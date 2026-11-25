//! GPUI scroll deltas are signed screen displacement: positive y moves
//! content down (earlier lyrics), negative y moves it up (later lyrics).
//! The player paints row_top + offset, so wheel deltas reach the engine
//! unchanged. scroll_by subtracts its argument; thumb travel therefore uses
//! a positive command to move content up while the thumb moves down.

use gpui_kit::{Pixels, ScrollDelta, ScrollEvent, ScrollSource, TouchPhase};
use oly_core::scroll::{ScrollEngine, ScrollMetrics};

/// One scroll surface: the engine plus the bookkeeping the app layer needs.
#[derive(Debug)]
pub struct ScrollSurface {
    engine: ScrollEngine,
    /// Position of the last touch sample, in window coordinates. The engine
    /// works in deltas from the gesture start, so the start sample is the only
    /// absolute position it needs.
    touch_origin: Option<(f64, f64)>,
    /// Set while a scrollbar thumb is held. The engine does not model the
    /// thumb; it only needs to know that auto-align must stay suspended.
    thumb_held: bool,
}

impl Default for ScrollSurface {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollSurface {
    pub fn new() -> Self {
        Self {
            engine: ScrollEngine::new(),
            touch_origin: None,
            thumb_held: false,
        }
    }

    pub fn engine(&self) -> &ScrollEngine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut ScrollEngine {
        &mut self.engine
    }

    pub fn metrics(&self) -> ScrollMetrics {
        self.engine.metrics()
    }

    pub fn offset(&self) -> f64 {
        self.engine.offset()
    }

    pub fn is_direct(&self)->bool {self.thumb_held || self.touch_origin.is_some() || self.engine.has_inertia()}

    pub fn is_browsing(&self) -> bool {
        self.engine.is_interacting() || self.thumb_held
    }

    /// Feed one GPUI scroll event. Returns true when the offset changed, which
    /// is what the caller uses to decide whether to repaint.
    pub fn on_scroll(&mut self, event: &ScrollEvent, now_ms: f64) -> bool {
        let before = self.engine.offset();
        let (delta_y, pixel_mode) = match event.delta {
            ScrollDelta::Pixels(delta) => (f32::from(delta.y) as f64, true),
            ScrollDelta::Lines(delta) => (delta.y as f64, false),
        };
        match event.source {
            ScrollSource::Touch => {
                self.on_touch(event, now_ms);
                return self.engine.offset() != before;
            }
            ScrollSource::Pointer => {
                self.engine.wheel(delta_y, pixel_mode, now_ms);
            }
        }
        // A zero delta still matters: it keeps the wheel debounce alive while a
        // trackpad reports the tail of a gesture, so it intentionally reaches
        // the engine above.
        self.engine.offset() != before
    }

    fn on_touch(&mut self, event: &ScrollEvent, now_ms: f64) {
        let x = f32::from(event.position.x) as f64;
        let y = f32::from(event.position.y) as f64;
        match event.touch_phase {
            TouchPhase::Started => {
                self.touch_origin = Some((x, y));
                self.engine.touch_start(y, x, now_ms);
            }
            TouchPhase::Moved => {
                let Some((origin_x, origin_y)) = self.touch_origin else {
                    // A move without a start is a lost first sample, not a
                    // gesture; adopting it would make the content jump by the
                    // distance from wherever the previous gesture ended.
                    self.touch_origin = Some((x, y));
                    self.engine.touch_start(y, x, now_ms);
                    return;
                };
                let _ = origin_x;
                let _ = origin_y;
                self.engine.touch_move(y, x, now_ms);
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.touch_origin = None;
                self.engine.touch_end(now_ms);
            }
        }
    }

    /// Recompute the reachable range after a layout pass.
    pub fn set_bounds(&mut self, min: f64, max: f64) {
        self.engine.update_boundary(min, max);
    }

    /// Scrollbar thumb pressed or released.
    pub fn set_thumb_held(&mut self, held: bool) {
        self.thumb_held = held;
    }

    /// A thumb drag expressed in reference `scrollBy` pixels: positive scrolls
    /// towards later lines (decreasing the painted offset). Callers with a
    /// pointer position should use [`Self::drag_thumb_by_pointer`] instead.
    pub fn drag_thumb(&mut self, scroll_by_px: f64) -> f64 {
        self.engine.scroll_by(scroll_by_px)
    }

    /// A thumb drag expressed as pointer travel in window pixels, plus the
    /// 1:1 content-per-lane-pixel factor the scrollbar computed. Dragging the
    /// thumb down means "go to later lines": scroll_by subtracts this positive
    /// command so the actual lyric row moves up.
    pub fn drag_thumb_by_pointer(&mut self, pointer_delta_y: f64, factor: f64) -> f64 {
        self.engine.scroll_by(pointer_delta_y * factor)
    }

    /// Advance inertia and expire the wheel debounce. Call once per frame.
    pub fn tick(&mut self, now_ms: f64) -> bool {
        self.engine.tick(now_ms)
    }

    /// True when the view may snap back to the playing line.
    pub fn can_resume_auto_align(&self, now_ms: f64) -> bool {
        self.engine.can_resume_auto_align(now_ms)
    }

    /// Set the content offset without discarding a wheel gesture or held thumb.
    pub fn move_to(&mut self, target: f64) {
        self.engine.scroll_by(self.engine.offset() - target);
    }

    /// The song changed or the view rebuilt.
    pub fn reset(&mut self, target: f64) {
        self.touch_origin = None;
        self.thumb_held = false;
        self.engine.reset(target);
    }
}

/// Convert a GPUI pixel value to `f64` without reaching into the newtype.
pub fn px_f64(value: Pixels) -> f64 {
    f32::from(value) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, px};

    fn pointer(delta_y: f32, pixels: bool) -> ScrollEvent {
        ScrollEvent {
            position: point(px(0.0), px(0.0)),
            delta: if pixels {
                ScrollDelta::Pixels(point(px(0.0), px(delta_y)))
            } else {
                ScrollDelta::Lines(point(0.0, delta_y))
            },
            source: ScrollSource::Pointer,
            ..Default::default()
        }
    }

    /// Bounds as the oracle records them: offset runs from 0 (start of the song)
    /// to 1000 (end of the content).
    fn surface() -> ScrollSurface {
        let mut surface = ScrollSurface::new();
        surface.set_bounds(0.0, 1000.0);
        surface
    }

    #[test]
    fn a_wheel_rolled_backwards_moves_the_content_up() {
        let mut s=ScrollSurface::new();s.set_bounds(-1000.0,200.0);s.reset(-300.0);
        s.on_scroll(&pointer(-120.0,true),0.0);assert_eq!(s.offset(),-420.0);
    }
    #[test]
    fn a_wheel_rolled_forward_moves_the_content_down() {
        let mut s=ScrollSurface::new();s.set_bounds(-1000.0,200.0);s.reset(-300.0);
        s.on_scroll(&pointer(120.0,true),0.0);assert_eq!(s.offset(),-180.0);
    }
    #[test]
    fn line_mode_scrolls_are_scaled_by_the_engine_not_the_app() {
        let mut s=surface();s.on_scroll(&pointer(3.0,false),0.0);assert_eq!(s.offset(),150.0);
    }

    #[test]
    fn a_zero_delta_does_not_move_but_keeps_the_gesture_alive() {
        let mut surface = surface();
        surface.on_scroll(&pointer(100.0, true), 0.0);
        assert!(surface.is_browsing());
        surface.on_scroll(&pointer(0.0, true), 100.0);
        assert_eq!(surface.offset(), 100.0);
        assert!(surface.is_browsing());
    }

    #[test]
    fn dragging_the_thumb_down_moves_towards_later_lines() {
        let mut surface = surface();
        surface.on_scroll(&pointer(300.0, true), 0.0);
        assert_eq!(surface.offset(), 300.0);
        // Pointer travel is positive downwards; factor 1 keeps it 1:1.
        surface.drag_thumb_by_pointer(200.0, 1.0);
        assert_eq!(surface.offset(), 100.0);
        // Upwards travel is the opposite direction.
        surface.drag_thumb_by_pointer(-400.0, 1.0);
        assert_eq!(surface.offset(), 500.0);
    }

    #[test]
    fn a_thumb_drag_is_one_to_one_and_clears_on_reset() {
        let mut surface = surface();
        surface.set_thumb_held(true);
        surface.drag_thumb(-200.0);
        assert_eq!(surface.offset(), 200.0);
        assert!(surface.is_browsing());
        surface.reset(0.0);
        assert_eq!(surface.offset(), 0.0);
        assert!(!surface.is_browsing());
    }

    #[test]
    fn a_touch_move_without_a_start_adopts_the_first_sample() {
        let mut surface = surface();
        let mut event = pointer(0.0, true);
        event.source = ScrollSource::Touch;
        event.touch_phase = TouchPhase::Moved;
        event.position = point(px(10.0), px(400.0));
        surface.on_scroll(&event, 0.0);
        assert_eq!(surface.offset(), 0.0, "no travel yet, so no movement");
    }

    #[test]
    fn auto_align_stays_off_until_the_gesture_has_been_still() {
        let mut surface = surface();
        surface.on_scroll(&pointer(100.0, true), 0.0);
        assert!(!surface.can_resume_auto_align(0.0));
        // The debounce ends the gesture at +150 ms; the resume window is a
        // further 500 ms from that point.
        assert!(!surface.tick(150.0));
        assert!(!surface.can_resume_auto_align(649.0));
        assert!(surface.can_resume_auto_align(650.0));
    }
    #[test]
    fn absolute_motion_preserves_thumb_and_uses_the_engine_sign() {
        let mut s = ScrollSurface::new();
        s.set_bounds(-1000.0, 300.0);
        s.move_to(200.0);
        assert_eq!(s.offset(), 200.0);
        s.set_thumb_held(true);
        s.move_to(-400.0);
        assert_eq!(s.offset(), -400.0);
        assert!(s.is_browsing());
    }

    #[test]
    fn wheel_down_moves_visible_lyrics_up_in_actual_view_coordinates() {
        let mut s=ScrollSurface::new();s.set_bounds(-2000.0,200.0);s.reset(-300.0);
        let row_top=1000.0;let before=row_top+s.offset();
        s.on_scroll(&pointer(-120.0,true),0.0);
        assert!(row_top+s.offset()<before,"GPUI wheel down must move the actual painted row up");
        s.on_scroll(&pointer(120.0,true),10.0);
        assert_eq!(row_top+s.offset(),before);
    }
    #[test]
    fn line_wheel_down_uses_the_same_visible_direction() {
        let mut s=ScrollSurface::new();s.set_bounds(-2000.0,200.0);s.reset(-300.0);
        s.on_scroll(&pointer(-3.0,false),0.0);assert_eq!(s.offset(),-450.0);
    }

}

#[cfg(test)] mod discrete_wheel_regression {
 use super::*;
 #[test] fn wheel_sets_a_target_without_bypassing_the_row_spring(){
 let mut s=ScrollSurface::new();s.set_bounds(-1000.,200.);s.reset(0.);
 let ev=ScrollEvent{delta:ScrollDelta::Lines(gpui_kit::point(0.,-3.)),source:ScrollSource::Pointer,..Default::default()};
 s.on_scroll(&ev,0.);assert!(s.is_browsing());assert!(!s.is_direct());
 s.set_thumb_held(true);assert!(s.is_direct());
 }
}

#[cfg(test)] mod wheel_motion_integration {
 use super::*;
 #[test] fn discrete_wheel_eases_while_a_held_thumb_tracks_one_to_one(){
 let mut s=ScrollSurface::new();s.set_bounds(-1000.,200.);s.reset(0.);
 let mut motion=crate::player::model::RowMotion::new(0.);
 s.on_scroll(&ScrollEvent{delta:ScrollDelta::Lines(gpui_kit::point(0.,-3.)),source:ScrollSource::Pointer,..Default::default()},0.);
 let params=crate::player::model::Display::default().spring_params();
 let first=motion.step(s.offset(),s.is_direct(),0.,8.,params);assert!(first<0.&&first>s.offset());
 for _ in 0..240{motion.step(s.offset(),s.is_direct(),0.,8.,params);}
 assert!((motion.step(s.offset(),false,0.,8.,params)-s.offset()).abs()<0.1);
 s.set_thumb_held(true);s.move_to(-450.);assert_eq!(motion.step(s.offset(),s.is_direct(),0.,8.,params),-450.);
 }
}
