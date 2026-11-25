//! Scene-sampled glass for the popover panels.
//!
//! The reference panels are `backdrop-filter: blur(6px) saturate(1.12)
//! contrast(1.05)` over a `rgb(24 25 28) / .36` tint with a two-tone rim. A
//! backdrop filter reads the pixels *behind the panel*, which in a native
//! renderer means the frame that has already been drawn this frame — not a
//! snapshot taken once at startup.
//!
//! The fork's `PaintHook` is that seam: immediately before a panel's surface is
//! composited, the back buffer holds everything drawn beneath it, so the hook
//! copies the panel's region out, runs the same three-pass chain as the CPU
//! reference (`shaders/glass.hlsl`), and writes the result into the surface's
//! own texture. The surface pass then blends that in at 1:1.
//!
//! Nothing here reads back to the CPU: the copy, the two blur passes and the
//! composite all run on the window's own D3D11 device.
//!
//! Panels are authored in *device* pixels. The scene's surface bounds are
//! already scaled by the window's DPR when the hook runs, so a panel created for
//! `(w, h)` logical pixels is registered at `(w * dpr, h * dpr)` and the rim
//! radius is scaled with it.

use std::ffi::c_void;

use anyhow::{Context as _, Result};
use gpui_kit::{CustomSurfaceHandle, HookContext, PaintHook, Window};
use oly_render::glass::gpu::{GlassRenderer, GlassRequest};
use oly_render::glass::{Material, Rect as GlassRect};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView,
    ID3D11ShaderResourceView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::Interface;

/// Everything the paint hook needs, kept alive by the panel that owns it.
struct HookState {
    device: ID3D11Device,
    /// The immediate context the window renders with. Held so the hook can
    /// copy from the live target without asking the platform mid-paint.
    context: ID3D11DeviceContext,
    renderer: GlassRenderer,
    liquid: [f32;4],
    light:[f32;3],
    lens_mode: f32,
    material: Material,
    shape: Option<GlassRect>,
    region: (u32, u32),
    output: ID3D11RenderTargetView,
    /// The composited texture itself, kept for the evidence probe: the run
    /// report reads it back once so a blank panel can be told apart from a
    /// panel that never reached the scene.
    output_texture: ID3D11Texture2D,
    /// Created on the first frame from the snapshot texture. The pointer is
    /// stable for the panel's lifetime, so one SRV is enough.
    scene_srv: Option<ID3D11ShaderResourceView>,
    /// Last failure, reported once by the owning view instead of being swallowed.
    error: Option<String>,
    /// How many times the paint hook ran, and the last region it copied. A
    /// panel whose counter stays at zero never reached the compositor.
    calls: u64,
    last_box: [u32; 4],
}

/// One glass panel: the texture the surface composites, the scratch copy of the
/// frame behind it, and the hook that fills the former from the latter.
pub struct GlassPanel {
    surface: CustomSurfaceHandle,
    snapshot_surface: CustomSurfaceHandle,
    pixels: (u32, u32),
    /// Boxed so its address is stable for as long as the panel lives; the hook
    /// holds this pointer as its `user_data`.
    state: *mut HookState,
}

// The state lives on the paint thread and is only used there.
unsafe impl Send for GlassPanel {}

impl GlassPanel {
    /// Allocates a panel and registers both textures with the window.
    ///
    /// `width`/`height` are the panel's *device* pixel size, i.e. the logical
    /// size multiplied by the window's scale factor.
    pub fn create(
        window: &Window,
        width: u32,
        height: u32,
        material: Material,
        radius_scale: f64,
    ) -> Result<Self> {
        anyhow::ensure!(width > 0 && height > 0, "glass panel must be nonzero");
        let (device, context) = window.gpu_device_handle()?;

        let output_texture = create_texture(
            &device,
            width,
            height,
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        )?;
        let mut output_view = None;
        unsafe { device.CreateRenderTargetView(&output_texture, None, Some(&mut output_view)) }
            .context("creating the glass panel render target")?;
        let output = output_view.context("no glass panel render target view")?;

        let snapshot =
            create_texture(&device, width, height, D3D11_BIND_SHADER_RESOURCE.0 as u32)?;

        let surface = window.register_gpu_surface(&output_texture)?;
        let snapshot_surface = window.register_gpu_surface(&snapshot)?;

        let mut material = material;
        material.radius *= radius_scale;

        let state = Box::new(HookState {
            device: device.clone(),
            context: context.clone(),
            renderer: GlassRenderer::from_device(device, context),
            liquid: [2.6,12.0,0.14,1.0],
            light:[0.25,0.05,0.35],
            lens_mode: 0.0,
            material,
            shape: None,
            region: (width, height),
            output,
            output_texture,
            scene_srv: None,
            error: None,
            calls: 0,
            last_box: [0; 4],
        });

        Ok(Self {
            surface,
            snapshot_surface,
            pixels: (width, height),
            state: Box::into_raw(state),
        })
    }

    /// Physical optical dimensions follow DPI, while surface alpha only exits.
    pub fn set_liquid(&mut self, scale: f32, opacity: f32, motion: f32) {
        // The boxed state belongs exclusively to this UI/paint thread.
        unsafe { (*self.state).liquid=[2.6*scale*(1.0+0.12*motion),12.0*scale,0.14,opacity]; }
    }

    /// Smooth meniscus refraction and edge blur for clear selector droplets, adapted to axis.
    pub fn set_lens_liquid(&mut self, scale: f32, opacity: f32, swell: f32, vertical: bool) {
        let swell = swell.clamp(0.0, 1.25);
        unsafe {
            (*self.state).lens_mode = if vertical { 2.0 } else { 1.0 };
            (*self.state).liquid = [
                (5.2 + 3.6 * swell) * scale,
                (11.0 + 3.5 * swell) * scale,
                0.24,
                opacity,
            ];
        }
    }

    pub fn set_shape(&mut self,shape:GlassRect,radius:f64) {unsafe {(*self.state).shape=Some(shape);(*self.state).material.radius=radius;}}

    pub fn set_radius(&mut self, radius: f64) {
        unsafe {
            (*self.state).material.radius = radius;
        }
    }

    pub fn set_light(&mut self,light:[f32;3]) {unsafe {(*self.state).light=light;}}
    pub fn set_tint(&mut self, tint: [f64; 3], opacity: f64, rim_hi: [f64; 4], rim_lo: [f64; 4]) {
        unsafe {
            (*self.state).material.tint = tint;
            (*self.state).material.tint_opacity = opacity;
            (*self.state).material.rim_hi = rim_hi;
            (*self.state).material.rim_lo = rim_lo;
        }
    }
    pub fn pixels(&self) -> (u32, u32) { self.pixels }

    /// The handle the element tree composites.
    pub fn surface(&self) -> CustomSurfaceHandle {
        self.surface
    }

    /// The scratch handle the hook writes the frame copy into.
    pub fn snapshot(&self) -> CustomSurfaceHandle {
        self.snapshot_surface
    }

    /// The paint hook for `paint_custom_surface_with_scene`.
    pub fn hook(&self) -> PaintHook {
        PaintHook {
            callback: run_hook,
            user_data: self.state as *mut c_void,
        }
    }

    /// Takes the most recent failure, if any, so the view can surface it.
    pub fn take_error(&mut self) -> Option<String> {
        // Safety: the state is owned by this panel and only touched here or on
        // the paint thread, which never runs while the view is mutating.
        unsafe { (*self.state).error.take() }
    }

    /// Evidence probe: how many times the paint hook ran, where it sampled, and
    /// what the composited texture actually holds.
    ///
    /// A panel that composites as nothing is ambiguous between "the hook never
    /// ran" and "the hook ran but wrote transparent pixels". This separates the
    /// two without guessing from screenshots.
    pub fn probe(&mut self) -> String {
        let (calls, last_box) = unsafe { ((*self.state).calls, (*self.state).last_box) };
        let pixels = match self.read_output() {
            Ok(pixels) => pixels,
            Err(error) => return format!("calls={calls} box={last_box:?} read={error:#}"),
        };
        let mut opaque = 0u32;
        let mut sum = [0u64; 4];
        for px in pixels.chunks_exact(4) {
            if px[3] > 8 {
                opaque += 1;
            }
            for i in 0..4 {
                sum[i] += px[i] as u64;
            }
        }
        let total = (pixels.len() / 4).max(1) as u64;
        format!(
            "calls={calls} box={last_box:?} opaque={opaque}/{} meanBgra=({},{},{},{})",
            pixels.len() / 4,
            sum[0] / total,
            sum[1] / total,
            sum[2] / total,
            sum[3] / total
        )
    }

    /// Copies the composite back to the CPU, for the evidence probe only.
    fn read_output(&self) -> Result<Vec<u8>> {
        let (w, h) = unsafe { (*self.state).region };
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let state = unsafe { &*self.state };
        let mut staging = None;
        unsafe { state.device.CreateTexture2D(&desc, None, Some(&mut staging)) }
            .context("creating the glass probe staging texture")?;
        let staging = staging.context("CreateTexture2D returned no staging texture")?;
        let mut mapped = Default::default();
        unsafe {
            state.context.CopyResource(&staging, &state.output_texture);
            state
                .context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("mapping the glass probe staging texture")?;
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            for y in 0..h {
                let src = (mapped.pData as *const u8).add((y * mapped.RowPitch) as usize);
                let start = (y * w * 4) as usize;
                std::ptr::copy_nonoverlapping(src, pixels[start..].as_mut_ptr(), (w * 4) as usize);
            }
            state.context.Unmap(&staging, 0);
            Ok(pixels)
        }
    }

    /// Releases the GPU textures. Must be called with the window that created
    /// them still alive.
    pub fn destroy(&mut self, window: &Window) {
        window.destroy_custom_surface(self.surface);
        window.destroy_custom_surface(self.snapshot_surface);
        if !self.state.is_null() {
            // Safety: the pointer came from `Box::into_raw` and is taken once.
            unsafe { drop(Box::from_raw(self.state)) };
            self.state = std::ptr::null_mut();
        }
    }
}

impl Drop for GlassPanel {
    fn drop(&mut self) {
        if !self.state.is_null() {
            // The window may already be gone here, so only the Rust side is
            // reclaimed; the platform drops its surface registry with the window.
            unsafe { drop(Box::from_raw(self.state)) };
            self.state = std::ptr::null_mut();
        }
    }
}

fn create_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    bind: u32,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        // The surface shader samples the panel texture verbatim and the swap
        // chain is B8G8R8A8, so a panel must be BGRA too or the red and blue
        // channels swap on composite.
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .context("creating a glass panel texture")?;
    texture.context("CreateTexture2D returned no glass texture")
}

/// The hook: copy the frame behind the panel, filter it, write the panel.
///
/// # Safety
/// Called by the platform renderer during the paint phase with `user_data`
/// pointing at a live [`HookState`] and `ctx` at a valid [`HookContext`]. Both
/// pointers are borrowed only for the call.
unsafe fn run_hook(user_data: *mut c_void, ctx: *mut HookContext) {
    if user_data.is_null() || ctx.is_null() {
        return;
    }
    // Safety: the caller guarantees both pointers for the duration of the call.
    let state = unsafe { &mut *(user_data as *mut HookState) };
    let ctx = unsafe { &*ctx };

    // Safety: every pointer in the context is a borrowed `ID3D11Texture2D`
    // owned by the platform renderer for this call.
    let (Some(render_target), Some(snapshot)) = (unsafe {
        (
            ID3D11Texture2D::from_raw_borrowed(&ctx.render_target),
            ID3D11Texture2D::from_raw_borrowed(&ctx.snapshot),
        )
    }) else {
        state.error = Some("glass panel has no render target or snapshot".into());
        return;
    };

    let region = (ctx.width.min(state.region.0), ctx.height.min(state.region.1));
    if region.0 == 0 || region.1 == 0 {
        return;
    }

    let mut target_desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { render_target.GetDesc(&mut target_desc) };

    let left = ctx.x.max(0) as u32;
    let top = ctx.y.max(0) as u32;
    let right = (left + region.0).min(target_desc.Width);
    let bottom = (top + region.1).min(target_desc.Height);
    if right <= left || bottom <= top {
        // Entirely clipped: the panel is off-window and nothing can be sampled.
        return;
    }

    state.calls += 1;
    state.last_box = [left, top, right, bottom];

    let copy = D3D11_BOX {
        left,
        top,
        front: 0,
        right,
        bottom,
        back: 1,
    };
    // Safety: both textures are on this device, `copy` is inside the source and
    // no pass is in flight while the hook runs.
    unsafe {
        let context = state.context.clone();
        context.CopySubresourceRegion(snapshot, 0, 0, 0, 0, render_target, 0, Some(&copy));
    }

    if state.scene_srv.is_none() {
        let mut view = None;
        // Safety: same-device texture, freshly created, no concurrent use.
        match unsafe { state.device.CreateShaderResourceView(snapshot, None, Some(&mut view)) } {
            Ok(()) => state.scene_srv = view,
            Err(error) => {
                state.error = Some(format!("glass snapshot SRV: {error}"));
                return;
            }
        }
    }
    let Some(scene) = state.scene_srv.as_ref() else {
        return;
    };

    // The panel box is the whole region: the texture is exactly panel-sized, so
    // the rounded-rect SDF covers it edge to edge.
    state.renderer.set_liquid(state.liquid[0],state.liquid[1],state.liquid[2],state.liquid[3]);
    state.renderer.set_light(state.light[0],state.light[1],state.light[2]);
    state.renderer.set_lens_mode(state.lens_mode);
    let request = GlassRequest {
        material: &state.material,
        panel: state.shape.unwrap_or(GlassRect {
            left: 0.0, top: 0.0, width: region.0 as f64, height: region.1 as f64,
        }),
        region,
        scene: &[],
    };
    if let Err(error) = state
        .renderer
        .render_from_scene_texture_into(&request, scene, false, &state.output)
    {
        state.error = Some(format!("glass composite: {error}"));
    }
}
