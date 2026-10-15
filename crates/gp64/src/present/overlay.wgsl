struct Placement {
    origin: vec2<u32>,
    padding: vec2<u32>,
};
@group(0) @binding(0) var layer: texture_2d<f32>;
@group(0) @binding(1) var<uniform> placement: Placement;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(positions[index], 0.0, 1.0);
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let at = vec2<u32>(position.xy) - placement.origin;
    let colour = textureLoad(layer, vec2<i32>(at), 0);
    if colour.a == 0.0 {
        discard;
    }
    return vec4<f32>(colour.rgb, 1.0);
}
