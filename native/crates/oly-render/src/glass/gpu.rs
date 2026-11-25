//! The D3D11 side of the local glass pass.
//!
//! [`super::composite_glass`] is the software reference: it computes the exact
//! chain (blur → saturate → contrast → tint → rim → shape) that the CSS
//! backdrop-filter stack produces. This module runs the same chain on the GPU
//! as three passes over an intermediate texture:
//!
//! 1. horizontal Gaussian (sigma from the material),
//! 2. vertical Gaussian,
//! 3. composite: saturate → contrast → tint → rim → rounded shape.
//!
//! The panel is drawn into an offscreen RGBA8 texture sized to the *region*
//! (panel + sample margin). Reading the region back and cropping the panel
//! rect yields the same bytes the software reference produces; the parity test
//! (`glass_pass.rs`) does exactly that against `composite_glass` on synthetic
//! scenes.
//!
//! Like the background GPU module, this is one file holding all unsafe calls
//! for the glass pass.

#![allow(unsafe_code)]

use std::ffi::c_void;

use anyhow::{anyhow, Context, Result};
use windows::core::PCSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::{
    D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3,
};
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, ID3DInclude, D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1, D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext,
    ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11SamplerState,
    ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader, D3D11_BIND_CONSTANT_BUFFER,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BLEND_DESC, D3D11_BLEND_ONE,
    D3D11_BLEND_OP_ADD, D3D11_BLEND_ZERO, D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL,
    D3D11_COMPARISON_ALWAYS, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CULL_NONE, D3D11_FILL_SOLID, D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_MAP_READ,
    D3D11_RASTERIZER_DESC, D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};

use super::{Material, Rect};

/// The shader source is compiled at runtime with `D3DCompile` (see the module
/// documentation of `background::gpu` for why).
const SHADER_SRC: &str = include_str!("../../shaders/glass.hlsl");

/// One panel render request: the material, the panel rect in scene px, and the
/// scene capture (already cropped to the region: panel + sample margin,
/// clamped to scene bounds).
pub struct GlassRequest<'a> {
    pub material: &'a Material,
    pub panel: Rect,
    pub region: (u32, u32),
    pub scene: &'a [u8],
}

impl<'a> GlassRequest<'a> {
    /// The expected byte length of `scene`.
    pub fn scene_len(&self) -> usize {
        (self.region.0 * self.region.1 * 4) as usize
    }
}

/// GPU constants, `#[repr(C)]`, 144 bytes (9 × float4).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Params {
    region: [f32; 4],
    panel: [f32; 4],
    tint: [f32; 4],
    rim_hi: [f32; 4],
    rim_lo: [f32; 4],
    tuning: [f32; 4],
    pass: [f32; 4],
    liquid: [f32; 4],
    light: [f32; 4],
}

const PARAMS_BYTES: usize = std::mem::size_of::<Params>();

impl Params {
    fn to_bytes(&self) -> [u8; PARAMS_BYTES] {
        let mut out = [0u8; PARAMS_BYTES];
        for (group, slot) in [
            self.region,
            self.panel,
            self.tint,
            self.rim_hi,
            self.rim_lo,
            self.tuning,
            self.pass,
            self.liquid,
            self.light,
        ]
        .iter()
        .enumerate()
        {
            for (c, value) in slot.iter().enumerate() {
                out[group * 16 + c * 4..group * 16 + c * 4 + 4]
                    .copy_from_slice(&value.to_le_bytes());
            }
        }
        out
    }
}

/// One intermediate texture with its views.
struct Target {
    _texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    rtv: ID3D11RenderTargetView,
}

/// Pipelines and vertex data shared by every panel render.
struct Pipelines {
    quad_vs: ID3D11VertexShader,
    glass_ps: ID3D11PixelShader,
}

/// The glass pass renderer. Owns its own D3D11 device when built with
/// [`GlassRenderer::new_own`] (tests, offline QA) or borrows an external one
/// with [`GlassRenderer::from_device`] (the eventual GPUI integration).
pub struct GlassRenderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    pipelines: Pipelines,
    params: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    liquid: [f32; 4],
    light: [f32; 4],
    blend_opaque: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    /// Region-sized intermediates, recreated when the region size changes.
    /// The two blur intermediates are RGBA16F so the separable kernel is
    /// quantised once (at the composite) exactly like the CPU reference; the
    /// output is RGBA8, the format the caller consumes.
    scratch: [Option<Target>; 2],
    output: Option<(ID3D11Texture2D, ID3D11RenderTargetView)>,
    scratch_size: (u32, u32),
}

impl Drop for GlassRenderer {
    fn drop(&mut self) {
        // Release the scratch targets before the device goes away.
        self.scratch = [None, None];
        self.output = None;
    }
}

/// Compiles the two entry points and creates the shader objects.
fn make_pipelines(device: &ID3D11Device) -> Result<Pipelines> {
    let quad_vs_blob = compile("quad_vertex", "vs_4_1")?;
    let glass_ps_blob = compile("glass_fragment", "ps_4_1")?;
    let mut quad_vs = None;
    unsafe {
        device
            .CreateVertexShader(blob_bytes(&quad_vs_blob), None, Some(&mut quad_vs))
            .context("creating the glass vertex shader")?;
    }
    let mut glass_ps = None;
    unsafe {
        device
            .CreatePixelShader(blob_bytes(&glass_ps_blob), None, Some(&mut glass_ps))
            .context("creating the glass pixel shader")?;
    }
    Ok(Pipelines {
        quad_vs: quad_vs.context("no glass vertex shader")?,
        glass_ps: glass_ps.context("no glass pixel shader")?,
    })
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}

impl GlassRenderer {
    /// Builds a renderer on its own hardware D3D11 device (tests, QA).
    pub fn new_own() -> Result<Self> {
        let mut device = None;
        let mut context = None;
        let levels = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
        ];
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None::<*mut D3D_FEATURE_LEVEL>,
                Some(&mut context),
            )
            .context("creating a D3D11 device for the glass pass")?;
        }
        let device = device.context("D3D11CreateDevice returned no device")?;
        let context = context.context("D3D11CreateDevice returned no context")?;
        Ok(Self::from_device_inner(device, context))
    }

    /// Builds a renderer over an existing device/context pair.
    pub fn from_device(device: ID3D11Device, context: ID3D11DeviceContext) -> Self {
        Self::from_device_inner(device, context)
    }

    fn from_device_inner(device: ID3D11Device, context: ID3D11DeviceContext) -> Self {
        let pipelines = make_pipelines(&device).expect("creating the glass pipelines");
        let params = create_params(&device).expect("creating the glass constant buffer");
        let sampler = create_sampler(&device).expect("creating the glass sampler");
        let blend_opaque = create_blend(&device).expect("creating the glass blend state");
        let raster = create_rasterizer(&device).expect("creating the glass rasterizer");
        Self {
            device,
            context,
            pipelines,
            params,
            sampler,
            liquid: [0.0, 12.0, 0.0, 1.0],
            light: [0.25, 0.05, 0.0, 0.0],
            blend_opaque,
            raster,
            scratch: [None, None],
            output: None,
            scratch_size: (0, 0),
        }
    }

    /// A normalized local light position and its energy.
    pub fn set_light(&mut self, x: f32, y: f32, energy: f32) {
        self.light = [
            x.clamp(-0.25, 1.25),
            y.clamp(-0.25, 1.25),
            energy.clamp(0.0, 1.0),
            self.light[3],
        ];
    }

    /// Selects liquid optical mode (`0` = panel glass, `1` = horizontal selector droplet, `2` = vertical selector droplet).
    pub fn set_lens_mode(&mut self, mode: f32) {
        self.light[3] = mode;
    }

    /// Configure scene refraction in physical pixels. Zero depth keeps the legacy pass.
    pub fn set_liquid(&mut self, depth: f32, band: f32, shine: f32, opacity: f32) {
        self.liquid = [
            depth.clamp(0.0, 24.0),
            band.clamp(1.0, 64.0),
            shine.clamp(0.0, 0.5),
            opacity.clamp(0.0, 1.0),
        ];
    }

    /// Renders one panel. Returns nothing; call [`GlassRenderer::read_output`]
    /// to fetch the composited region.
    pub fn render(&mut self, request: &GlassRequest<'_>) -> Result<()> {
        if request.scene.len() != request.scene_len() {
            return Err(anyhow!(
                "scene capture is {} bytes, expected {} for {:?}",
                request.scene.len(),
                request.scene_len(),
                request.region
            ));
        }
        let scene = self.upload_scene(request)?;
        self.render_source(request, scene.as_ref(), false)
    }

    /// Renders one panel from a texture that already holds the scene capture.
    ///
    /// A live window produces its backdrop as a GPU texture, so a panel can
    /// filter the frame it is drawn over with no CPU readback. `source_bgra`
    /// marks the texture as `B8G8R8A8` (the swap chain's format) so the
    /// composite pass restores channel order before computing luma.
    pub fn render_from_scene_texture(
        &mut self,
        request: &GlassRequest<'_>,
        scene: &ID3D11ShaderResourceView,
        source_bgra: bool,
    ) -> Result<()> {
        self.render_source(request, Some(scene), source_bgra)
    }

    /// Renders one panel into a caller-owned render target.
    ///
    /// A live window composites its panels through the custom-surface pass,
    /// which samples the panel's texture verbatim. That texture lives on the
    /// swap chain's format (`B8G8R8A8`), so the caller owns the target and the
    /// composite pass writes straight into it — no CPU round trip, and no
    /// extra copy from an RGBA8 intermediate.
    pub fn render_from_scene_texture_into(
        &mut self,
        request: &GlassRequest<'_>,
        scene: &ID3D11ShaderResourceView,
        source_bgra: bool,
        output: &ID3D11RenderTargetView,
    ) -> Result<()> {
        self.render_passes(request, Some(scene), source_bgra, Some(output))
    }

    fn render_source(
        &mut self,
        request: &GlassRequest<'_>,
        scene: Option<&ID3D11ShaderResourceView>,
        source_bgra: bool,
    ) -> Result<()> {
        self.render_passes(request, scene, source_bgra, None)
    }

    fn render_passes(
        &mut self,
        request: &GlassRequest<'_>,
        scene: Option<&ID3D11ShaderResourceView>,
        source_bgra: bool,
        output: Option<&ID3D11RenderTargetView>,
    ) -> Result<()> {
        self.ensure_scratch(request.region)?;

        let params = self.params_for(request, 0.0, 1.0);
        // Pass 0: horizontal blur into scratch[0].
        self.run_pass(
            &params,
            [1.0, 0.0],
            0.0,
            scene,
            &self.scratch[0].as_ref().unwrap().rtv,
        )?;
        // Pass 1: vertical blur into scratch[1].
        let pass1 = Params {
            pass: [0.0, 1.0, 0.0, 1.0],
            ..params
        };
        let scratch0_srv = self.scratch[0].as_ref().unwrap().srv.clone();
        self.run_pass(
            &pass1,
            [0.0, 1.0],
            1.0,
            Some(&scratch0_srv),
            &self.scratch[1].as_ref().unwrap().rtv,
        )?;
        // Pass 2: composite. `pass.w` carries the capture's channel order, which
        // survives the per-channel blur. A caller-supplied target is written
        // directly so a composited window panel needs no extra copy.
        let pass2 = Params {
            pass: [0.0, 0.0, 2.0, if source_bgra { 1.0 } else { 0.0 }],
            ..params
        };
        let scratch1_srv = self.scratch[1].as_ref().unwrap().srv.clone();
        let owned;
        let output_rtv = match output {
            Some(target) => target,
            None => {
                self.ensure_output(request.region)?;
                owned = self.output.as_ref().unwrap().1.clone();
                &owned
            }
        };
        // The edge lens samples the actual unblurred scene as well as the
        // frosted buffer. It never samples foreground text painted afterward.
        unsafe {
            self.context
                .PSSetShaderResources(1, Some(&[scene.cloned()]));
        }
        let result = self.run_pass(&pass2, [0.0, 0.0], 2.0, Some(&scratch1_srv), output_rtv);
        unsafe {
            self.context.PSSetShaderResources(1, Some(&[None]));
        }
        result?;
        Ok(())
    }

    /// The composited panel texture, so a caller can composite it directly
    /// instead of reading the region back.
    pub fn output_texture(&self) -> Option<&ID3D11Texture2D> {
        self.output.as_ref().map(|(texture, _)| texture)
    }

    /// Reads the composited region (row 0 at the top) as RGBA8.
    pub fn read_output(&mut self, region: (u32, u32)) -> Result<Vec<u8>> {
        let target = &self
            .output
            .as_ref()
            .context("no rendered output to read")?
            .0;
        let staging = create_staging(&self.device, region.0, region.1)?;
        unsafe {
            self.context.CopyResource(&staging, target);
            let mut mapped = Default::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("mapping the glass staging texture")?;
            let mut pixels = vec![0u8; (region.0 * region.1 * 4) as usize];
            for y in 0..region.1 {
                let src = (mapped.pData as *const u8).add((y * mapped.RowPitch) as usize);
                let dst =
                    &mut pixels[(y * region.0 * 4) as usize..((y + 1) * region.0 * 4) as usize];
                std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), dst.len());
            }
            self.context.Unmap(&staging, 0);
            Ok(pixels)
        }
    }

    /// Uploads the scene capture into a scratch-sized texture and returns its
    /// SRV. The texture lives for this call only (the upload is per render).
    fn upload_scene(&self, request: &GlassRequest<'_>) -> Result<Option<ID3D11ShaderResourceView>> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: request.region.0,
            Height: request.region.1,
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
            pSysMem: request.scene.as_ptr() as *const c_void,
            SysMemPitch: request.region.0 * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, Some(&data), Some(&mut texture))
                .context("uploading the glass scene capture")?;
        }
        let texture = texture.context("CreateTexture2D returned no scene texture")?;
        let mut view = None;
        unsafe {
            self.device
                .CreateShaderResourceView(&texture, None, Some(&mut view))
                .context("creating the scene SRV")?;
        }
        Ok(Some(view.context("no scene SRV")?))
    }

    fn params_for(&self, request: &GlassRequest<'_>, _elapsed: f64, _opacity: f64) -> Params {
        let material = request.material;
        Params {
            region: [request.region.0 as f32, request.region.1 as f32, 0.0, 0.0],
            panel: [
                request.panel.left as f32,
                request.panel.top as f32,
                request.panel.width as f32,
                request.panel.height as f32,
            ],
            tint: [
                material.tint[0] as f32,
                material.tint[1] as f32,
                material.tint[2] as f32,
                material.tint_opacity as f32,
            ],
            rim_hi: [
                material.rim_hi[0] as f32,
                material.rim_hi[1] as f32,
                material.rim_hi[2] as f32,
                material.rim_hi[3] as f32,
            ],
            rim_lo: [
                material.rim_lo[0] as f32,
                material.rim_lo[1] as f32,
                material.rim_lo[2] as f32,
                material.rim_lo[3] as f32,
            ],
            tuning: [
                material.saturation as f32,
                material.contrast as f32,
                material.blur_sigma() as f32,
                material.radius as f32,
            ],
            pass: [1.0, 0.0, 0.0, 0.0],
            liquid: self.liquid,
            light: self.light,
        }
    }

    /// Runs one pass into `target`, binding `source` (or none for pass 0's
    /// special case of an uploaded scene).
    fn run_pass(
        &self,
        params: &Params,
        _direction: [f32; 2],
        pass_index: f64,
        source: Option<&ID3D11ShaderResourceView>,
        target: &ID3D11RenderTargetView,
    ) -> Result<()> {
        let (w, h) = (self.scratch_size.0, self.scratch_size.1);
        unsafe {
            self.context
                .OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            self.context
                .ClearRenderTargetView(target, &[0.0, 0.0, 0.0, 1.0]);
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w as f32,
                Height: h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            self.context.RSSetState(&self.raster);
            self.context
                .OMSetBlendState(&self.blend_opaque, None, u32::MAX);
            self.upload(params)?;

            self.context.VSSetShader(&self.pipelines.quad_vs, None);
            self.context.PSSetShader(&self.pipelines.glass_ps, None);
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            if let Some(view) = source {
                self.context
                    .PSSetShaderResources(0, Some(&[Some(view.clone())]));
            }
            self.context
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.context.Draw(4, 0);
            // Unbind to avoid a feedback loop on the next pass.
            self.context.PSSetShaderResources(0, Some(&[None]));
        }
        let _ = pass_index;
        Ok(())
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

    fn ensure_output(&mut self, region: (u32, u32)) -> Result<()> {
        if self.output.is_none() || self.scratch_size != region {
            let (texture, rtv) = create_output_target(&self.device, region.0, region.1)?;
            self.output = Some((texture, rtv));
        }
        Ok(())
    }

    fn ensure_scratch(&mut self, region: (u32, u32)) -> Result<()> {
        if self.scratch_size != region {
            let a = create_target(&self.device, region.0, region.1)?;
            let b = create_target(&self.device, region.0, region.1)?;
            let out = create_target_rgba8(&self.device, region.0, region.1)?;
            self.scratch = [Some(a), Some(b)];
            self.output = Some(out);
            self.scratch_size = region;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ helpers

fn create_target(device: &ID3D11Device, width: u32, height: u32) -> Result<Target> {
    // RGBA16F: the blur intermediates must not quantise between the two
    // passes (the CPU reference keeps full precision until the final store).
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
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
            .context("creating a glass scratch target")?;
    }
    let texture = texture.context("CreateTexture2D returned no target")?;
    let mut view = None;
    unsafe {
        device
            .CreateShaderResourceView(&texture, None, Some(&mut view))
            .context("creating the scratch SRV")?;
    }
    let srv = view.context("no scratch SRV")?;
    let mut rtv = None;
    unsafe {
        device
            .CreateRenderTargetView(&texture, None, Some(&mut rtv))
            .context("creating the scratch RTV")?;
    }
    Ok(Target {
        _texture: texture,
        srv,
        rtv: rtv.context("no scratch RTV")?,
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
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .context("creating the glass output target")?;
    }
    let texture = texture.context("CreateTexture2D returned no output")?;
    let mut rtv = None;
    unsafe {
        device
            .CreateRenderTargetView(&texture, None, Some(&mut rtv))
            .context("creating the glass output RTV")?;
    }
    Ok((texture, rtv.context("no glass output RTV")?))
}

/// The final RGBA8 output target for the composite pass.
fn create_output_target(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<(ID3D11Texture2D, ID3D11RenderTargetView)> {
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
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .context("creating the glass output target")?;
    }
    let texture = texture.context("CreateTexture2D returned no output texture")?;
    let mut rtv = None;
    unsafe {
        device
            .CreateRenderTargetView(&texture, None, Some(&mut rtv))
            .context("creating the glass output RTV")?;
    }
    Ok((texture, rtv.context("no output RTV")?))
}

fn create_staging(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
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
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut texture = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .context("creating the glass staging texture")?;
    }
    texture.context("CreateTexture2D returned no staging texture")
}

fn create_params(device: &ID3D11Device) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: ((PARAMS_BYTES + 15) / 16 * 16) as u32,
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
            .context("creating the glass constant buffer")?;
    }
    buffer.context("CreateBuffer returned no constant buffer")
}

fn create_sampler(device: &ID3D11Device) -> Result<ID3D11SamplerState> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D11_COMPARISON_ALWAYS,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: f32::MAX,
    };
    let mut sampler = None;
    unsafe {
        device
            .CreateSamplerState(&desc, Some(&mut sampler))
            .context("creating the glass sampler")?;
    }
    sampler.context("CreateSamplerState returned no sampler")
}

fn create_blend(device: &ID3D11Device) -> Result<ID3D11BlendState> {
    let rt = D3D11_RENDER_TARGET_BLEND_DESC {
        BlendEnable: false.into(),
        SrcBlend: D3D11_BLEND_ONE,
        DestBlend: D3D11_BLEND_ZERO,
        BlendOp: D3D11_BLEND_OP_ADD,
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
            .context("creating the glass blend state")?;
    }
    state.context("CreateBlendState returned no state")
}

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
            .context("creating the glass rasterizer")?;
    }
    state.context("CreateRasterizerState returned no rasterizer")
}

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
            PCSTR::from_raw(c"glass.hlsl".as_ptr() as *const u8),
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
            .map(|e| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    e.GetBufferPointer() as *const u8,
                    e.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_else(|| format!("{hr:?}"));
        return Err(anyhow!("compiling {entry} ({target}): {detail}"));
    }
    blob.context("D3DCompile returned no bytecode")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_params_block_is_nine_floating_point_vec4s() {
        assert_eq!(PARAMS_BYTES, 144);
        assert_eq!(PARAMS_BYTES % 16, 0);
    }

    #[test]
    fn the_params_block_serialises_in_declaration_order() {
        let params = Params {
            region: [1.0, 2.0, 3.0, 4.0],
            panel: [5.0, 6.0, 7.0, 8.0],
            tint: [9.0, 10.0, 11.0, 12.0],
            rim_hi: [13.0, 14.0, 15.0, 16.0],
            rim_lo: [17.0, 18.0, 19.0, 20.0],
            tuning: [21.0, 22.0, 23.0, 24.0],
            pass: [25.0, 0.0, 1.0, 2.0],
            liquid: [3.0, 12.0, 0.16, 1.0],
            light: [0.25, 0.05, 0.8, 0.0],
        };
        let bytes = params.to_bytes();
        for (index, value) in [
            1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0,
            16.0, 17.0, 18.0, 19.0, 20.0, 21.0, 22.0, 23.0, 24.0, 25.0, 0.0, 1.0, 2.0, 3.0, 12.0,
            0.16, 1.0, 0.25, 0.05, 0.8, 0.0,
        ]
        .iter()
        .enumerate()
        {
            let stored = f32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap());
            assert_eq!(stored, *value, "slot {index}");
        }
    }
}
