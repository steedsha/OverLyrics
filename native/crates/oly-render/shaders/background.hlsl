// The am_bg v36 immersion background, as HLSL.
//
// One-for-one port of `src/windows/lyrics/background/shaders.ts` at commit
// 708f6b1, which is itself the extraction of
// `am_bg/deliverable/apple_music_immersion_import.html`
// (sha256 82706b03…). Every numeric constant below is copied from that file,
// not re-derived: a "tidier" value here is a fidelity regression.
//
// ## What each pass does
//
//   layer  draws the 200x200 source through three rotated/scaled copies into
//          `acc`. `acc` is never cleared between steps, so the copies that the
//          `discard` leaves out keep the previous step's pixels. That feedback
//          is the slow smear the effect is built on.
//   mesh   deforms `acc` through a 6x6 warped grid into `msh` (cleared black).
//   blurX  separable gaussian, sigma 16, direction (1/200, 0).
//   blurY  same kernel, direction (0, 1/200) -> `b2`, the field the final pass
//          samples.
//   mix    `b2` back over `acc` with alpha 1, so the next step's layer pass
//          starts from the blurred field instead of the raw one.
//   final  lift / saturation / gain in float, scrim, then the 1-LSB dither as
//          the very last operation, and only then the 8-bit conversion.
//
// ## Coordinate conventions (checked against the GLSL, one by one)
//
// WebGL and D3D11 disagree on where row 0 of a render target sits, and the
// reference relies on that row order in three places: the source upload, the
// `acc` feedback, and `gl.readPixels` in the QA harness. The rule used here is
//
//     device_position = float4(ndc.x, -ndc.y, 0, 1)      uv unchanged
//
// which makes every *intermediate* pass store the same bytes in the same row
// order as WebGL: the pass that wrote `acc` and the pass that samples it agree
// about which row is which, so the feedback loop is preserved exactly. The
// consequence is that a raw buffer from this renderer is row 0 = the *top* of
// the window, while WebGL's `readPixels` returns row 0 = the *bottom*
// (`tests/background_parity` flips before comparing, and says so).
//
// The final pass is the one exception: its output leaves the pipeline, and a
// GPUI custom surface shows row 0 at the top of the window while WebGL shows
// framebuffer row 0 at the bottom. So `final_vertex` does *not* negate y, and
// the fragment that WebGL put at the bottom of the window lands in the last row
// here. `uv` still means the same thing in both, so the body of `final_fragment`
// is a literal translation; only the vertex position differs.
//
// Everything else carries over unchanged because the reference uses no
// matrices, no texture matrices and no sRGB views:
//
//   * sampling centres: `Sample` on a 200x200 target at uv=(i+0.5)/200 hits the
//     same texel centre the GL linear filter hits at the same uv;
//   * linear vs sRGB: the source texture is `R8G8B8A8_UNORM` and the four
//     intermediates are `R16G16B16A16_FLOAT`; there is no `_SRGB` format
//     anywhere, so no implicit decode/encode happens in either API;
//   * premultiplied alpha: the intermediates carry alpha 1 everywhere (the
//     layer and blur passes write 1, the mix pass writes 1), and the only blend
//     in the chain is the final composite over the previous scene, which uses
//     `SRC_ALPHA/ONE_MINUS_SRC_ALPHA` on straight alpha — same as WebGL;
//   * edge mode: both sampler states are `CLAMP_TO_EDGE` + `LINEAR`;
//   * `fract`, `sin`, `exp`, `dot`, `clamp` and `mix` are the HLSL intrinsics
//     with the same definitions as the GLSL ones used here (`fract(x)` is
//     `x - floor(x)` in both, which matters for negative inputs in the dither).

// ---------------------------------------------------------------- constants

static const float SIZE = 200.0;

static const int BLUR_RADIUS = 32;
static const float BLUR_CUTOFF = 0.0015;

// The final pass's vertical slice is `Y0..Y0+KEEP` of a 200px field, which is
// 43.75..156.25 — the upstream HTML derives those from an 800-px design space
// (`175*SIZE/800`, `450*SIZE/800`). The Rust side multiplies the sampling
// fractions by SIZE; they are spelled out here only so a reviewer can check the
// numbers against `background/presentation.rs`.
//
// The three layer copies are not constants here: `p_layer` is uploaded once per
// copy by the Rust driver, which owns the table
// `[omega 8.3333333, centre .5 .5, scale 1], [16.6666667, 0 0, .82],
//  [4.1666667, .75 .75, 1]`. Keeping one copy of that table (in Rust, where the
// per-pass uniforms are built) avoids two sources of truth drifting apart.

// ------------------------------------------------------------------ cbuffer
//
// One shared block. A separate block per pass would be six more objects to keep
// in sync with Rust for no measurable gain, and the reference uploads these as
// loose uniforms anyway.

cbuffer BackgroundParams : register(b0)
{
    float4 p_layer;   // x: angle (radians), y: cx, z: cy, w: scale
    float4 p_final;   // x: post-process on, y: lift, z: saturation, w: gain
    float4 p_final2;  // x: y0, y: keep, z: size, w: opacity
    float4 p_scrim;   // rgb: three-stop scrim, a: unused
    float2 p_dir;     // blur direction, in uv units
    float p_sigma;    // blur sigma, in uv units * size
    float p_mesh;     // mesh morph, 0..1
    float p_swap;     // 1 -> swap r/b after sampling
    float p_alpha;    // mix pass alpha
    float2 p_target;  // present target size in pixels, for gl_FragCoord parity
};

// ------------------------------------------------------------------ samplers

Texture2D t_source : register(t0);
SamplerState s_linear_clamp : register(s0);

// Vertex outputs carry uv only; positions are computed per pass.
struct QuadVertexOut
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

// --------------------------------------------------------------- vertex: quad

// A 4-vertex triangle strip, addressed by `SV_VertexID`. The uv table is the
// reference's `quad` buffer ([-1,-1,0,0] [1,-1,1,0] [-1,1,0,1] [1,1,1,1]).
QuadVertexOut quad_vertex(uint vertex_id : SV_VertexID)
{
    static const float2 positions[4] = {
        float2(-1.0, -1.0), float2(1.0, -1.0),
        float2(-1.0, 1.0),  float2(1.0, 1.0)
    };
    static const float2 uvs[4] = {
        float2(0.0, 0.0), float2(1.0, 0.0),
        float2(0.0, 1.0), float2(1.0, 1.0)
    };
    QuadVertexOut output;
    float2 p = positions[vertex_id];
    // See the header: negating y reproduces WebGL's row order.
    output.position = float4(p.x, -p.y, 0.0, 1.0);
    output.uv = uvs[vertex_id];
    return output;
}

// ------------------------------------------------------------ vertex: present

// The final pass writes the texture that leaves the pipeline, and that texture
// is shown row 0 = top. So this vertex does *not* negate y (see the header):
// uv.y = 1 must land on the top of the window, which is where WebGL showed it.
QuadVertexOut final_vertex(uint vertex_id : SV_VertexID)
{
    static const float2 positions[4] = {
        float2(-1.0, -1.0), float2(1.0, -1.0),
        float2(-1.0, 1.0),  float2(1.0, 1.0)
    };
    static const float2 uvs[4] = {
        float2(0.0, 0.0), float2(1.0, 0.0),
        float2(0.0, 1.0), float2(1.0, 1.0)
    };
    QuadVertexOut output;
    output.position = float4(positions[vertex_id], 0.0, 1.0);
    output.uv = uvs[vertex_id];
    return output;
}

// --------------------------------------------------------------- vertex: mesh

// The grid position (`aPos`) is implied by the quad index buffer, so the input
// layout starts at the uv attribute; the vertex buffer still carries the
// position first, and the layout's offsets skip it.
struct MeshVertexIn
{
    float2 uv : TEXCOORD0;
    float2 from : TEXCOORD1;
    float2 to : TEXCOORD2;
};

QuadVertexOut mesh_vertex(MeshVertexIn input)
{
    QuadVertexOut output;
    // VS_MESH: `gl_Position = vec4(mix(aFrom, aTo, uV), 0, 1)`.
    float2 p = lerp(input.from, input.to, p_mesh);
    output.position = float4(p.x, -p.y, 0.0, 1.0);
    output.uv = input.uv;
    return output;
}

// ------------------------------------------------------------------- layer

// FS_LAYER. Rotates the sampling coordinate about (cx, cy), scales it, and
// discards anything that falls outside the source. The discard is load-bearing:
// the pixels it skips keep whatever `acc` held, which is the whole feedback
// mechanism.
float4 layer_fragment(QuadVertexOut input) : SV_Target
{
    float2 p = (input.uv - p_layer.yz) * SIZE;
    float c = cos(-p_layer.x);
    float s = sin(-p_layer.x);
    float2 q = float2(c * p.x - s * p.y, s * p.x + c * p.y);
    float2 uv = q / (SIZE * p_layer.w) + p_layer.yz;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0)
    {
        discard;
    }
    float3 colour = t_source.Sample(s_linear_clamp, uv).rgb;
    if (p_swap > 0.5)
    {
        colour = colour.bgr;
    }
    // Convert sRGB source to linear light so feedback and Gaussian blur
    // blend optically without dark/muddy gamma-space transition dips.
    colour = pow(max(colour, 0.0), 2.2);
    return float4(colour, 1.0);
}

// -------------------------------------------------------------------- blur

// FS_BLUR. A symmetric gaussian truncated at R=32 taps with the reference's
// early-out. `p_sigma` is in the same units as the tap index, so sigma=16 means
// 16 texels.
float4 blur_fragment(QuadVertexOut input) : SV_Target
{
    float3 acc = t_source.Sample(s_linear_clamp, input.uv).rgb;
    float weight_sum = 1.0;
    [loop]
    for (int i = 1; i <= BLUR_RADIUS; ++i)
    {
        float x = (float)i;
        float w = exp(-0.5 * x * x / (p_sigma * p_sigma));
        if (w < BLUR_CUTOFF)
        {
            break;
        }
        acc += t_source.Sample(s_linear_clamp, input.uv + p_dir * x).rgb * w;
        acc += t_source.Sample(s_linear_clamp, input.uv - p_dir * x).rgb * w;
        weight_sum += 2.0 * w;
    }
    return float4(acc / weight_sum, 1.0);
}

// --------------------------------------------------------------------- mix

// FS_MIX. Copies `b2` back over `acc`; the reference enables
// SRC_ALPHA/ONE_MINUS_SRC_ALPHA with alpha 1, which is an overwrite. The blend
// state is configured by the caller so the pass stays a byte-for-byte match
// even if the alpha is ever changed.
float4 mix_fragment(QuadVertexOut input) : SV_Target
{
    return float4(t_source.Sample(s_linear_clamp, input.uv).rgb, p_alpha);
}

// ------------------------------------------------------------------- final

// FS_FINAL. This is where the 8-bit output is produced, and the ordering is
// part of the look:
//
//   1. sample the 200px field at the vertical slice the window asks for;
//   2. lift the black point, saturate, apply the per-cover gain, clamp to 1;
//   3. multiply by the three-stop scrim ramp (float, not after quantisation);
//   4. add the interleaved-gradient dither of +/- 1/510 — the *last* operation;
//   5. clamp and let the UNORM target round to 8 bits.
//
// `gl_FragCoord.xy` in the reference is in pixels with (0.5, 0.5) at the centre
// of the bottom-left texel. The dither is a function of pixel coordinates only,
// so the exact origin does not change its statistics, but it does change the
// pattern: the offset below reproduces the WebGL phase (x+0.5, y+0.5) with the
// row order the header describes.
float4 final_fragment(QuadVertexOut input) : SV_Target
{
    float2 uv = float2(input.uv.x, (p_final2.x + input.uv.y * p_final2.y) / p_final2.z);

    // Dynamic breathing multiplier passed in p_scrim.a (0 -> default 1.0).
    // p_layer carries (sim_time_sec, mono_factor, beat_flash, beat_wobble) in final pass.
    float breath = p_scrim.a > 0.01 ? p_scrim.a : 1.0;
    float t_ribbon = p_layer.x;
    float mono = clamp(p_layer.y, 0.0, 1.0);
    float beat = clamp(p_layer.z, 0.0, 2.5);
    float wobble = clamp(p_layer.w, -1.25, 2.5);

    float screen_y = 1.0 - input.uv.y;
    float screen_x = input.uv.x;

    // Subwoofer air-vibration: visible radial elastic expansion around the album
    // cover + smooth fluid UV wobble before sampling the blurred color-blob field.
    float2 uv_sample = uv;
    if (abs(wobble) > 0.001)
    {
        float2 from_orig = float2(screen_x - 0.26, screen_y - 0.48);
        float2 expand_uv = -from_orig * (wobble * 0.11);
        float2 fluid_uv = float2(
            sin(screen_y * 3.8 + t_ribbon * 1.9),
            cos(screen_x * 3.2 - t_ribbon * 1.5)
        ) * (wobble * 0.028);
        float v_scale = p_final2.y / max(p_final2.z, 1.0);
        uv_sample = clamp(
            uv + float2(expand_uv.x + fluid_uv.x, (-expand_uv.y + fluid_uv.y) * v_scale),
            0.0,
            1.0
        );
    }

    float3 c = max(t_source.Sample(s_linear_clamp, uv_sample).rgb, 0.0);

    if (p_final.x > 0.5)
    {
        // Lift & optional saturation in linear space if explicitly requested.
        if (p_final.y > 1e-4)
        {
            float y0 = dot(c, float3(0.2126, 0.7152, 0.0722));
            if (y0 < p_final.y)
            {
                float3 neutral = float3(p_final.y, p_final.y, p_final.y);
                float3 lifted = c * (p_final.y / max(y0, 1e-4));
                float trust = smoothstep(0.0002, max(0.0003, min(p_final.y, 0.015)), y0);
                c = lerp(neutral, lifted, trust);
            }
        }
        if (abs(p_final.z - 1.0) > 1e-4)
        {
            float y = dot(c, float3(0.2126, 0.7152, 0.0722));
            c = max((c - y) * p_final.z + y, 0.0);
        }
        c *= p_final.w * breath;
    }

    // Mega-scale, non-repeating drifting light/shadow clouds (larger than the
    // viewport so monochrome covers like brat get broad stage lighting with zero
    // visible sine-wave stripes). Clouds also expand & surge elastically with `wobble`.
    float2 cloud_hi = float2(
        0.34 + 0.26 * sin(t_ribbon * 0.21 + 0.5),
        0.32 + 0.22 * cos(t_ribbon * 0.17 + 1.1)
    );
    float2 cloud_lo = float2(
        0.68 + 0.24 * cos(t_ribbon * 0.19 - 0.7),
        0.70 + 0.20 * sin(t_ribbon * 0.15 + 2.3)
    );
    float cloud_scale = max(1.0 - wobble * 0.22, 0.55);
    float2 push_dir = normalize(float2(screen_x - 0.26, screen_y - 0.48) + float2(1e-4, 1e-4));
    float2 p_wob = float2(screen_x, screen_y) - push_dir * (wobble * 0.045);
    float2 d_hi = (p_wob - cloud_hi) * float2(0.85, 1.15) * cloud_scale;
    float2 d_lo = (p_wob - cloud_lo) * float2(0.90, 1.10) * cloud_scale;
    float hi_wash = exp(-dot(d_hi, d_hi) * 1.45);
    float lo_wash = exp(-dot(d_lo, d_lo) * 1.35);
    float cloud_mod = (hi_wash * 0.65 - lo_wash * 0.50);
    if (cloud_mod > 0.0)
    {
        float2 lyr_cloud_d = float2((screen_x - 0.67) * 1.35, (screen_y - 0.50) * 1.15);
        float cloud_pos_damp = 1.0 - 0.45 * exp(-dot(lyr_cloud_d, lyr_cloud_d) * 2.5);
        cloud_mod *= cloud_pos_damp;
    }
    float cloud_amp = lerp(0.16, 0.44, mono) + abs(wobble) * 0.06;
    c *= max(1.0 + cloud_mod * cloud_amp, 0.25);

    // Intensity-graded Option 1 beat flash (toggled via 'B', scaled by panel strength):
    // Softer peripheral glow (+28% outer edge, +12% cover halo at 1.0x) paired with the
    // visible subwoofer blob wobble above, keeping right-hand lyrics rock-steady.
    if (beat > 0.001)
    {
        float flash = beat;
        float2 edge_c = float2(screen_x - 0.5, screen_y - 0.5) * float2(1.08, 1.05);
        float edge_r = length(edge_c);
        float edge_mask = smoothstep(0.46 - min(flash, 1.8) * 0.16, 0.88, edge_r);
        float bottom_bias = 0.85 + 0.24 * smoothstep(0.30, 1.0, screen_y);

        float2 lyr_d = float2((screen_x - 0.67) * 1.35, (screen_y - 0.50) * 1.15);
        float lyr_protect = 1.0 - 0.84 * exp(-dot(lyr_d, lyr_d) * 3.2);

        float2 cov_d = float2((screen_x - 0.24) * 1.45, (screen_y - 0.44) * 1.15);
        float cov_halo = exp(-dot(cov_d, cov_d) * max(2.5 - wobble * 0.90, 0.85));

        float local_boost = (edge_mask * bottom_bias * 0.28 + cov_halo * 0.12) * lyr_protect;
        c *= 1.0 + flash * local_boost;

        float vib_boost = (edge_mask * 0.15 + cov_halo * 0.08) * lyr_protect;
        float yb = dot(c, float3(0.2126, 0.7152, 0.0722));
        c = max((c - yb) * (1.0 + flash * vib_boost) + yb, 0.0);
    }

    // Soft filmic shoulder roll-off in linear light to keep bright glows silky.
    c = c / (1.0 + 0.14 * c);
    // Back to sRGB display space.
    c = pow(clamp(c, 0.0, 1.0), 1.0 / 2.2);

    // Vertical adaptive scrim + subtle left-cover halo / right-lyrics calm balance.
    float shade = screen_y < 0.65
        ? lerp(p_scrim.x, p_scrim.y, screen_y / 0.65)
        : lerp(p_scrim.y, p_scrim.z, (screen_y - 0.65) / 0.35);

    // Subtle radial vignette (corners ~8% darker) for depth framing.
    float2 centered = float2(screen_x - 0.5, screen_y - 0.5) * float2(1.15, 1.0);
    float vignette = smoothstep(0.25, 0.85, length(centered)) * 0.09;

    // Gentle left-cover ambient halo (+4%) vs calm right-lyrics reading zone (-3%).
    float2 cover_dist = float2((screen_x - 0.24) * 1.4, (screen_y - 0.44) * 1.1);
    float cover_halo = exp(-dot(cover_dist, cover_dist) * 2.4) * 0.045;
    float right_calm = smoothstep(0.42, 0.85, screen_x) * 0.035;

    c *= clamp(1.0 - shade - vignette - right_calm + cover_halo, 0.0, 1.12);

    // Interleaved Gradient Noise (IGN) + subtle tactile film grain (~1.5 LSB)
    // eliminates 8-bit banding and adds frosted-velvet surface texture.
    float2 frag = float2(input.position.x, p_target.y - input.position.y);
    float ign1 = frac(52.9829189 * frac(dot(frag, float2(0.06711056, 0.00583715))));
    float ign2 = frac(52.9829189 * frac(dot(frag + float2(17.0, 31.0), float2(0.06711056, 0.00583715))));
    float tri_noise = ign1 + ign2 - 1.0; // triangular PDF in [-1, 1]
    c += tri_noise * (1.6 / 255.0);

    return float4(clamp(c, 0.0, 1.0), p_final2.w);
}

// --------------------------------------------------------- opaque copy helper

// FS_COPY, used by the mesh pass. Kept separate from `mix_fragment` so the
// mesh pass writes alpha 1 exactly like the reference's `FS_COPY`.
float4 copy_fragment(QuadVertexOut input) : SV_Target
{
    return float4(t_source.Sample(s_linear_clamp, input.uv).rgb, 1.0);
}
