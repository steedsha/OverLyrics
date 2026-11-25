//! The D3D11 side of the am_bg v36 background.
//!
//! [`super::scene`] decides *what* to draw (warm-up steps, swap, cross-fade);
//! this module owns the device objects and runs [`shaders/background.hlsl`]
//! against them. The split keeps the ordering rules testable without a GPU and
//! keeps every unsafe call in one file.
//!
//! ## Resource shape
//!
//! One cover is one [`GpuScene`]: a 200x200 `R8G8B8A8_UNORM` source plus four
//! 200x200 `R16G16B16A16_FLOAT` targets (`acc`, `msh`, `b1`, `b2`). The
//! reference keeps at most three scenes alive (current, previous, pending), and
//! so does this renderer — [`BackgroundRenderer::live_textures`] derives the
//! live count from those slots, so the lifecycle test measures the scenes that
//! actually exist rather than a bookkeeping number that can drift.
//!
//! `acc` is never cleared between steps. That is the feedback: the layer pass
//! discards outside its rotated footprint, so those pixels keep the previous
//! step's value. Clearing it would turn the slow smear into a spinning decal,
//! which is the single easiest way to break this effect while looking plausible.
//!
//! ## Where the pixels go
//!
//! The final pass renders into an offscreen `R8G8B8A8_UNORM` target the size of
//! the window. [`BackgroundRenderer::read_present`] copies it to a staging
//! texture and maps it, which is both the QA path and the current integration
//! path: the bytes feed `create_custom_surface`/`update_custom_surface` in the
//! vendored GPUI fork. Handing the SRV straight to the compositor would avoid
//! the round trip; that is a later optimisation, and the readback keeps this
//! step honest about what the finished frame actually contains.

#![allow(unsafe_code)]

use std::ffi::c_void;

use anyhow::{Context, Result, anyhow};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::{
    D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
    ID3DBlob, ID3DInclude,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_INDEX_BUFFER, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BIND_VERTEX_BUFFER, D3D11_BLEND_DESC,
    D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BLEND_SRC_ALPHA,
    D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CULL_NONE, D3D11_FILL_SOLID,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_VERTEX_DATA,
    D3D11_MAP_READ, D3D11_RASTERIZER_DESC, D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SAMPLER_DESC,
    D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11_VIEWPORT,
    D3D11CreateDevice, ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext,
    ID3D11InputLayout, ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R16_UINT,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_SAMPLE_DESC,
};
use windows::core::PCSTR;

use super::artwork::RgbaImage;
use super::scene::{BackgroundScene, FramePlan, PresentPlan};

/// The working resolution of the whole pass chain, from `renderer.ts`.
const SIZE: u32 = 200;

/// Vertex stride of the mesh grid: position, uv, from, to.
const MESH_STRIDE: u32 = 32;

/// The shader source is compiled at runtime with `D3DCompile`.
///
/// `build.rs` in the GPUI fork precompiles its own modules with `fxc` in
/// release; this crate cannot assume an SDK is installed, and
/// `d3dcompiler_47.dll` ships with Windows, so `D3DCompile` is the portable
/// choice. Compilation happens once per device (a few milliseconds); the
/// bytecode is then reused for every frame.
const SHADER_SRC: &str = include_str!("../../shaders/background.hlsl");

/// `renderer.ts` step parameters: omega (deg/s), rotation centre, scale.
const LAYERS: [(f32, f32, f32, f32); 3] = [
    (8.3333333, 0.5, 0.5, 1.0),
    (16.6666667, 0.0, 0.0, 0.82),
    (4.1666667, 0.75, 0.75, 1.0),
];

/// `blurX`/`blurY` sigma, in 200px texels.
const BLUR_SIGMA: f32 = 16.0;

/// Sampling window for the final pass, in source-field uv space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampling {
    pub sample_y: f32,
    pub sample_height: f32,
    pub scrim: [f32; 3],
    /// Dynamic brightness breathing multiplier (1.0 = normal, <1.0 paused, >1.0 bass pulse).
    pub breath: f32,
    /// Optional drum-beat onset flash intensity (`0.0..=1.0`, `0.0` when disabled).
    pub beat_flash: f32,
    /// Elastic subwoofer spring displacement (`-0.35..=1.0`, `0.0` when disabled).
    pub beat_wobble: f32,
}

impl Default for Sampling {
    fn default() -> Self {
        // `renderer.ts`'s default `sampling`.
        Self {
            sample_y: 0.21875,
            sample_height: 0.5625,
            scrim: [0.0, 0.0, 0.0],
            breath: 1.0,
            beat_flash: 0.0,
            beat_wobble: 0.0,
        }
    }
}

/// Which of a scene's four intermediates to read back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Acc,
    Mesh,
    BlurX,
    BlurY,
}

/// A 200x200 RGBA float field, row 0 at the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f32>,
}

impl Field {
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }
}

/// The constant block, laid out to match `cbuffer BackgroundParams`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Params {
    layer: [f32; 4],
    final_: [f32; 4],
    final2: [f32; 4],
    scrim: [f32; 4],
    dir: [f32; 2],
    sigma: f32,
    mesh: f32,
    swap: f32,
    alpha: f32,
    target: [f32; 2],
}

/// 96 bytes, which is `size_of::<Params>()` and a multiple of 16.
const PARAMS_BYTES: usize = 96;

impl Params {
    fn to_bytes(self) -> [u8; PARAMS_BYTES] {
        let mut out = [0u8; PARAMS_BYTES];
        let mut at = 0;
        let mut put = |v: f32| {
            out[at..at + 4].copy_from_slice(&v.to_le_bytes());
            at += 4;
        };
        for v in self.layer {
            put(v);
        }
        for v in self.final_ {
            put(v);
        }
        for v in self.final2 {
            put(v);
        }
        for v in self.scrim {
            put(v);
        }
        for v in self.dir {
            put(v);
        }
        put(self.sigma);
        put(self.mesh);
        put(self.swap);
        put(self.alpha);
        for v in self.target {
            put(v);
        }
        debug_assert_eq!(at, PARAMS_BYTES);
        out
    }
}

struct Target {
    _texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    rtv: ID3D11RenderTargetView,
}

/// One cover's textures and its exposure.
struct GpuScene {
    _source: ID3D11Texture2D,
    source_view: ID3D11ShaderResourceView,
    acc: Target,
    mesh: Target,
    blur_x: Target,
    blur_y: Target,
    gain: f32,
    scrim: f32,
    mono_factor: f32,
}

impl GpuScene {
    fn target(&self, slot: Slot) -> &Target {
        match slot {
            Slot::Acc => &self.acc,
            Slot::Mesh => &self.mesh,
            Slot::BlurX => &self.blur_x,
            Slot::BlurY => &self.blur_y,
        }
    }
}

struct Pipelines {
    quad_vs: ID3D11VertexShader,
    final_vs: ID3D11VertexShader,
    mesh_vs: ID3D11VertexShader,
    layer_ps: ID3D11PixelShader,
    blur_ps: ID3D11PixelShader,
    mix_ps: ID3D11PixelShader,
    final_ps: ID3D11PixelShader,
    copy_ps: ID3D11PixelShader,
    mesh_layout: ID3D11InputLayout,
    mesh_vertices: ID3D11Buffer,
    mesh_indices: ID3D11Buffer,
    index_count: u32,
}

/// The am_bg v36 renderer.
pub struct BackgroundRenderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    owns_device: bool,
    pipelines: Pipelines,
    params: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend_opaque: ID3D11BlendState,
    blend_alpha: ID3D11BlendState,
    raster: ID3D11RasterizerState,

    scene: BackgroundScene,
    current: Option<GpuScene>,
    previous: Option<GpuScene>,
    pending: Option<GpuScene>,

    present_texture: ID3D11Texture2D,
    present_rtv: ID3D11RenderTargetView,
    present_size: (u32, u32),
    needs_present: bool,
    staging: Option<ID3D11Texture2D>,
}

impl BackgroundRenderer {
    /// Creates a private device. The QA tests use this; the application will use
    /// [`Self::from_device`] with the device GPUI already owns, so there is
    /// exactly one `ID3D11Device` in the process.
    pub fn new_own() -> Result<Self> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[
                    D3D_FEATURE_LEVEL_11_1,
                    D3D_FEATURE_LEVEL_11_0,
                    D3D_FEATURE_LEVEL_10_1,
                ]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None::<*mut D3D_FEATURE_LEVEL>,
                Some(&mut context),
            )
            .context("creating a D3D11 device for the background renderer")?;
        }
        let device = device.context("D3D11CreateDevice returned no device")?;
        let context = context.context("D3D11CreateDevice returned no context")?;
        let mut renderer = Self::from_device(device, context)?;
        renderer.owns_device = true;
        Ok(renderer)
    }

    /// Builds the renderer on an existing device.
    pub fn from_device(device: ID3D11Device, context: ID3D11DeviceContext) -> Result<Self> {
        let pipelines = Pipelines::new(&device)?;
        let sampler = create_sampler(&device)?;
        let params = create_params(&device)?;
        let blend_opaque = create_blend(&device, false)?;
        let blend_alpha = create_blend(&device, true)?;
        let raster = create_rasterizer(&device)?;
        let (present_texture, present_rtv) = create_target_rgba8(&device, 1272, 843)?;
        Ok(Self {
            device,
            context,
            owns_device: false,
            pipelines,
            params,
            sampler,
            blend_opaque,
            blend_alpha,
            raster,
            scene: BackgroundScene::new(),
            current: None,
            previous: None,
            pending: None,
            present_texture,
            present_rtv,
            present_size: (1272, 843),
            needs_present: false,
            staging: None,
        })
    }

    /// Resizes the offscreen present target. Called when the window resizes.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let width = width.max(1);
        let height = height.max(1);
        if (width, height) == self.present_size {
            return Ok(());
        }
        let (texture, rtv) = create_target_rgba8(&self.device, width, height)?;
        self.present_texture = texture;
        self.present_rtv = rtv;
        self.present_size = (width, height);
        self.needs_present = self.current.is_some();
        // The staging copy was sized for the old target.
        self.staging = None;
        Ok(())
    }

    pub fn present_size(&self) -> (u32, u32) {
        self.present_size
    }

    /// The GPU texture the final background pass renders into (BGRA8, same
    /// device, `present_size` pixels). The shell registers it once with the
    /// fork as a GPU custom surface and repaints the scene over it per frame —
    /// no CPU readback on this path. Re-register after device loss (the fork
    /// clears registered surfaces when its device is rebuilt).
    pub fn present_texture_handle(&self) -> &ID3D11Texture2D {
        &self.present_texture
    }

    /// Installs a cover. Any earlier pending cover is released, exactly as
    /// `setArtwork` drops `pending` before building the new scene.
    pub fn set_artwork(&mut self, prepared: &super::artwork::PreparedArtwork) -> Result<()> {
        self.pending = None;
        let scene = GpuScene::new(
            &self.device,
            &prepared.source,
            prepared.gain as f32,
            prepared.mono_factor,
        )?;
        // `setArtwork` clears `acc` to opaque black once, right after the four
        // targets exist. D3D11 leaves a fresh texture's contents undefined, so
        // without this the first warm-up steps would blend three layer copies
        // onto whatever the driver happened to hand back.
        unsafe {
            self.context
                .ClearRenderTargetView(&scene.acc.rtv, &[0.0, 0.0, 0.0, 1.0]);
        }
        self.pending = Some(scene);
        self.scene.set_artwork(self.scene.time());
        Ok(())
    }

    /// Drops every scene, as `renderer.clear()` does.
    pub fn clear(&mut self) {
        self.scene.clear();
        self.current = None;
        self.previous = None;
        self.pending = None;
        self.needs_present = false;
    }

    /// How many scene textures are alive right now.
    ///
    /// Derived from the slots rather than incremented by hand. The reference
    /// keeps a `Set` of textures and deletes them on drop; a counter here would
    /// have to be decremented on every drop path, and the first path that
    /// forgets turns the lifecycle test into a check of the counter instead of
    /// the textures. One live scene owns five (source, acc, msh, b1, b2) and at
    /// most three scenes are resident (current, previous, pending), so the
    /// ceiling this can report is 15.
    pub fn live_textures(&self) -> u64 {
        let alive = [
            self.current.is_some(),
            self.previous.is_some(),
            self.pending.is_some(),
        ];
        5 * alive.iter().filter(|slot| **slot).count() as u64
    }

    pub fn is_ready(&self) -> bool {
        self.scene.is_ready()
    }

    pub fn has_work(&self) -> bool {
        self.scene.has_work()
    }

    /// Advances one display frame and renders it.
    ///
    /// `times` are the 60 Hz simulation steps this frame earned; `now` is the
    /// display timestamp used for the 550 ms cross-fade.
    pub fn tick(&mut self, times: &[f64], now: f64, sampling: Sampling) -> Result<()> {
        let plan = self.scene.tick(times, now);
        self.run_plan(&plan, now, sampling)
    }

    fn run_plan(&mut self, plan: &FramePlan, now: f64, sampling: Sampling) -> Result<()> {
        // Destructure so the disjoint field borrows below are explicit: the
        // step helper needs `&mut GpuScene` while every device object is
        // borrowed immutably.
        let Self {
            context,
            pipelines,
            params,
            sampler,
            blend_opaque,
            raster,
            pending,
            ..
        } = self;

        if let Some((start, count)) = plan.warmup {
            let scene = pending
                .as_mut()
                .context("a warm-up plan without a pending scene")?;
            for k in 0..count {
                // `renderer.ts` decrements `warm` inside the loop, so each step
                // advances by one 60 Hz tick.
                let at = start + f64::from(k) / 60.0;
                run_step(context, pipelines, params, sampler, blend_opaque, raster, scene, at)?;
            }
        }

        if plan.swapped {
            // `previous = current; current = pending; pending = None`, dropping
            // the scene that was fading out.
            self.previous = self.current.take();
            self.current = self.pending.take();
        }

        {
            let Self {
                context,
                pipelines,
                params,
                sampler,
                blend_opaque,
                raster,
                current,
                ..
            } = self;
            if let Some(scene) = current.as_mut() {
                for &t in &plan.steps {
                    run_step(context, pipelines, params, sampler, blend_opaque, raster, scene, t)?;
                }
            }
        }

        if let Some(present) = plan.present {
            self.present(present, sampling)?;
            self.needs_present = false;
        } else if self.needs_present && self.current.is_some() {
            let present = self.scene.present_plan(now).unwrap_or(PresentPlan {
                previous: None,
                current: 1.0,
            });
            self.present(present, sampling)?;
            self.needs_present = false;
        }

        if !self.scene.has_work() {
            self.previous = None;
        }
        Ok(())
    }

    fn present(&mut self, plan: PresentPlan, sampling: Sampling) -> Result<()> {
        let (width, height) = self.present_size;
        unsafe {
            self.context
                .OMSetRenderTargets(Some(&[Some(self.present_rtv.clone())]), None);
            self.context
                .ClearRenderTargetView(&self.present_rtv, &[0.0, 0.0, 0.0, 1.0]);
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
        }

        if let Some(previous_opacity) = plan.previous {
            unsafe {
                self.context.OMSetBlendState(&self.blend_opaque, None, u32::MAX);
            }
            let scene = self
                .previous
                .as_ref()
                .context("a cross-fade plan without a previous scene")?;
            self.present_one(scene, previous_opacity, sampling)?;
        }

        unsafe {
            self.context.OMSetBlendState(&self.blend_alpha, None, u32::MAX);
        }
        {
            let scene = self
                .current
                .as_ref()
                .context("a present plan without a current scene")?;
            self.present_one(scene, plan.current, sampling)?;
        }
        unsafe {
            self.context.OMSetBlendState(&self.blend_opaque, None, u32::MAX);
        }
        Ok(())
    }

    fn present_one(&self, scene: &GpuScene, opacity: f64, sampling: Sampling) -> Result<()> {
        let s = scene.scrim;
        let params = Params {
            layer: [
                self.scene.time() as f32,
                scene.mono_factor,
                sampling.beat_flash.clamp(0.0, 2.5),
                sampling.beat_wobble.clamp(-1.25, 2.5),
            ],
            final_: [1.0, 0.0, 1.0, scene.gain],
            final2: [
                sampling.sample_y * SIZE as f32,
                sampling.sample_height * SIZE as f32,
                SIZE as f32,
                opacity as f32,
            ],
            scrim: [s * 0.6, s * 0.85, s, sampling.breath.max(0.05)],
            target: [self.present_size.0 as f32, self.present_size.1 as f32],
            ..Params::default()
        };
        self.upload(&params)?;

        unsafe {
            self.context.VSSetShader(&self.pipelines.final_vs, None);
            self.context.PSSetShader(&self.pipelines.final_ps, None);
            self.context.IASetInputLayout(None::<&ID3D11InputLayout>);
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            self.context
                .PSSetShaderResources(0, Some(&[Some(scene.blur_y.srv.clone())]));
            self.context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.context.Draw(4, 0);
        }
        Ok(())
    }

    /// Reads one of the four intermediates back as float.
    ///
    /// Row 0 is the top of the field. WebGL's `readPixels` on the same target
    /// returns row 0 at the bottom, so the parity test flips before comparing.
    pub fn read_field(&mut self, slot: Slot) -> Result<Field> {
        let scene = self
            .current
            .as_ref()
            .or(self.pending.as_ref())
            .context("no scene to read")?;
        let target = scene.target(slot);
        let staging = create_staging(&self.device, SIZE, SIZE, DXGI_FORMAT_R16G16B16A16_FLOAT)?;
        unsafe {
            self.context.CopyResource(&staging, &target._texture);
            let mut mapped = Default::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("mapping the field staging texture")?;
            let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
            for y in 0..SIZE {
                let row = (mapped.pData as *const u8).add((y * mapped.RowPitch) as usize);
                let halves = std::slice::from_raw_parts(row as *const u16, (SIZE * 4) as usize);
                for &h in halves {
                    rgba.push(half_to_f32(h));
                }
            }
            self.context.Unmap(&staging, 0);
            Ok(Field {
                width: SIZE,
                height: SIZE,
                rgba,
            })
        }
    }

    /// Reads the composed frame as RGBA8, row 0 at the top.
    ///
    /// The present target is BGRA8 so the window can register it verbatim as a
    /// swap-chain-ordered surface. This readback keeps the QA contract in RGB
    /// order: the staging copy is BGRA8 and the two colour channels are
    /// swapped while the rows are copied out.
    pub fn read_present(&mut self) -> Result<RgbaImage> {
        let (width, height) = self.present_size;
        let staging = create_staging(&self.device, width, height, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        unsafe {
            self.context.CopyResource(&staging, &self.present_texture);
            let mut mapped = Default::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("mapping the present staging texture")?;
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            for y in 0..height {
                let src = (mapped.pData as *const u8).add((y * mapped.RowPitch) as usize);
                let dst = &mut pixels[(y * width * 4) as usize..((y + 1) * width * 4) as usize];
                for x in 0..width as usize {
                    let s = src.add(x * 4);
                    dst[x * 4] = *s.add(2);
                    dst[x * 4 + 1] = *s.add(1);
                    dst[x * 4 + 2] = *s.add(0);
                    dst[x * 4 + 3] = *s.add(3);
                }
            }
            self.context.Unmap(&staging, 0);
            Ok(RgbaImage::new(width, height, pixels))
        }
    }

    fn upload(&self, params: &Params) -> Result<()> {
        let bytes = params.to_bytes();
        unsafe {
            self.context.UpdateSubresource(
                &self.params,
                0,
                None,
                bytes.as_ptr() as *const c_void,
                0,
                0,
            );
            self.context
                .VSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            self.context
                .PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
        }
        Ok(())
    }
}

impl Drop for BackgroundRenderer {
    fn drop(&mut self) {
        self.clear();
        if self.owns_device {
            // Nothing else to do: the COM references below drop with the struct.
        }
    }
}

/// Runs one simulation step: three layer draws, mesh, blurX, blurY, mix.
///
/// The blend and rasterizer states are owned by [`BackgroundRenderer`] and
/// passed in rather than created here: a warm-up tick runs this up to
/// [`MAX_WARM_PER_TICK`] times, and creating a state object per pass would
/// churn COM objects on the frame that matters most.
///
/// The rasterizer state is `CULL_NONE` because WebGL starts with `CULL_FACE`
/// disabled, and D3D11 does not: the default state culls back faces, which
/// would silently drop one of the two triangles of every mesh quad.
fn run_step(
    context: &ID3D11DeviceContext,
    pipelines: &Pipelines,
    params: &ID3D11Buffer,
    sampler: &ID3D11SamplerState,
    blend_opaque: &ID3D11BlendState,
    raster: &ID3D11RasterizerState,
    scene: &mut GpuScene,
    t: f64,
) -> Result<()> {
    let upload = |p: &Params| -> Result<()> {
        let bytes = p.to_bytes();
        unsafe {
            context.UpdateSubresource(
                params,
                0,
                None,
                bytes.as_ptr() as *const c_void,
                0,
                0,
            );
            context.VSSetConstantBuffers(0, Some(&[Some(params.clone())]));
            context.PSSetConstantBuffers(0, Some(&[Some(params.clone())]));
        }
        Ok(())
    };
    let bind = |target: &Target| unsafe {
        context.OMSetRenderTargets(Some(&[Some(target.rtv.clone())]), None);
        context.RSSetState(raster);
        context.RSSetViewports(Some(&[D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: SIZE as f32,
            Height: SIZE as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        }]));
    };
    let quad = |srv: &ID3D11ShaderResourceView| unsafe {
        context.VSSetShader(&pipelines.quad_vs, None);
        context.IASetInputLayout(None::<&ID3D11InputLayout>);
        context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        context.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
        context.PSSetSamplers(0, Some(&[Some(sampler.clone())]));
        context.Draw(4, 0);
    };

    // ---- layer: three rotated copies straight into `acc`, no clear.
    bind(&scene.acc);
    unsafe {
        context.OMSetBlendState(blend_opaque, None, u32::MAX);
    }
    unsafe { context.PSSetShader(&pipelines.layer_ps, None) };
    for (omega, cx, cy, scale) in LAYERS {
        // `uAng = -omega * (t + 28) * PI / 180`, with the +28 s phase from the
        // reference. The HLSL shader evaluates `cos(-p_layer.x)` and
        // `sin(-p_layer.x)` like the GLSL does, so the sign has to arrive
        // here already negated: passing a positive angle flips the rotation
        // direction, which a symmetric field would hide.
        let angle = -omega * (t as f32 + 28.0) * std::f32::consts::PI / 180.0;
        upload(&Params {
            layer: [angle, cx, cy, scale],
            swap: 0.0,
            ..Params::default()
        })?;
        quad(&scene.source_view);
    }

    // ---- mesh: clear `msh` to opaque black, then deform `acc` into it.
    bind(&scene.mesh);
    unsafe {
        context.ClearRenderTargetView(&scene.mesh.rtv, &[0.0, 0.0, 0.0, 1.0]);
        context.OMSetBlendState(blend_opaque, None, u32::MAX);
        context.VSSetShader(&pipelines.mesh_vs, None);
        context.PSSetShader(&pipelines.copy_ps, None);
        context.PSSetShaderResources(0, Some(&[Some(scene.acc.srv.clone())]));
        context.PSSetSamplers(0, Some(&[Some(sampler.clone())]));
        context.IASetInputLayout(&pipelines.mesh_layout);
        context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        let buffers = [Some(pipelines.mesh_vertices.clone())];
        let strides = [MESH_STRIDE];
        let offsets = [0u32];
        context.IASetVertexBuffers(
            0,
            1,
            Some(buffers.as_ptr()),
            Some(strides.as_ptr()),
            Some(offsets.as_ptr()),
        );
        context.IASetIndexBuffer(&pipelines.mesh_indices, DXGI_FORMAT_R16_UINT, 0);
    }
    let u = t as f32 + 28.0;
    let morph = 0.5 * ((0.897598 * u).sin().asin() + 0.35 * (0.371 * u).sin()) / 1.25;
    upload(&Params {
        mesh: morph,
        ..Params::default()
    })?;
    unsafe { context.DrawIndexed(pipelines.index_count, 0, 0) };

    // ---- blurX, then blurY.
    bind(&scene.blur_x);
    unsafe {
        context.OMSetBlendState(blend_opaque, None, u32::MAX);
        context.PSSetShader(&pipelines.blur_ps, None);
    }
    upload(&Params {
        dir: [1.0 / SIZE as f32, 0.0],
        sigma: BLUR_SIGMA,
        ..Params::default()
    })?;
    quad(&scene.mesh.srv);

    bind(&scene.blur_y);
    unsafe { context.PSSetShader(&pipelines.blur_ps, None) };
    upload(&Params {
        dir: [0.0, 1.0 / SIZE as f32],
        sigma: BLUR_SIGMA,
        ..Params::default()
    })?;
    quad(&scene.blur_x.srv);

    // ---- mix back into `acc` with alpha 1 (an overwrite, but the reference
    // uses the alpha blend state here and the result must match if it changes).
    bind(&scene.acc);
    unsafe {
        context.PSSetShader(&pipelines.mix_ps, None);
    }
    upload(&Params {
        alpha: 1.0,
        ..Params::default()
    })?;
    quad(&scene.blur_y.srv);

    Ok(())
}

impl GpuScene {
    fn new(
        device: &ID3D11Device,
        source: &RgbaImage,
        gain: f32,
        mono_factor: f32,
    ) -> Result<Self> {
        let source_texture = create_texture_rgba8(device, SIZE, SIZE, &source.pixels)?;
        let source_view = create_srv(device, &source_texture)?;
        let n = (source.width * source.height).max(1) as f32;
        let mean_y: f32 = source
            .pixels
            .chunks_exact(4)
            .map(|px| (0.3 * px[0] as f32 + 0.59 * px[1] as f32 + 0.11 * px[2] as f32) / 255.0)
            .sum::<f32>()
            / n;
        let scrim = ((mean_y - 0.18) * 0.45).clamp(0.04, 0.18);
        Ok(Self {
            _source: source_texture,
            source_view,
            acc: create_target_float(device)?,
            mesh: create_target_float(device)?,
            blur_x: create_target_float(device)?,
            blur_y: create_target_float(device)?,
            gain,
            scrim,
            mono_factor,
        })
    }
}

// ------------------------------------------------------------------ factories

fn create_texture_rgba8(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<ID3D11Texture2D> {
    assert_eq!(pixels.len(), (width * height * 4) as usize);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr() as *const c_void,
        SysMemPitch: width * 4,
        SysMemSlicePitch: (width * height * 4) as u32,
    };
    let mut texture = None;
    unsafe {
        device
            .CreateTexture2D(&desc, Some(&data), Some(&mut texture))
            .context("creating the 200px source texture")?;
    }
    texture.context("CreateTexture2D returned no texture")
}

fn create_target_float(device: &ID3D11Device) -> Result<Target> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: SIZE,
        Height: SIZE,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .context("creating a 200px RGBA16F target")?;
    }
    let texture = texture.context("CreateTexture2D returned no target")?;
    let srv = create_srv(device, &texture)?;
    let mut rtv = None;
    unsafe {
        device
            .CreateRenderTargetView(&texture, None, Some(&mut rtv))
            .context("creating a render target view")?;
    }
    Ok(Target {
        _texture: texture,
        srv,
        rtv: rtv.context("CreateRenderTargetView returned no view")?,
    })
}

fn create_target_rgba8(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, ID3D11RenderTargetView)> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        // The present target is what the window shows, so it is stored in the
        // swap chain's own channel order (BGRA8). A BGRA8 target means the
        // shader's .x lands in the byte the display reads as blue, which is
        // what a "verbatim" surface sample expects. The render-target flag is
        // joined by the shader-resource flag because the window registers this
        // very texture as a GPUI surface; without it CreateShaderResourceView
        // fails with E_INVALIDARG.
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .context("creating the present target")?;
    }
    let texture = texture.context("CreateTexture2D returned no present target")?;
    let mut rtv = None;
    unsafe {
        device
            .CreateRenderTargetView(&texture, None, Some(&mut rtv))
            .context("creating the present render target view")?;
    }
    Ok((texture, rtv.context("no present rtv")?))
}

fn create_staging(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: windows::Win32::Graphics::Direct3D11::D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .context("creating a staging texture")?;
    }
    texture.context("CreateTexture2D returned no staging texture")
}

fn create_srv(device: &ID3D11Device, texture: &ID3D11Texture2D) -> Result<ID3D11ShaderResourceView> {
    let mut view = None;
    unsafe {
        device
            .CreateShaderResourceView(texture, None, Some(&mut view))
            .context("creating a shader resource view")?;
    }
    view.context("CreateShaderResourceView returned no view")
}

fn create_sampler(device: &ID3D11Device) -> Result<ID3D11SamplerState> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: windows::Win32::Graphics::Direct3D11::D3D11_COMPARISON_ALWAYS,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
    };
    let mut sampler = None;
    unsafe {
        device
            .CreateSamplerState(&desc, Some(&mut sampler))
            .context("creating the linear/clamp sampler")?;
    }
    sampler.context("CreateSamplerState returned no sampler")
}

fn create_params(device: &ID3D11Device) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: PARAMS_BYTES as u32,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let mut buffer = None;
    unsafe {
        device
            .CreateBuffer(&desc, None, Some(&mut buffer))
            .context("creating the constant buffer")?;
    }
    buffer.context("CreateBuffer returned no constant buffer")
}

fn create_blend(
    device: &ID3D11Device,
    alpha: bool,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11BlendState> {
    let rt = D3D11_RENDER_TARGET_BLEND_DESC {
        BlendEnable: alpha.into(),
        SrcBlend: if alpha {
            D3D11_BLEND_SRC_ALPHA
        } else {
            windows::Win32::Graphics::Direct3D11::D3D11_BLEND_ONE
        },
        DestBlend: if alpha {
            D3D11_BLEND_INV_SRC_ALPHA
        } else {
            windows::Win32::Graphics::Direct3D11::D3D11_BLEND_ZERO
        },
        BlendOp: D3D11_BLEND_OP_ADD,
        // Keep the destination alpha at 1 so the uploaded surface is opaque.
        SrcBlendAlpha: D3D11_BLEND_ONE,
        DestBlendAlpha: D3D11_BLEND_ONE,
        BlendOpAlpha: D3D11_BLEND_OP_ADD,
        RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
    };
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0] = rt;
    let mut state = None;
    unsafe {
        device
            .CreateBlendState(&desc, Some(&mut state))
            .context("creating a blend state")?;
    }
    state.context("CreateBlendState returned no state")
}

/// `CULL_NONE`, solid fill: WebGL starts with `CULL_FACE` disabled and D3D11
/// does not, so the default state would drop half of every mesh quad.
fn create_rasterizer(device: &ID3D11Device) -> Result<ID3D11RasterizerState> {
    let desc = D3D11_RASTERIZER_DESC {
        FillMode: D3D11_FILL_SOLID,
        CullMode: D3D11_CULL_NONE,
        FrontCounterClockwise: false.into(),
        DepthBias: 0,
        DepthBiasClamp: 0.0,
        SlopeScaledDepthBias: 0.0,
        DepthClipEnable: true.into(),
        ScissorEnable: false.into(),
        MultisampleEnable: false.into(),
        AntialiasedLineEnable: false.into(),
    };
    let mut state = None;
    unsafe {
        device
            .CreateRasterizerState(&desc, Some(&mut state))
            .context("creating the rasterizer state")?;
    }
    state.context("CreateRasterizerState returned no state")
}

// ------------------------------------------------------------------ shaders

fn compile(entry: &str, target: &str) -> Result<ID3DBlob> {
    let mut entry_z = Vec::with_capacity(entry.len() + 1);
    entry_z.extend_from_slice(entry.as_bytes());
    entry_z.push(0);
    let mut target_z = Vec::with_capacity(target.len() + 1);
    target_z.extend_from_slice(target.as_bytes());
    target_z.push(0);

    let mut blob: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let hr = unsafe {
        D3DCompile(
            SHADER_SRC.as_ptr() as *const c_void,
            SHADER_SRC.len(),
            PCSTR::from_raw(c"background.hlsl".as_ptr() as *const u8),
            None,
            None::<&ID3DInclude>,
            PCSTR::from_raw(entry_z.as_ptr()),
            PCSTR::from_raw(target_z.as_ptr()),
            D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut blob,
            Some(&mut errors),
        )
    };
    if hr.is_err() {
        let detail = errors
            .map(|e| {
                unsafe {
                    String::from_utf8_lossy(std::slice::from_raw_parts(
                        e.GetBufferPointer() as *const u8,
                        e.GetBufferSize(),
                    ))
                }
                .into_owned()
            })
            .unwrap_or_else(|| format!("{hr:?}"));
        return Err(anyhow!("compiling {entry} ({target}): {detail}"));
    }
    blob.context("D3DCompile returned no bytecode")
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}

impl Pipelines {
    fn new(device: &ID3D11Device) -> Result<Self> {
        let quad_vs_blob = compile("quad_vertex", "vs_4_1")?;
        let final_vs_blob = compile("final_vertex", "vs_4_1")?;
        let mesh_vs_blob = compile("mesh_vertex", "vs_4_1")?;
        let layer_blob = compile("layer_fragment", "ps_4_1")?;
        let blur_blob = compile("blur_fragment", "ps_4_1")?;
        let mix_blob = compile("mix_fragment", "ps_4_1")?;
        let final_blob = compile("final_fragment", "ps_4_1")?;
        let copy_blob = compile("copy_fragment", "ps_4_1")?;

        let vertex = |blob: &ID3DBlob| -> Result<ID3D11VertexShader> {
            let mut shader = None;
            unsafe {
                device
                    .CreateVertexShader(blob_bytes(blob), None, Some(&mut shader))
                    .context("creating a vertex shader")?;
            }
            shader.context("no vertex shader")
        };
        let fragment = |blob: &ID3DBlob| -> Result<ID3D11PixelShader> {
            let mut shader = None;
            unsafe {
                device
                    .CreatePixelShader(blob_bytes(blob), None, Some(&mut shader))
                    .context("creating a pixel shader")?;
            }
            shader.context("no pixel shader")
        };

        let mesh_layout = {
            let semantic_uv = c"TEXCOORD";
            let mut elements = [
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR::from_raw(semantic_uv.as_ptr() as *const u8),
                    SemanticIndex: 0,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 8,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR::from_raw(semantic_uv.as_ptr() as *const u8),
                    SemanticIndex: 1,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 16,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
                D3D11_INPUT_ELEMENT_DESC {
                    SemanticName: PCSTR::from_raw(semantic_uv.as_ptr() as *const u8),
                    SemanticIndex: 2,
                    Format: DXGI_FORMAT_R32G32_FLOAT,
                    InputSlot: 0,
                    AlignedByteOffset: 24,
                    InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                    InstanceDataStepRate: 0,
                },
            ];
            let mut layout = None;
            unsafe {
                device
                    .CreateInputLayout(
                        &mut elements,
                        blob_bytes(&mesh_vs_blob),
                        Some(&mut layout),
                    )
                    .context("creating the mesh input layout")?;
            }
            layout.context("no input layout")
        };

        let (mesh_vertices, mesh_indices, index_count) = create_mesh_buffers(device)?;

        Ok(Self {
            quad_vs: vertex(&quad_vs_blob)?,
            final_vs: vertex(&final_vs_blob)?,
            mesh_vs: vertex(&mesh_vs_blob)?,
            layer_ps: fragment(&layer_blob)?,
            blur_ps: fragment(&blur_blob)?,
            mix_ps: fragment(&mix_blob)?,
            final_ps: fragment(&final_blob)?,
            copy_ps: fragment(&copy_blob)?,
            mesh_layout: mesh_layout?,
            mesh_vertices,
            mesh_indices,
            index_count,
        })
    }
}

/// The 6x6 warped grid from `shaders.ts`, with `FROM`/`TO` copied verbatim.
fn create_mesh_buffers(device: &ID3D11Device) -> Result<(ID3D11Buffer, ID3D11Buffer, u32)> {
    const GRID: usize = 6;
    let mut vertices = Vec::with_capacity(GRID * GRID * 8);
    for i in 0..GRID {
        for j in 0..GRID {
            let gx = -1.0 + 2.0 * j as f32 / (GRID - 1) as f32;
            let gy = -1.0 + 2.0 * i as f32 / (GRID - 1) as f32;
            let k = i * GRID + j;
            vertices.extend_from_slice(&[gx, gy]);
            vertices.extend_from_slice(&[(gx + 1.0) / 2.0, (gy + 1.0) / 2.0]);
            vertices.extend_from_slice(&MESH_FROM[k]);
            vertices.extend_from_slice(&MESH_TO[k]);
        }
    }
    let mut indices: Vec<u16> = Vec::with_capacity(150);
    for i in 0..GRID - 1 {
        for j in 0..GRID - 1 {
            let a = (i * GRID + j) as u16;
            let b = (i * GRID + j + 1) as u16;
            let c = ((i + 1) * GRID + j) as u16;
            let d = ((i + 1) * GRID + j + 1) as u16;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    let vertex_buffer = create_buffer(
        device,
        &vertices
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>(),
        D3D11_BIND_VERTEX_BUFFER.0 as u32,
        "mesh vertices",
    )?;
    let index_buffer = create_buffer(
        device,
        &indices
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>(),
        D3D11_BIND_INDEX_BUFFER.0 as u32,
        "mesh indices",
    )?;
    Ok((vertex_buffer, index_buffer, indices.len() as u32))
}

fn create_buffer(
    device: &ID3D11Device,
    bytes: &[u8],
    bind: u32,
    what: &str,
) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: bytes.len() as u32,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: bytes.as_ptr() as *const c_void,
        SysMemPitch: 0,
        SysMemSlicePitch: 0,
    };
    let mut buffer = None;
    unsafe {
        device
            .CreateBuffer(&desc, Some(&data), Some(&mut buffer))
            .with_context(|| format!("creating the {what} buffer"))?;
    }
    buffer.context("CreateBuffer returned no buffer")
}

// ------------------------------------------------------------------ half

/// IEEE 754 binary16 to binary32. `f16` is still unstable, and the field
/// readback only needs this one direction.
fn half_to_f32(h: u16) -> f32 {
    let sign = (h >> 15) & 1;
    let exponent = (h >> 10) & 0x1f;
    let mantissa = h & 0x3ff;
    let magnitude = if exponent == 0 {
        // Subnormal: value = mantissa * 2^-24.
        f32::from(mantissa) * 5.960_464_5e-8
    } else if exponent == 0x1f {
        if mantissa == 0 {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1.0 + f32::from(mantissa) / 1024.0) * (2.0f32).powi(i32::from(exponent) - 15)
    };
    if sign == 1 { -magnitude } else { magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_round_trip_through_the_reference_values() {
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xbc00), -1.0);
        assert_eq!(half_to_f32(0x3800), 0.5);
        assert_eq!(half_to_f32(0x7bff), 65504.0);
        assert!(half_to_f32(0x7c00).is_infinite());
        assert!(half_to_f32(0x7e00).is_nan());
        // Smallest positive subnormal.
        assert_eq!(half_to_f32(0x0001), 5.960_464_5e-8);
    }

    #[test]
    fn the_parameter_block_is_a_multiple_of_sixteen_bytes() {
        assert_eq!(std::mem::size_of::<Params>(), PARAMS_BYTES);
        assert_eq!(PARAMS_BYTES % 16, 0);
    }

    #[test]
    fn the_parameter_block_puts_every_field_at_its_hlsl_offset() {
        let params = Params {
            layer: [1.0, 2.0, 3.0, 4.0],
            final_: [5.0, 6.0, 7.0, 8.0],
            final2: [9.0, 10.0, 11.0, 12.0],
            scrim: [13.0, 14.0, 15.0, 16.0],
            dir: [17.0, 18.0],
            sigma: 19.0,
            mesh: 20.0,
            swap: 21.0,
            alpha: 22.0,
            target: [23.0, 24.0],
        };
        let bytes = params.to_bytes();
        let read = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(read(0), 1.0);
        assert_eq!(read(12), 4.0);
        assert_eq!(read(16), 5.0);
        assert_eq!(read(48), 13.0);
        assert_eq!(read(60), 16.0);
        assert_eq!(read(64), 17.0);
        assert_eq!(read(72), 19.0);
        assert_eq!(read(76), 20.0);
        assert_eq!(read(80), 21.0);
        assert_eq!(read(84), 22.0);
        assert_eq!(read(88), 23.0);
        assert_eq!(read(92), 24.0);
    }

    #[test]
    fn the_mesh_grid_is_the_reference_table() {
        // Spot-check the entries that differ between FROM and TO, plus the
        // corners, against `shaders.ts`.
        assert_eq!(MESH_FROM[0], [-1.0, 1.0]);
        assert_eq!(MESH_FROM[7], [-0.347, 0.2322]);
        assert_eq!(MESH_TO[7], [-0.5126, 0.1216]);
        assert_eq!(MESH_FROM[13], [-0.6, 0.2]);
        assert_eq!(MESH_TO[13], [-0.7012, 0.0426]);
        assert_eq!(MESH_FROM[14], [-0.2, 0.2]);
        assert_eq!(MESH_TO[14], [-0.2, -0.0126]);
        assert_eq!(MESH_FROM[15], [0.2, 0.0194]);
        assert_eq!(MESH_TO[15], [0.3932, -0.032]);
        assert_eq!(MESH_FROM[16], [0.3148, 0.0194]);
        assert_eq!(MESH_TO[16], [0.6278, 0.1044]);
        assert_eq!(MESH_FROM[19], [-0.2458, 0.1232]);
        assert_eq!(MESH_TO[19], [-0.5126, -0.217]);
        assert_eq!(MESH_FROM[20], [0.283, -0.1894]);
        assert_eq!(MESH_TO[20], [0.2828, -0.1512]);
        assert_eq!(MESH_FROM[35], [1.0, -1.0]);
        assert_eq!(MESH_TO[35], [1.0, -1.0]);
    }
}

/// The warped grid, verbatim from `shaders.ts` (`FROM`).
#[rustfmt::skip]
const MESH_FROM: [[f32; 2]; 36] = [
    [-1.0, 1.0], [-0.6, 1.0], [-0.2, 1.0], [0.2, 1.0], [0.6, 1.0], [1.0, 1.0],
    [-1.0, 0.6], [-0.347, 0.2322], [-0.2, 0.6], [-0.076, 0.3152], [0.366, 0.4406], [1.0, 0.6],
    [-1.0, 0.2], [-0.6, 0.2], [-0.2, 0.2], [0.2, 0.0194], [0.3148, 0.0194], [1.2714, 0.2],
    [-1.2346, 0.0806], [-0.2458, 0.1232], [0.283, -0.1894], [0.6508, -0.387], [0.8668, -0.1724], [1.0, -0.2],
    [-1.0874, -0.3066], [-0.6, -0.3236], [0.366, -0.4724], [0.6278, -0.666], [0.8208, -0.617], [1.0, -0.6],
    [-1.0, -1.0], [-0.6, -1.0], [-0.2, -1.0], [0.2, -1.0], [0.6, -1.0], [1.0, -1.0],
];

/// The warped grid, verbatim from `shaders.ts` (`TO`). Identical to
/// [`MESH_FROM`] except at indices 7, 13, 14, 15, 16, 19 and 20.
#[rustfmt::skip]
const MESH_TO: [[f32; 2]; 36] = [
    [-1.0, 1.0], [-0.6, 1.0], [-0.2, 1.0], [0.2, 1.0], [0.6, 1.0], [1.0, 1.0],
    [-1.0, 0.6], [-0.5126, 0.1216], [-0.2, 0.6], [-0.076, 0.3152], [0.366, 0.4406], [1.0, 0.6],
    [-1.0, 0.2], [-0.7012, 0.0426], [-0.2, -0.0126], [0.3932, -0.032], [0.6278, 0.1044], [1.2714, 0.2],
    [-1.2346, 0.0806], [-0.5126, -0.217], [0.2828, -0.1512], [0.6508, -0.387], [0.8668, -0.1724], [1.0, -0.2],
    [-1.0874, -0.3066], [-0.6, -0.3236], [0.366, -0.4724], [0.6278, -0.666], [0.8208, -0.617], [1.0, -0.6],
    [-1.0, -1.0], [-0.6, -1.0], [-0.2, -1.0], [0.2, -1.0], [0.6, -1.0], [1.0, -1.0],
];
