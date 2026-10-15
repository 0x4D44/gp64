// scaler.rs::bilinear_axis: endpoint alignment, then nearest 16-bit fraction.
fn axis(at: u32, source: u32, extent: u32) -> vec2<u32> {
    let den = max(extent - 1u, 1u);
    let num = at * (source - 1u);
    return vec2<u32>(num / den, (((num % den) << 16u) + den / 2u) / den);
}

fn horizontal(at: vec2<u32>, fraction: u32, last: u32) -> vec3<u32> {
    return pixel(at) * (65536u - fraction)
        + pixel(vec2<u32>(min(at.x + 1u, last), at.y)) * fraction;
}

fn vertical(upper: u32, lower: u32, fraction: u32) -> u32 {
    let sum = wide_add(wide_mul(upper, 65536u - fraction), wide_mul(lower, fraction));
    // One rounding after both axes, exactly (sum + 2^31) >> 32.
    return wide_add(sum, vec2<u32>(0x80000000u, 0u)).y;
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let local = vec2<u32>(position.xy) - geometry.origin;
    let source = textureDimensions(guest);
    let x = axis(local.x, source.x, geometry.extent.x);
    let y = axis(local.y, source.y, geometry.extent.y);
    let upper = horizontal(vec2<u32>(x.x, y.x), x.y, source.x - 1u);
    let lower = horizontal(vec2<u32>(x.x, min(y.x + 1u, source.y - 1u)), x.y, source.x - 1u);
    return output(vec3<u32>(vertical(upper.r, lower.r, y.y),
        vertical(upper.g, lower.g, y.y), vertical(upper.b, lower.b, y.y)));
}
