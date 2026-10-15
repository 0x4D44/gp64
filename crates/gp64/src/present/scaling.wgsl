// Shared by the three exact integer-arithmetic scalers. The requested device's
// default 2D texture limit (8192) bounds coordinate products below 2^32.
struct Geometry {
    origin: vec2<u32>,
    extent: vec2<u32>,
    reciprocal: vec2<u32>,
    dim: u32,
    padding: u32,
};
@group(0) @binding(0) var guest: texture_2d<f32>;
@group(0) @binding(1) var<uniform> geometry: Geometry;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(positions[index], 0.0, 1.0);
}

fn pixel(at: vec2<u32>) -> vec3<u32> {
    return vec3<u32>(textureLoad(guest, vec2<i32>(at), 0).rgb * 255.0 + 0.5);
}

fn output(rgb: vec3<u32>) -> vec4<f32> {
    var channels = rgb;
    if geometry.dim != 0u {
        channels = (channels * 154u) >> vec3<u32>(8u);
    }
    return vec4<f32>(vec3<f32>(channels) / 255.0, 1.0);
}

// Unsigned 64-bit arithmetic, low word first. No optional GPU int64 feature.
fn wide_add(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let low = a.x + b.x;
    return vec2<u32>(low, a.y + b.y + select(0u, 1u, low < a.x));
}

fn wide_mul(a: u32, b: u32) -> vec2<u32> {
    let al = a & 65535u;
    let bl = b & 65535u;
    let ah = a >> 16u;
    let bh = b >> 16u;
    let low = al * bl;
    let middle = ah * bl + (low >> 16u);
    let upper = al * bh + (middle & 65535u);
    return vec2<u32>((upper << 16u) | (low & 65535u),
        ah * bh + (middle >> 16u) + (upper >> 16u));
}

fn wide_product(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let base = wide_mul(a.x, b.x);
    return vec2<u32>(base.x, base.y + a.x * b.y + a.y * b.x);
}
