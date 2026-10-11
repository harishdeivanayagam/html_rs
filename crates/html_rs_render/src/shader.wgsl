// html.rs quad pipeline: instanced rounded rectangles with SDF anti-aliasing
// and SDF border rings. All coordinates are physical pixels, y-down.

struct Camera {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

// Instance attributes are flattened (WGSL entry points only accept scalar and
// vector varyings, not structs):
//   location 1: pos_size  — xy = top-left (px), zw = width/height (px)
//   location 2: color     — linear RGBA, premultiplied on output
//   location 3: params    — x = corner radius (px), y = border width (px)
//   location 4: border_color — linear RGBA

struct VSOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) quad_size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) params: vec2<f32>,
    @location(4) border_color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) corner: vec2<f32>, // unit quad corner in {0,1}²
    @location(1) q_pos_size: vec4<f32>,
    @location(2) q_color: vec4<f32>,
    @location(3) q_params: vec4<f32>,
    @location(4) q_border_color: vec4<f32>,
) -> VSOut {
    let px = q_pos_size.xy + corner * q_pos_size.zw;
    let ndc = vec2<f32>(
        px.x / camera.size.x * 2.0 - 1.0,
        1.0 - px.y / camera.size.y * 2.0,
    );
    var out: VSOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.local = corner * q_pos_size.zw;
    out.quad_size = q_pos_size.zw;
    out.color = q_color;
    out.params = q_params.xy;
    out.border_color = q_border_color;
    return out;
}

fn rounded_box_sdf(p: vec2<f32>, half_size: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half_size + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let half_size = in.quad_size * 0.5;
    let p = in.local - half_size;
    let radius = min(in.params.x, min(half_size.x, half_size.y));

    // 1px feather for smooth edges without MSAA.
    let d = rounded_box_sdf(p, half_size - vec2<f32>(0.5, 0.5), radius);
    let alpha = 1.0 - smoothstep(-0.5, 0.5, d);
    if (alpha <= 0.001) {
        discard;
    }

    var color = in.color;
    let bw = in.params.y;
    if (bw > 0.0) {
        let d_inner = rounded_box_sdf(
            p,
            max(half_size - vec2<f32>(0.5 + bw, 0.5 + bw), vec2<f32>(0.0, 0.0)),
            max(radius - bw, 0.0),
        );
        let inner_alpha = 1.0 - smoothstep(-0.5, 0.5, d_inner);
        let is_border = 1.0 - inner_alpha;
        color = mix(color, in.border_color, is_border);
    }

    // Premultiplied alpha.
    return vec4<f32>(color.rgb * alpha * color.a, alpha * color.a);
}
