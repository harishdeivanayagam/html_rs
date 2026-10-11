// Textured-quad pipeline for <img> elements.
// Instances carry the destination rect; UVs span the whole texture.
// Rounded corners are cut in the fragment shader via an SDF.

struct Camera {
    size: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var t_sampler: sampler;
@group(1) @binding(1) var t_tex: texture_2d<f32>;

struct VSOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) params: vec4<f32>, // (size.x, size.y, radius, 0)
};

@vertex
fn vs_main(
    @location(0) corner: vec2<f32>,
    @location(1) uv_in: vec2<f32>,
    @location(2) pos_size: vec4<f32>,
    @location(3) params: vec4<f32>,
) -> VSOut {
    let size = pos_size.zw;
    var out: VSOut;
    out.pos = vec4<f32>(pos_size.xy + corner * size, 0.0, 1.0);
    out.uv = uv_in;
    out.local = corner * size;
    out.params = vec4<f32>(size, params.x, 0.0);
    return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let r = in.params.z;
    var alpha = 1.0;
    if (r > 0.0) {
        let half_size = in.params.xy / 2.0;
        let q = abs(in.local - half_size) - (half_size - vec2<f32>(r, r));
        let outside = length(max(q, vec2<f32>(0.0, 0.0)));
        let inside = min(max(q.x, q.y), 0.0);
        let d = outside + inside - r;
        alpha = clamp(0.5 - d, 0.0, 1.0);
    }
    var color = textureSample(t_tex, t_sampler, in.uv);
    color.a *= alpha;
    return color;
}
