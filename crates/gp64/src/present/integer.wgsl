// Mirrored by source_coordinate in gpu.rs. Integer division deliberately uses
// the pixel's left/top edge, exactly like the CPU Integer blitter.
fn source_coordinate(pixel: vec2<u32>, source: vec2<u32>) -> vec2<u32> {
    return (pixel - geometry.origin) * source / geometry.extent;
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let at = source_coordinate(vec2<u32>(position.xy), textureDimensions(guest));
    return output(pixel(at));
}
