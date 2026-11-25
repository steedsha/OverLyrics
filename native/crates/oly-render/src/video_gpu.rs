//! Task 10: the GPU NV12 → BGRA8 video pass.
//!
//! [`VideoRenderer`] converts one NV12 frame into a BGRA8 texture **on the
//! render device**, so the fork's `register_gpu_surface` can composite it into
//! the GPUI scene without any CPU readback. The conversion maths comes from
//! [`VideoColour::shader_constants`], which the Task 9 CPU tests already prove
//! against the media type's declared matrix and range; this pass only wires
//! the same constants into `shaders/video.hlsl` on the GPU.
//!
//! This is the *decode-to-composite* half of the motion cover. The decode
//! half lives in `oly-platform-win` and is proven bit-exact against FFmpeg.

#![allow(unsafe_code)]

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D::Fxc::{
    D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, D3D_SRV_DIMENSION_TEXTURE2D, ID3DBlob, ID3DInclude,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::core::PCSTR;

use super::video::{ColourSpace, VideoColour};

const VIDEO_SHADER: &str = include_str!("../shaders/video.hlsl");

/// The D3D11 handle types this module hands out, re-exported so the
/// application crate can name them without a second `windows` dependency
/// version. They are the same types GPUI's platform window returns from
/// `gpu_device_handle` (both crates resolve to windows 0.62 in this graph).
pub use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device as VideoDevice, ID3D11DeviceContext as VideoContext,
    ID3D11Texture2D as VideoTexture,
};

/// GPU NV12 → BGRA8 pass on one device.
///
/// The renderer keeps the input/output textures and the pipeline alive between
/// frames; nothing is allocated per frame. [`VideoRenderer::render`] saves and
/// restores the pipeline state it touches, so the pass can run between GPUI
/// draws without corrupting the scene pipeline.
pub struct VideoRenderer {
    width: u32,
    height: u32,

    luma: ID3D11Texture2D,
    luma_srv: ID3D11ShaderResourceView,
    chroma: ID3D11Texture2D,
    chroma_srv: ID3D11ShaderResourceView,

    output: ID3D11Texture2D,
    output_rtv: ID3D11RenderTargetView,

    vertex: ID3D11VertexShader,
    fragment: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
}

impl VideoRenderer {
    /// Creates the pass for a `width × height` stream. The declared colour
    /// facts select the conversion constants — never a fixed BT.601
    /// assumption.
    pub fn new(
        device: &ID3D11Device,
        _context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
        space: &ColourSpace,
    ) -> Result<Self> {
        anyhow::ensure!(
            width > 0 && height > 0 && width % 2 == 0 && height % 2 == 0,
            "video size {width}x{height} must be nonzero and even (NV12)"
        );

        // NV12 planes: luma R8 full size, chroma R8G8 half size.
        let plane_desc = |format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
                          w: u32,
                          h: u32|
         -> D3D11_TEXTURE2D_DESC {
            D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            }
        };
        let luma = create_texture(device, &plane_desc(DXGI_FORMAT_R8_UNORM, width, height))?;
        let chroma = create_texture(
            device,
            &plane_desc(DXGI_FORMAT_R8G8_UNORM, width / 2, height / 2),
        )?;

        // The composite target the fork's surface pipeline samples: BGRA8
        // (the window render target's family), bindable both as a surface
        // source and as this pass's render target.
        let mut output_desc = plane_desc(DXGI_FORMAT_B8G8R8A8_UNORM, width, height);
        output_desc.BindFlags =
            (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32;
        let output = create_texture(device, &output_desc)?;

        let luma_srv = create_srv(device, &luma, DXGI_FORMAT_R8_UNORM)?;
        let chroma_srv = create_srv(device, &chroma, DXGI_FORMAT_R8G8_UNORM)?;
        let output_rtv = {
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            unsafe {
                device
                    .CreateRenderTargetView(&output, None, Some(&mut rtv))
                    .context("creating the video output render target view")?;
            }
            rtv.context("CreateRenderTargetView returned no view")?
        };

        // video.hlsl: vertex shader is a vertex-id triangle strip (no vertex
        // buffer); pixel shader is the proven colour maths.
        let vertex_blob = compile("video_vertex", "vs_4_0")?;
        let fragment_blob = compile("video_fragment", "ps_4_0")?;
        let vertex = {
            let mut s: Option<ID3D11VertexShader> = None;
            unsafe {
                device
                    .CreateVertexShader(blob_bytes(&vertex_blob), None, Some(&mut s))
                    .context("creating the video vertex shader")?;
            }
            s.context("CreateVertexShader returned no shader")?
        };
        let fragment = {
            let mut s: Option<ID3D11PixelShader> = None;
            unsafe {
                device
                    .CreatePixelShader(blob_bytes(&fragment_blob), None, Some(&mut s))
                    .context("creating the video pixel shader")?;
            }
            s.context("CreatePixelShader returned no shader")?
        };

        // Clamp-to-edge, nearest: the frame is 1:1 with the target, so
        // nearest preserves the studio values exactly.
        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
        };
        let sampler = {
            let mut s: Option<ID3D11SamplerState> = None;
            unsafe {
                device
                    .CreateSamplerState(&sampler_desc, Some(&mut s))
                    .context("creating the video sampler")?;
            }
            s.context("CreateSamplerState returned no state")?
        };

        // One 16-float constant buffer, filled by set_colour_space.
        let constants = create_constant_buffer(device)?;

        let mut renderer = Self {
            width,
            height,
            luma,
            luma_srv,
            chroma,
            chroma_srv,
            output,
            output_rtv,
            vertex,
            fragment,
            sampler,
            constants,
        };
        renderer.set_colour_space(space)?;
        Ok(renderer)
    }

    /// The stream's colour facts changed (new source): update the constant
    /// block. Everything derives from Kr/Kb and the nominal range.
    pub fn set_colour_space(&mut self, space: &ColourSpace) -> Result<()> {
        let constants = VideoColour::new(space).shader_constants();
        let device = unsafe { self.luma.GetDevice() }
            .context("video luma texture lost its device")?;
        let context = unsafe { device.GetImmediateContext() }
            .context("no immediate context from the video device")?;
        unsafe {
            context.UpdateSubresource(
                &self.constants,
                0,
                None,
                constants.as_ptr() as *const _,
                0,
                0,
            );
        }
        Ok(())
    }

    /// Uploads one NV12 frame from tightly packed rows (no pitch padding):
    /// `luma` must be `w*h` bytes, `chroma` `(w/2)*(h/2)*2`.
    pub fn upload_nv12(
        &self,
        context: &ID3D11DeviceContext,
        luma: &[u8],
        chroma: &[u8],
    ) -> Result<()> {
        let luma_expected = self.width as usize * self.height as usize;
        let chroma_expected = (self.width as usize / 2) * (self.height as usize / 2) * 2;
        anyhow::ensure!(
            luma.len() == luma_expected,
            "luma plane is {} bytes, expected {luma_expected}",
            luma.len()
        );
        anyhow::ensure!(
            chroma.len() == chroma_expected,
            "chroma plane is {} bytes, expected {chroma_expected}",
            chroma.len()
        );
        unsafe {
            context.UpdateSubresource(
                &self.luma,
                0,
                None,
                luma.as_ptr() as *const _,
                self.width,
                0,
            );
            context.UpdateSubresource(
                &self.chroma,
                0,
                None,
                chroma.as_ptr() as *const _,
                self.width / 2 * 2, // row pitch: one UV pair per chroma texel
                0,
            );
        }
        Ok(())
    }

    /// Renders the uploaded frame to the BGRA8 output texture and returns a
    /// clone of its handle (the texture stays owned here).
    ///
    /// State hygiene: the previous render target, viewport and PS SRV slots
    /// are saved and restored, because this pass may run between GPUI draws
    /// (via the fork's paint hook) or inside a QA harness.
    pub fn render(&self, context: &ID3D11DeviceContext) -> Result<ID3D11Texture2D> {
        self.draw(
            context,
            &[
                self.luma_srv.clone(),
                self.chroma_srv.clone(),
            ],
        )?;
        Ok(self.output.clone())
    }

    /// Renders a decoder-owned NV12 texture (Task 9's `VideoSample.texture`,
    /// same device, same size) straight to the output: the two plane views
    /// are created over the incoming texture, so there is no CPU staging and
    /// no upload anywhere on this path.
    ///
    /// NV12 plane views are the documented D3D11 pattern for planar formats:
    /// an SRV with `DXGI_FORMAT_R8_UNORM` reads the luma plane, one with
    /// `DXGI_FORMAT_R8G8_UNORM` the chroma plane. The views are per-call —
    /// each decoded sample carries its own texture — which is two view
    /// creations per presented frame, not per pixel.
    pub fn render_nv12_texture(
        &self,
        context: &ID3D11DeviceContext,
        nv12: &ID3D11Texture2D,
    ) -> Result<ID3D11Texture2D> {
        let device =
            unsafe { self.output.GetDevice() }.context("video output texture lost its device")?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { nv12.GetDesc(&mut desc) };
        anyhow::ensure!(
            desc.Format == windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12,
            "sample texture format is {:?}, expected NV12",
            desc.Format
        );
        anyhow::ensure!(
            desc.Width == self.width && desc.Height == self.height,
            "sample texture is {}x{}, renderer built for {}x{}",
            desc.Width,
            desc.Height,
            self.width,
            self.height
        );
        let luma_srv = create_srv(&device, nv12, DXGI_FORMAT_R8_UNORM)?;
        let chroma_srv = create_srv(&device, nv12, DXGI_FORMAT_R8G8_UNORM)?;
        self.draw(context, &[luma_srv, chroma_srv])?;
        Ok(self.output.clone())
    }

    /// The shared full-screen draw: save the pipeline state the pass touches,
    /// bind the given plane SRVs, draw, restore.
    fn draw(
        &self,
        context: &ID3D11DeviceContext,
        srvs: &[ID3D11ShaderResourceView],
    ) -> Result<()> {
        let mut prev_rtv: [Option<ID3D11RenderTargetView>; 1] = [None];
        let mut prev_depth: Option<ID3D11DepthStencilView> = None;
        let mut prev_viewports = [D3D11_VIEWPORT::default(); 16];
        let mut vp_count: u32 = 16;
        let mut prev_srvs: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
        unsafe {
            context.OMGetRenderTargets(Some(&mut prev_rtv), Some(&mut prev_depth));
            context.RSGetViewports(&mut vp_count, Some(prev_viewports.as_mut_ptr()));
            context.PSGetShaderResources(0, Some(&mut prev_srvs));

            context.OMSetRenderTargets(Some(&[Some(self.output_rtv.clone())]), None);
            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.width as f32,
                Height: self.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));

            context.VSSetShader(&self.vertex, None);
            context.PSSetShader(&self.fragment, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            let bound: [Option<ID3D11ShaderResourceView>; 2] =
                [Some(srvs[0].clone()), Some(srvs[1].clone())];
            context.PSSetShaderResources(0, Some(&bound));
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            context.Draw(4, 0);

            // Restore what the caller had.
            context.PSSetShaderResources(0, Some(&prev_srvs));
            context.RSSetViewports(Some(&prev_viewports[..vp_count as usize]));
            context.OMSetRenderTargets(Some(&prev_rtv), prev_depth.as_ref());
        }
        Ok(())
    }

    /// The BGRA8 output texture, for `register_gpu_surface` on the window's
    /// device. Stable for the renderer's lifetime; re-register after device
    /// loss (the fork clears its registry then).
    pub fn output_texture(&self) -> ID3D11Texture2D {
        self.output.clone()
    }

    /// The output texture's dimensions (equals the stream size).
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Reads a BGRA8 texture back to tightly packed rows (staging copy + Map).
/// QA/capture helper: never on the production present path.
pub fn readback_bgra8(texture: &ID3D11Texture2D) -> Result<(Vec<u8>, u32, u32)> {
    let device = unsafe { texture.GetDevice() }.context("texture lost its device")?;
    let context = unsafe { device.GetImmediateContext() }.context("no immediate context")?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };

    let staging_desc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..desc.clone()
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe {
        device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
            .context("creating the readback staging texture")?;
    }
    let staging = staging.context("CreateTexture2D returned no staging texture")?;
    unsafe { context.CopyResource(&staging, texture) };

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .context("mapping the staging texture")?;
    }
    let row_bytes = desc.Width as usize * 4;
    let mut out = Vec::with_capacity(row_bytes * desc.Height as usize);
    let src = mapped.pData as *const u8;
    for y in 0..desc.Height as usize {
        let row =
            unsafe { std::slice::from_raw_parts(src.add(y * mapped.RowPitch as usize), row_bytes) };
        out.extend_from_slice(row);
    }
    unsafe { context.Unmap(&staging, 0) };
    Ok((out, desc.Width, desc.Height))
}

fn create_texture(device: &ID3D11Device, desc: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D> {
    let mut texture: Option<ID3D11Texture2D> = None;
    unsafe {
        device
            .CreateTexture2D(desc, None, Some(&mut texture))
            .context("creating a video texture")?;
    }
    texture.context("CreateTexture2D returned no texture")
}

fn create_srv(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
) -> Result<ID3D11ShaderResourceView> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    let mut srv: Option<ID3D11ShaderResourceView> = None;
    unsafe {
        device
            .CreateShaderResourceView(texture, Some(&desc), Some(&mut srv))
            .context("creating a video SRV")?;
    }
    srv.context("CreateShaderResourceView returned no view")
}

fn create_constant_buffer(device: &ID3D11Device) -> Result<ID3D11Buffer> {
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: 16 * 4,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
        StructureByteStride: 0,
    };
    let mut buffer: Option<ID3D11Buffer> = None;
    unsafe {
        device
            .CreateBuffer(&desc, None, Some(&mut buffer))
            .context("creating the video constant buffer")?;
    }
    buffer.context("CreateBuffer returned no buffer")
}

/// Compiles one entry of `shaders/video.hlsl`, reporting D3D's error log on
/// failure (the background pass's proven pattern).
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
            VIDEO_SHADER.as_ptr() as *const _,
            VIDEO_SHADER.len(),
            PCSTR::from_raw(c"video.hlsl".as_ptr() as *const u8),
            None,
            None::<&ID3DInclude>,
            PCSTR::from_raw(entry_z.as_ptr() as *const u8),
            PCSTR::from_raw(target_z.as_ptr() as *const u8),
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
        return Err(anyhow::anyhow!("compiling {entry} ({target}): {detail}"));
    }
    blob.context("D3DCompile returned no bytecode")
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}
