// Long-note glow: a separable blur of one word's glyph coverage, added behind
// the glyphs.
//
// AMLL draws the held syllable's halo as a CSS `text-shadow` with zero offset:
// `0 0 <blur>em rgba(255, 255, 255, <alpha>)`. A CSS shadow blur radius is
// twice the Gaussian standard deviation, so the shader is given the radius in
// *pixels* and converts once:
//
//     sigma = radius_px / 2
//
// Everything else in the shadow -- the 0.75 alpha scale from the trial profile,
// the radius clamp `min(0.3em, blur * 0.3)`, the per-character fan-out -- is
// evaluated on the CPU by oly-render's emphasis module, because those are
// animation curves, not pixel operations. This file only turns a coverage mask
// and a colour into the halo the curves asked for.
//
// The pass is separable (horizontal then vertical) so an N-tap blur costs 2N
// samples per pixel instead of N*N. Each direction reads the previous stage's
// target and writes a different one; the caller never reads and writes the same
// texture, so no hazard is possible even though the regions come from a pool
// that is reused across frames.

struct GlowInstance {
    // Destination rect in device pixels: x, y, w, h.
    float4 rect;
    // Source rect in the coverage texture, normalized: u0, v0, u1, v1.
    float4 uv;
    // Linear RGBA tint. The reference's halo is pure white; keeping the colour
    // in the instance record means a future theme can warm it without a second
    // shader.
    float4 color;
    // x = blur radius in pixels, y = horizontal (1) or vertical (0) direction,
    // z = tap count (odd, at most GLOW_MAX_TAPS), w = unused.
    float4 params;
};

// Nine taps cover every radius the emphasis curve can produce at the lyric font
// sizes we ship. Beyond that the radius is clamped on the CPU, which matches the
// reference: its shadow never blurs wider than 0.3em.
#define GLOW_MAX_TAPS 9

StructuredBuffer<GlowInstance> glow_items : register(t1);
Texture2D<float> coverage_in : register(t0);
SamplerState coverage_sampler : register(s0);

struct GlowVertex {
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
    nointerpolation float4 color : COLOR0;
    nointerpolation float4 params : TEXCOORD1;
    nointerpolation float4 rect : TEXCOORD2;
};

// unit_vertex is the (0,0) (1,0) (0,1) (1,1) triangle strip corner.
GlowVertex glow_vertex(uint vertex_id : SV_VertexID, uint instance_id : SV_InstanceID)
{
    GlowInstance item = glow_items[instance_id];
    float2 unit = float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
    float2 pixel = item.rect.xy + unit * item.rect.zw;
    float2 ndc = float2(pixel.x * 2.0 / global_viewport_size.x - 1.0,
                        1.0 - pixel.y * 2.0 / global_viewport_size.y);

    GlowVertex out;
    out.position = float4(ndc, 0.0, 1.0);
    out.uv = lerp(item.uv.xy, item.uv.zw, unit);
    out.color = item.color;
    out.params = item.params;
    out.rect = item.rect;
    return out;
}

// One side of the separable kernel. Taps are spaced one sigma apart and the
// weights come from a normalized Gaussian, so the blur's energy is preserved
// and the halo dims as it widens exactly the way the reference's does.
float glow_blur_fragment(GlowVertex input) : SV_Target
{
    float radius = max(input.params.x, 0.001);
    float sigma = radius * 0.5;
    float horizontal = input.params.y;
    int taps = (int)input.params.z;
    if (taps < 1) taps = 1;
    if (taps > GLOW_MAX_TAPS) taps = GLOW_MAX_TAPS;

    // Device-pixel step along the blur axis, expressed in UV of the *source*
    // region so the same code works for a pooled region of any size.
    float2 source_size = max(input.rect.zw, float2(1.0, 1.0));
    float2 uv_step = float2(horizontal, 1.0 - horizontal) / source_size;

    int half_taps = taps / 2;
    float weight_sum = 0.0;
    float coverage = 0.0;
    float two_sigma_sq = 2.0 * sigma * sigma;

    [unroll]
    for (int i = -half_taps; i <= half_taps; i++) {
        float distance_px = (float)i * sigma;
        float weight = exp(-(distance_px * distance_px) / two_sigma_sq);
        coverage += coverage_in.Sample(coverage_sampler, input.uv + uv_step * (float)i).r * weight;
        weight_sum += weight;
    }

    coverage /= max(weight_sum, 1e-5);
    // The blurred coverage is the halo's shape; the colour and alpha come from
    // the animation curve, applied in the composite pass.
    return coverage;
}

// The halo itself: coverage times tint. Drawn before the glyphs, so the glyphs
// cover the middle of the halo and only the fringe is visible -- which is what
// a text-shadow does, and why the stem cannot thicken.
float4 glow_composite_fragment(GlowVertex input) : SV_Target
{
    float coverage = coverage_in.Sample(coverage_sampler, input.uv).r;
    // Alpha accumulates additively against the scene behind the text, matching
    // the reference's light-on-dark shadow instead of occluding it.
    float alpha = saturate(coverage * input.color.a);
    return float4(input.color.rgb * alpha, alpha);
}
