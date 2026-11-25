// Task 9: NV12 -> RGB conversion for the motion poster.
//
// The coefficients are NOT baked at compile time: the renderer derives the
// constant block below from the media type's declared matrix and nominal
// range (MF_MT_YUV_MATRIX / MF_MT_VIDEO_NOMINAL_RANGE) and uploads it per
// draw. A stream that decodes as BT.601 converts as BT.601; limited range
// stays limited. See oly-render/src/video.rs, which mirrors this maths on the
// CPU for the colour tests and diagnostics.
//
// The block is four float4s and matches VideoColour::shader_constants():
//
//   g_row_r/g_row_g/g_row_b : the RGB rows of the decode matrix, with
//       coefficients on the *offset-corrected, range-scaled* signals
//       Y' = (Y255 + y_offset) * y_scale, C' = (C255 + c_offset) * c_scale:
//           R = row_r.x*Y' + row_r.y*U' + row_r.z*V'   (and likewise for G/B)
//   g_scale : (y_scale, y_offset, c_scale, c_offset)
//
// The sample planes are R8/R8G8 UNORM, i.e. normalised to 0..1; they are
// scaled back to the 0..255 studio domain before the offsets apply, which is
// exactly what the CPU reference does with raw bytes.

cbuffer VideoConstants : register(b0)
{
    float4 g_row_r; // (1, 0, 2(1-Kr), 0)
    float4 g_row_g; // (1, -2Kb(1-Kb)/Kg, -2Kr(1-Kr)/Kg, 0)
    float4 g_row_b; // (1, 2(1-Kb), 0, 0)
    float4 g_scale; // (y_scale, y_offset, c_scale, c_offset)
};

Texture2D g_luma : register(t0);   // R8_UNORM, full luma plane
Texture2D g_chroma : register(t1); // R8G8_UNORM, interleaved U/V, half size
SamplerState g_sampler : register(s0);

struct VSOutput
{
    float4 position : SV_POSITION;
    float2 uv : TEXCOORD0;
};

struct PSOutput
{
    float4 colour : SV_TARGET0;
};

// Full-screen triangle strip generated from the vertex id: the pass owns no
// vertex buffer and draws exactly the output viewport.
VSOutput video_vertex(uint vertex_id : SV_VertexID)
{
    // 0 -> (-1,-1) uv(0,1)  1 -> (3,-1) uv(2,1)  2 -> (-1,3) uv(0,-1)
    // Covers the full clip space; the rasteriser clips to [0,1].
    float2 pos = float2(
        (vertex_id == 1u) ? 3.0 : -1.0,
        (vertex_id == 2u) ? 3.0 : -1.0);
    VSOutput output;
    output.position = float4(pos, 0.0, 1.0);
    output.uv = float2((pos.x + 1.0) * 0.5, 1.0 - (pos.y + 1.0) * 0.5);
    return output;
}

PSOutput video_fragment(VSOutput input)
{
    PSOutput output;

    // NV12: Y at (x, y), UV at (x/2, y/2) with U and V packed side by side.
    // The video pass samples with a CLAMP-TO-EDGE nearest sampler; the source
    // reader's frame is 1:1 with the target, so there are no resampling
    // artifacts.
    float y255 = g_luma.Sample(g_sampler, input.uv).r * 255.0;
    float2 uv255 = g_chroma.Sample(g_sampler, input.uv).rg * 255.0;

    float yp = (y255 + g_scale.y) * g_scale.x;
    float up = (uv255.x + g_scale.w) * g_scale.z;
    float vp = (uv255.y + g_scale.w) * g_scale.z;

    float r = g_row_r.x * yp + g_row_r.y * up + g_row_r.z * vp;
    float g = g_row_g.x * yp + g_row_g.y * up + g_row_g.z * vp;
    float b = g_row_b.x * yp + g_row_b.y * up + g_row_b.z * vp;

    output.colour = float4(saturate(float3(r, g, b)), 1.0);
    return output;
}
