//! G0: proves a custom GPU texture can be composited into GPUI's own scene, on
//! GPUI's own Direct3D 11 device, in z-order with GPUI's own primitives.
//!
//! Run with:
//!   cargo test --manifest-path native/Cargo.toml -p oly-render \
//!       --test gpu_interop --features qa-capture -- --nocapture
//!
//! Four properties are asserted:
//!   1. an uploaded RGBA8 texture reaches the framebuffer with its own colours;
//!   2. GPUI quads painted before it are hidden, quads painted after it win —
//!      i.e. one scene, one depth order, not a second layered window;
//!   3. the surface still draws after the window is resized and its bounds
//!      follow the window scale factor instead of assuming device pixels are
//!      logical pixels;
//!   4. releasing the texture drops the live count back to zero.
//!
//! The readback path is QA-only (`render_to_image` exists under
//! `test-support`), so the shipped binary never stalls on a GPU copy.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::*;

const WINDOW_W: f32 = 400.0;
const WINDOW_H: f32 = 300.0;
const SURFACE_PX: u32 = 64;
const RECT_X: f32 = 20.0;
const RECT_Y: f32 = 20.0;
const RECT_W: f32 = 200.0;
const RECT_H: f32 = 200.0;

/// Corner colours of the test texture, in RGBA8.
const TL: [u8; 4] = [255, 0, 0, 255]; // red
const TR: [u8; 4] = [0, 255, 0, 255]; // green
const BL: [u8; 4] = [0, 0, 255, 255]; // blue
const BR: [u8; 4] = [255, 255, 255, 255]; // white

const OCCLUDER: [u8; 4] = [0, 0, 255, 255]; // blue quad painted on top
const UNDERLAY: [u8; 4] = [255, 0, 255, 255]; // magenta quad painted below

/// Builds the 64x64 corner-colour texture (row-major, top-left origin).
fn corner_texture() -> Vec<u8> {
    let mut pixels = vec![0u8; (SURFACE_PX * SURFACE_PX * 4) as usize];
    for y in 0..SURFACE_PX {
        for x in 0..SURFACE_PX {
            let color = match (x < SURFACE_PX / 2, y < SURFACE_PX / 2) {
                (true, true) => TL,
                (false, true) => TR,
                (true, false) => BL,
                (false, false) => BR,
            };
            let i = ((y * SURFACE_PX + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&color);
        }
    }
    pixels
}

struct InteropView {
    surface: Option<CustomSurfaceHandle>,
    /// Published for the harness: the view creates the texture, the test task
    /// releases it and asserts the renderer's live count.
    shared: Rc<Cell<Option<CustomSurfaceHandle>>>,
    texture: Vec<u8>,
}

impl InteropView {
    fn new(shared: Rc<Cell<Option<CustomSurfaceHandle>>>) -> Self {
        Self {
            surface: None,
            shared,
            texture: corner_texture(),
        }
    }
}

impl Render for InteropView {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if self.surface.is_none() {
            let handle = window
                .create_custom_surface(SURFACE_PX, SURFACE_PX, &self.texture)
                .expect("uploading the interop texture");
            log::info!("created custom surface {handle:?}");
            self.surface = Some(handle);
            self.shared.set(Some(handle));
        }
        let handle = self.surface.expect("surface created above");

        // `Div` is a one-shot builder, so every use needs a fresh one.
        let rect = || {
            div()
                .absolute()
                .left(px(RECT_X))
                .top(px(RECT_Y))
                .w(px(RECT_W))
                .h(px(RECT_H))
        };

        div()
            .relative()
            .size_full()
            // Painted first and full-bleed, so it fills every pixel the surface
            // does not cover. The DPI check below samples far outside the
            // surface's logical rect, which must therefore show this colour and
            // not the window clear colour.
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .w(px(WINDOW_W))
                    .h(px(WINDOW_H))
                    .bg(rgb(0xFF00FF)),
            )
            // The custom GPU texture, in the same scene as the quads above.
            .child(
                rect().child(
                    canvas(
                        move |bounds, _window, _cx| bounds,
                        move |bounds, _state, window, _cx| {
                            window.paint_custom_surface(bounds, handle);
                        },
                    )
                    .size_full(),
                ),
            )
            // Painted last, so it must win over the surface in this corner.
            .child(
                div()
                    .absolute()
                    .left(px(RECT_X))
                    .top(px(RECT_Y))
                    .w(px(40.0))
                    .h(px(40.0))
                    .bg(rgb(0x0000FF)),
            )
    }
}

fn approx(actual: [u8; 4], expected: [u8; 4], what: &str) -> Result<(), String> {
    let ok = actual
        .iter()
        .zip(expected.iter())
        .take(3)
        .all(|(a, e)| (*a as i32 - *e as i32).abs() <= 2);
    if ok {
        println!("  ok   {what}: {actual:?}");
        Ok(())
    } else {
        Err(format!("{what}: expected {expected:?}, got {actual:?}"))
    }
}

fn main() {
    env_logger::init();
    // Shared with the async task below: the app's run loop owns the closure, so
    // results are collected through a cell instead of a moved Vec.
    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let report = failures.clone();
    let shared: Rc<Cell<Option<CustomSurfaceHandle>>> = Rc::new(Cell::new(None));

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let failures = failures.clone();
        let view_shared = shared.clone();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(80.0), px(80.0)),
                size(px(WINDOW_W), px(WINDOW_H)),
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("oly-render gpu interop".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };

        let (handle, _view) = gpui_kit::open_window(options, cx, move |_, cx| {
            let view_shared = view_shared.clone();
            cx.new(move |_| InteropView::new(view_shared))
        })
        .expect("opening the interop window");
        let any_handle: AnyWindowHandle = handle.into();
        let task_shared = shared.clone();

        cx.spawn(async move |cx: &mut AsyncApp| {
            // The first frame was drawn by open_window; give the compositor a
            // couple of frames so the scene we capture is the one with the
            // surface already in it.
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;

            println!("\n[1] surface colours reach the framebuffer");
            let first = cx
                .update_window(any_handle, |_, window, _| window.render_to_image())
                .expect("window still open")
                .expect("readback");
            println!("  captured {}x{}", first.width(), first.height());
            for (name, x, y, expected) in [
                // Clear of the occluder quad, which covers 40x40 from the
                // surface's top-left corner.
                ("top-left red", RECT_X + 70.0, RECT_Y + 8.0, TL),
                ("top-right green", RECT_X + RECT_W - 8.0, RECT_Y + 8.0, TR),
                ("bottom-left blue", RECT_X + 8.0, RECT_Y + RECT_H - 8.0, BL),
                ("bottom-right white", RECT_X + RECT_W - 8.0, RECT_Y + RECT_H - 8.0, BR),
            ] {
                let pixel = first.get_pixel(x as u32, y as u32).0;
                if let Err(e) = approx(pixel, expected, name) {
                    failures.borrow_mut().push(e);
                }
            }

            println!("\n[2] z-order with GPUI primitives");
            // Painted after the surface, so the quad must win here.
            let got = first.get_pixel((RECT_X + 20.0) as u32, (RECT_Y + 20.0) as u32).0;
            if let Err(e) = approx(got, OCCLUDER, "quad painted after surface wins") {
                failures.borrow_mut().push(e);
            }
            // Just outside the occluder the surface must still be visible, so
            // the quad above did not simply win everywhere. Sampled deep inside
            // the top-right quadrant: the texture is bilinearly filtered, so a
            // point near the centre seam would legitimately return a blend of
            // two quadrant colours instead of either one.
            let got = first.get_pixel((RECT_X + 160.0) as u32, (RECT_Y + 12.0) as u32).0;
            if let Err(e) = approx(got, TR, "surface still visible beside the occluder") {
                failures.borrow_mut().push(e);
            }
            // The underlay is fully covered by the surface, sampled inside the
            // bottom-right quadrant and away from the centre seam.
            let got = first.get_pixel((RECT_X + 160.0) as u32, (RECT_Y + 160.0) as u32).0;
            if let Err(e) = approx(got, BR, "quad painted before surface is covered") {
                failures.borrow_mut().push(e);
            }

            println!("\n[3] resize keeps the surface correct");
            cx.update_window(any_handle, |_, window, _cx| {
                window.resize(size(px(640.0), px(480.0)));
                // `current_view()` is paint-phase only; `refresh()` is the
                // supported way to ask for a redraw from outside a frame.
                window.refresh();
            })
            .expect("resize");
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let resized = cx
                .update_window(any_handle, |_, window, _| window.render_to_image())
                .expect("window still open")
                .expect("readback after resize");
            println!("  captured {}x{}", resized.width(), resized.height());
            if resized.width() < 600 || resized.height() < 460 {
                failures.borrow_mut().push(format!(
                    "window did not resize: got {}x{}",
                    resized.width(),
                    resized.height()
                ));
            }
            for (name, x, y, expected) in [
                ("resized top-left red", RECT_X + 70.0, RECT_Y + 8.0, TL),
                ("resized bottom-right white", RECT_X + RECT_W - 8.0, RECT_Y + RECT_H - 8.0, BR),
            ] {
                let pixel = resized.get_pixel(x as u32, y as u32).0;
                if let Err(e) = approx(pixel, expected, name) {
                    failures.borrow_mut().push(e);
                }
            }

            println!("\n[4] scale factor moves the surface in device pixels");
            // The surface is 200 logical px wide starting at x=20. At scale 1
            // it ends at device x=220, so x=300 is bare underlay. At scale 1.5
            // the same logical rect snaps to device 30..330, so x=300 lands
            // inside the top-right (green) quadrant. A renderer that ignored
            // the scale factor would leave the underlay showing at both scales.
            let before = resized.get_pixel(300, 100).0;
            if let Err(e) = approx(before, UNDERLAY, "scale 1.0 leaves x=300 bare") {
                failures.borrow_mut().push(e);
            }
            cx.update_window(any_handle, |_, window, _cx| {
                window.set_scale_factor(1.5);
                println!("  window scale factor now {}", window.scale_factor());
            })
            .expect("scale factor");
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let scaled = cx
                .update_window(any_handle, |_, window, _| window.render_to_image())
                .expect("window still open")
                .expect("readback after scale factor change");
            println!(
                "  captured {}x{} at scale 1.5",
                scaled.width(),
                scaled.height()
            );
            let after = scaled.get_pixel(300, 100).0;
            if let Err(e) = approx(after, TR, "scale 1.5 grows the surface into x=300") {
                failures.borrow_mut().push(e);
            }

            println!("\n[5] releasing a surface returns the live count to zero");
            let live = cx
                .update_window(any_handle, |_, window, _| window.live_custom_surfaces())
                .expect("live count");
            println!("  live surfaces while open: {live}");
            if live != 1 {
                failures.borrow_mut().push(format!("expected exactly 1 live surface, got {live}"));
            }
            let handle = task_shared.get().expect("view published its surface handle");
            cx.update_window(any_handle, move |_, window, _| {
                window.destroy_custom_surface(handle);
            })
            .expect("destroy surface");
            let live_after = cx
                .update_window(any_handle, |_, window, _| window.live_custom_surfaces())
                .expect("live count after destroy");
            println!("  live surfaces after release: {live_after}");
            if live_after != 0 {
                failures.borrow_mut().push(format!(
                    "expected the live surface count to return to 0, got {live_after}"
                ));
            }

            cx.update(|app| app.quit());
        })
        .detach();
    });

    println!();
    let failures = report.borrow();
    if failures.is_empty() {
        println!("gpu_interop: PASS");
    } else {
        println!("gpu_interop: FAIL ({} checks)", failures.len());
        for failure in failures.iter() {
            println!("  - {failure}");
        }
        drop(failures);
        std::process::exit(1);
    }
}
