fn channel(sum: vec2<u32>) -> u32 {
    // Match AreaPlan's ceil(2^40 / (sw * sh)), including its rounding bias.
    let product = wide_product(sum, geometry.reciprocal);
    return wide_add(product, vec2<u32>(0u, 128u)).y >> 8u;
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // Helper lanes can lie outside the viewport/scissor, especially for a
    // one-pixel viewport at an odd origin. Keep their unsigned coordinates
    // bounded too: hi wrapping to zero would make last == UINT_MAX at extent
    // one, and a <= loop would wrap forever. Covered pixels are unchanged.
    let local = min(vec2<u32>(position.xy) - geometry.origin,
        geometry.extent - vec2<u32>(1u));
    let source = textureDimensions(guest);
    let lo = local * source;
    let hi = lo + source;
    let first = lo / geometry.extent;
    let last = (hi - vec2<u32>(1u)) / geometry.extent;
    if all(first == last) {
        return output(pixel(first));
    }
    var red = vec2<u32>(0u);
    var green = vec2<u32>(0u);
    var blue = vec2<u32>(0u);
    // The entire footprint, without a tap cap, mip approximation or staged
    // rounding. Axis::area measures overlaps in 1/extent source-pixel units.
    for (var y = first.y; y <= last.y; y += 1u) {
        let wy = min(hi.y, (y + 1u) * geometry.extent.y) - max(lo.y, y * geometry.extent.y);
        var row = vec3<u32>(0u);
        for (var x = first.x; x <= last.x; x += 1u) {
            let wx = min(hi.x, (x + 1u) * geometry.extent.x) - max(lo.x, x * geometry.extent.x);
            row += pixel(vec2<u32>(x, y)) * wx;
        }
        red = wide_add(red, wide_mul(row.r, wy));
        green = wide_add(green, wide_mul(row.g, wy));
        blue = wide_add(blue, wide_mul(row.b, wy));
    }
    return output(vec3<u32>(channel(red), channel(green), channel(blue)));
}
