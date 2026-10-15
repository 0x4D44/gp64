struct Geometry {
    origin: vec2<u32>,
    extent: vec2<u32>,
};
@group(0) @binding(0) var guest: texture_2d<f32>;
@group(0) @binding(1) var<uniform> geometry: Geometry;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(positions[index], 0.0, 1.0);
}

// Mirrored by source_coordinate in gpu.rs. Integer division deliberately uses
// the pixel's left/top edge, exactly like the CPU Integer blitter.
fn source_coordinate(pixel: vec2<u32>, source: vec2<u32>) -> vec2<u32> {
    return (pixel - geometry.origin) * source / geometry.extent;
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let at = source_coordinate(vec2<u32>(position.xy), textureDimensions(guest));
    return vec4<f32>(textureLoad(guest, vec2<i32>(at), 0).rgb, 1.0);
}
