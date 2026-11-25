// Glass pass shaders: the backdrop-filter chain, once per panel.
//
// A panel is rendered as one chain over an intermediate texture the size of
// the captured scene region (panel + sample margin, clamped to the scene
// bounds). The three passes reproduce, per pixel, the reference chain from
// `glass/mod.rs::composite_glass`:
//
//   pass 0: horizontal Gaussian (sigma = blur_px / 2, R = ceil(3*sigma))
//   pass 1: vertical Gaussian
//   pass 2: saturate -> contrast -> tint -> rim -> rounded shape
//
// All coordinates are in region-local px. The scene capture is a straight
// RGBA8 sample of the already-composed frame: no premultiplication happens on
// either side of the chain, matching the CSS compositor's handling of opaque
// window content. Clamp addressing at the region border matches the CPU
// reference (a Gaussian tail truncated at the capture edge).
//
// The blur passes render into RGBA16F so the two Gaussian stages stay float
// like the CPU reference; only the composite quantises to the output format.
//
// The tint and rim endpoints arrive as float4s so the same shader serves the
// popover (0.36 tint) and the window plate (0.3) without recompiling.

#ifndef GLASS_RADIUS
#define GLASS_RADIUS 9
#endif

cbuffer GlassParams : register(b0)
{
  // xy = region size in px.
  float4 p_region;
  // xy = panel rect position inside the region, zw = panel size in px.
  float4 p_panel;
  // xyz = tint colour 0..1, w = tint opacity.
  float4 p_tint;
  // rgb = rim highlight 0..1, a = rim highlight alpha.
  float4 p_rim_hi;
  // rgb = rim shadow 0..1, a = rim shadow alpha.
  float4 p_rim_lo;
  // x = saturation, y = contrast, z = sigma, w = unused.
  float4 p_tuning;
  // xy = blur direction in texels, z = pass id (0 = blur H, 1 = blur V,
  // 2 = composite), w = unused.
  float4 p_pass;
  // x = lens depth px, y = edge band px, z = directional gloss, w = opacity.
  float4 p_liquid;
  float4 p_light; // normalized pointer xy, energy z
};

Texture2D<float4> t_source : register(t0);
Texture2D<float4> t_scene : register(t1);
SamplerState s_source : register(s0);

struct QuadOutput
{
  float4 position : SV_Position;
  float2 uv : TEXCOORD0;
};

// uv(0,0) = top-left of the region; output y down (the composite is consumed
// by the CPU or by another shader, never by the swap chain directly).
QuadOutput quad_vertex(uint vertex_id : SV_VertexID)
{
  float2 corners[4] = {
    float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0), float2(1.0, 1.0)
  };
  float2 corner = corners[vertex_id];
  QuadOutput output;
  output.uv = corner;
  output.position = float4(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
  return output;
}

// Rounded-rect signed distance, matching `rounded_rect_sdf` in glass/mod.rs.
float rounded_rect_sdf(float2 point_px, float2 size_px, float radius)
{
  float r = min(radius, min(size_px.x, size_px.y) * 0.5);
  float2 p = point_px - size_px * 0.5;
  float2 q = abs(p) - (size_px * 0.5 - r);
  float outside = length(max(q, 0.0));
  float inside = min(max(q.x, q.y), 0.0);
  return outside + inside - r;
}

// Pass 0/1: separable Gaussian. Pass 2: the full chain.
float4 glass_fragment(QuadOutput input) : SV_Target
{
  float2 px = input.uv * p_region.xy;
  float4 result;

  if (p_pass.z < 1.5)
  {
    // A blur pass: rebuild the kernel so any sigma works, direction p_pass.xy.
    // Preserves premultiplied RGB and coverage alpha so transparent desktop glass
    // areas stay translucent instead of darkening to opaque black.
    float sigma = max(p_tuning.z, 0.01);
    float4 acc = 0.0;
    float ws = 0.0;
    // Keep 3 sigma support at every DPI, including the wider frosted material.
    int radius = min(64, (int)ceil(3.0 * sigma));
    [loop]
    for (int i = -radius; i <= radius; ++i)
    {
      float w = exp(-0.5 * float(i) * float(i) / (sigma * sigma));
      float2 offset = p_pass.xy * float(i);
      float2 sample_uv = (px + offset) / p_region.xy;
      acc += t_source.Sample(s_source, sample_uv) * w;
      ws += w;
    }
    result = acc / ws;
  }
  else
  {
    // The composite pass: the blurred backdrop is bound as t_source.
    float2 local = px - p_panel.xy;
    float sdf = rounded_rect_sdf(local, p_panel.zw, p_tuning.w);
    float inside = -sdf;
    float4 sampled4 = t_source.Sample(s_source, input.uv);
    float3 sampled = sampled4.rgb;
    float sampled_a = sampled4.a;
    float gloss = 0.0;
    float pointer_spot = 0.0;
    float pointer_key = 0.0;
    if (p_liquid.x > 0.0)
    {
      // The SDF normal follows both straight sides and circular corners.
      float2 n = float2(
        rounded_rect_sdf(local + float2(0.5,0.0), p_panel.zw,p_tuning.w) - rounded_rect_sdf(local - float2(0.5,0.0),p_panel.zw,p_tuning.w),
        rounded_rect_sdf(local + float2(0.0,0.5), p_panel.zw,p_tuning.w) - rounded_rect_sdf(local - float2(0.0,0.5),p_panel.zw,p_tuning.w));
      n = n / max(length(n),0.0001);
      bool is_droplet = p_light.w > 0.5;
      bool is_vertical = p_light.w > 1.5;
      float band_scale = 1.0;
      float2 refract_n = n;
      if (is_droplet)
      {
        // Weight meniscus curvature along the active selector travel axis so
        // horizontal tabs emphasize left/right caps while vertical rails
        // emphasize top/bottom row transitions without smearing side margins.
        float2 axis_w = is_vertical ? float2(0.45, 1.15) : float2(1.12, 0.62);
        refract_n = n * axis_w;
        float rn_len = length(refract_n);
        if (rn_len > 0.0001) { refract_n /= rn_len; }
        band_scale = is_vertical ? (0.58 + 0.52 * abs(n.y)) : (0.64 + 0.44 * abs(n.x));
      }
      float local_band = max(p_liquid.y * band_scale, 2.0);
      float edge = saturate(1.0 - max(inside,0.0) / local_band);
      float e_smooth = smoothstep(0.0, 1.0, edge);
      edge = e_smooth * e_smooth;
      float2 lens_uv = clamp((px - refract_n * p_liquid.x * edge) / p_region.xy,0.5/p_region.xy,1.0-0.5/p_region.xy);
      if (is_droplet || p_tuning.z < 0.8)
      {
        // Selector liquid droplet: crystal-clear interior with curved meniscus
        // refraction, RGB dispersion and soft edge blur around the outer rim.
        float disp = is_droplet ? 0.16 : 0.06;
        float2 uv_r = clamp((px - refract_n * p_liquid.x * edge * (1.0 + disp)) / p_region.xy,0.5/p_region.xy,1.0-0.5/p_region.xy);
        float2 uv_b = clamp((px - refract_n * p_liquid.x * edge * (1.0 - disp)) / p_region.xy,0.5/p_region.xy,1.0-0.5/p_region.xy);
        float4 c_r = t_scene.Sample(s_source, uv_r);
        float4 c_g = t_scene.Sample(s_source, lens_uv);
        float4 c_b = t_scene.Sample(s_source, uv_b);
        float4 b_r = t_source.Sample(s_source, uv_r);
        float4 b_g = t_source.Sample(s_source, lens_uv);
        float4 b_b = t_source.Sample(s_source, uv_b);
        float3 clear_disp = float3(c_r.r, c_g.g, c_b.b);
        float3 blur_disp = float3(b_r.r, b_g.g, b_b.b);
        float rim_blur = is_droplet ? smoothstep(0.18, 0.92, e_smooth) * 0.45 : 0.14;
        sampled = lerp(clear_disp, blur_disp, rim_blur);
        sampled_a = lerp(c_g.a, b_g.a, rim_blur);
      }
      else
      {
        float4 refracted = t_source.Sample(s_source,lens_uv);
        float4 clear_edge = t_scene.Sample(s_source,lens_uv);
        float4 combined = lerp(refracted,clear_edge,0.42*edge);
        sampled = combined.rgb;
        sampled_a = combined.a;
      }
      float key = pow(saturate(dot(n,float2(-0.447214,-0.894427))),3.0);
      float2 light_delta = (local / p_panel.zw - p_light.xy) * float2(p_panel.z / p_panel.w, 1.0);
      pointer_spot = exp(-dot(light_delta,light_delta) / 0.16) * p_light.z;
      float2 light_direction = normalize(float2((p_light.x - 0.5)*1.5, -1.0 + p_light.y*0.5));
      pointer_key = pow(saturate(dot(n,light_direction)),3.0);
      key = lerp(key,pointer_key,p_light.z);
      float bounce = pow(saturate(dot(n,float2(0.707107,0.707107))),5.0);
      gloss = ((key + bounce*0.35)*p_liquid.z + pointer_spot*0.14)*edge;
    }
    // A live-window capture is B8G8R8A8; swap back before touching luma.
    float fg_a = saturate(sampled_a);
    float3 fg_rgb = p_pass.w > 0.5 ? sampled.bgr : sampled;
    if (fg_a > 0.001)
    {
      fg_rgb = saturate(fg_rgb / fg_a);
      // saturate(s) around the Rec.709 luma.
      float luma = dot(fg_rgb, float3(0.2126, 0.7152, 0.0722));
      fg_rgb = luma + (fg_rgb - luma) * p_tuning.x;
      // contrast(c) around mid grey.
      fg_rgb = (fg_rgb - 0.5) * p_tuning.y + 0.5;
    }
    else
    {
      fg_rgb = p_tint.xyz;
    }

    // The tint element (Porter-Duff over the blurred foreground coverage).
    float glass_a = p_tint.w + fg_a * (1.0 - p_tint.w);
    float3 rgb = (glass_a > 0.0001)
      ? (p_tint.xyz * p_tint.w + fg_rgb * fg_a * (1.0 - p_tint.w)) / glass_a
      : p_tint.xyz;
    float out_a = glass_a;

    // The rim: a 1px inset band on the rounded-rect edge, gradient from the
    // highlight (top) to the shadow (bottom). Peaks 0.5px inside the edge.
    if (inside <= 3.2)
    {
      float coverage = p_light.w > 0.5
        ? smoothstep(0.0, 0.95, inside) * smoothstep(3.2, 0.75, inside)
        : saturate(1.0 - abs(inside - 0.5));
      if (coverage > 0.0 && (p_light.w > 0.5 || inside <= 1.5))
      {
        float gradient = 1.0 - (px.y - p_panel.y) / p_panel.w;
        float3 rim = p_rim_lo.rgb + (p_rim_hi.rgb - p_rim_lo.rgb) * gradient;
        float rim_alpha = (p_rim_lo.a + (p_rim_hi.a - p_rim_lo.a) * gradient) * coverage;
        if (p_liquid.x > 0.0 && p_light.z > 0.0) {
          float spec_rim = coverage * (0.06 + 0.22 * pointer_key + 0.16 * pointer_spot);
          if (p_light.w > 0.5) {
            rim_alpha = max(rim_alpha * 0.65, spec_rim * 0.45);
            rim = lerp(rim, p_rim_hi.rgb, saturate(pointer_key * 0.45 + pointer_spot * 0.35) * p_light.z);
          } else {
            rim_alpha = lerp(rim_alpha, spec_rim, p_light.z);
            rim = lerp(rim, p_rim_hi.rgb, p_light.z * 0.35);
          }
        }
        float next_a = rim_alpha + out_a * (1.0 - rim_alpha);
        rgb = (next_a > 0.0001) ? (rim * rim_alpha + rgb * out_a * (1.0 - rim_alpha)) / next_a : rim;
        out_a = next_a;
      }
    }

    // Specular gloss over the glass shell.
    float final_a = gloss + out_a * (1.0 - gloss);
    rgb = (final_a > 0.0001) ? (1.0 * gloss + rgb * out_a * (1.0 - gloss)) / final_a : 1.0;

    // Corner pixels outside the shape stay transparent with smooth derivative AA.
    float aa_w = max(fwidth(sdf), 0.75);
    float corner_alpha = p_liquid.x > 0.0 ? smoothstep(aa_w, -aa_w, sdf) : (sdf > 0.5 ? 0.0 : 1.0);
    corner_alpha *= p_liquid.w * final_a;
    // A caller rendering straight into a B8G8R8A8 surface (the swap chain's
    // own format, which the surface pass samples back as RGBA) needs the
    // channels stored in that order. `p_region.z` selects it per pass.
    if (p_region.z > 0.5)
    {
      rgb = rgb.bgr;
    }
    result = float4(rgb, corner_alpha);
  }

  return result;
}
