// Per-glyph colouring for lyric text.
//
// One glyph quad, one texture sample, one mask evaluation. The glyph atlas
// holds coverage only (R8); the colour comes from the instance record, so a
// word that is half lit never needs two overlapping text layers and the stem
// cannot thicken.
//
// The mask is the reference's per-word gradient: a bright->dark ramp of
// `fade_width` pixels whose left edge sits at `mask_position +
// left_percent * mask_width`. `mask_position` is animated per word by
// oly-render's mask module, which reproduces AMLL's keyframe timeline exactly.

struct TextGlyphInstance {
    // Destination rect in device pixels: x, y, w, h.
    float4 rect;
    // Source rect in the atlas, normalized: u0, v0, u1, v1.
    float4 uv;
    // Linear RGBA colour.
    float4 color;
    // Mask state: x = mask position (px), y = band start relative to the glyph
    // quad's left edge (px), z = band width (px), w = unlit alpha.
    float4 mask;
    // x = lit alpha, y = 1 when the mask applies, z = 1 for colour (emoji)
    // glyphs that ignore the ramp, w = unused.
    float4 flags;
};

StructuredBuffer<TextGlyphInstance> glyphs : register(t1);
Texture2D<float> atlas : register(t0);
SamplerState atlas_sampler : register(s0);

struct GlyphVertex {
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
    nointerpolation float4 color : COLOR0;
    nointerpolation float4 mask : TEXCOORD1;
    nointerpolation float4 flags : TEXCOORD2;
};

// unit_vertex is the (0,0) (1,0) (0,1) (1,1) triangle strip corner.
GlyphVertex text_mask_vertex(uint vertex_id : SV_VertexID, uint instance_id : SV_InstanceID)
{
    TextGlyphInstance item = glyphs[instance_id];
    float2 unit = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    float2 pixel = item.rect.xy + unit * item.rect.zw;
    float2 ndc = float2(pixel.x * 2.0 / viewport_size.x - 1.0,
                        1.0 - pixel.y * 2.0 / viewport_size.y);

    GlyphVertex out;
    out.position = float4(ndc, 0.0, 1.0);
    out.uv = lerp(item.uv.xy, item.uv.zw, unit);
    out.color = item.color;
    out.mask = item.mask;
    out.flags = item.flags;
    return out;
}

float text_mask_alpha(float local_x, float4 mask, float4 flags)
{
    if (flags.y <= 0.0 || flags.z > 0.0) {
        // No mask (idle line, translation sub-row) or a colour glyph: one alpha
        // for the whole quad.
        return flags.y <= 0.0 ? 1.0 : flags.x;
    }
    float t = saturate((local_x - (mask.x + mask.y)) / max(mask.z, 1e-4));
    return lerp(flags.x, mask.w, t);
}

float4 text_mask_fragment(GlyphVertex input) : SV_Target
{
    float coverage = atlas.Sample(atlas_sampler, input.uv);
    float alpha = text_mask_alpha(input.uv.x * 0.0 + input.position.x, input.mask, input.flags);
    // Coverage is sampled exactly once: the mask multiplies coverage, it never
    // re-samples or re-blends a second text layer.
    return float4(input.color.rgb, input.color.a * coverage * alpha);
}
