// Height-field water: the damped 2D wave equation on a grid, plus drops and the output pass.
//
// Two height buffers hold the state: `cur` (tick n) and `prev` (tick n - 1). `wave_step`
// writes tick n + 1 into `prev`, and the host swaps which buffer it calls `cur`. Edges are
// closed (a wave reflects). Every cell is written by one invocation and reads only cells
// nobody writes in that dispatch, so the result does not depend on scheduling.

struct Params {
    width: u32,
    height: u32,
    // (c dt / dx)^2 for one substep. Stable below 0.5.
    c2: f32,
    // Velocity kept per substep.
    vel_keep: f32,
    // Height kept per substep (pulls the mean back to flat).
    height_keep: f32,
    // Output pass: slope scale for the normal.
    normal_scale: f32,
    _pad0: f32,
    _pad1: f32,
}

struct Splash {
    // Centre in cells.
    x: f32,
    y: f32,
    // Radius in cells.
    radius: f32,
    strength: f32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> cur: array<f32>;
@group(0) @binding(2) var<storage, read_write> prev: array<f32>;

@group(1) @binding(0) var height_out: texture_storage_2d<rgba16float, write>;
@group(1) @binding(1) var normal_out: texture_storage_2d<rgba8snorm, write>;

// Group 1 differs per entry point: textures for `output`, the splash for `add_drop`.
@group(1) @binding(2) var<uniform> splash: Splash;

fn at(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return cur[u32(cy) * params.width + u32(cx)];
}

@compute @workgroup_size(8, 8)
fn wave_step(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.width || id.y >= params.height {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * params.width + id.x;
    let c = cur[i];
    let lap = at(x - 1, y) + at(x + 1, y) + at(x, y - 1) + at(x, y + 1) - 4.0 * c;
    let v = (c - prev[i]) * params.vel_keep;
    prev[i] = (c + v + params.c2 * lap) * params.height_keep;
}

// A drop displaces the surface at rest: the same bump goes into both ticks, so it starts with
// no velocity and spreads as a ring.
@compute @workgroup_size(8, 8)
fn add_drop(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.width || id.y >= params.height {
        return;
    }
    let d = distance(vec2<f32>(f32(id.x), f32(id.y)), vec2<f32>(splash.x, splash.y));
    if d >= splash.radius {
        return;
    }
    let bump = splash.strength * 0.5 * (1.0 + cos(3.14159265 * d / splash.radius));
    let i = id.y * params.width + id.x;
    cur[i] = cur[i] + bump;
    prev[i] = prev[i] + bump;
}

// Height texture: r = height, g = change over the last tick (cur - prev), a = 1. Normal
// texture: xyz = surface normal from central differences (+z faces the viewer), w = 1.
@compute @workgroup_size(8, 8)
fn output(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.width || id.y >= params.height {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * params.width + id.x;
    let h = cur[i];
    textureStore(height_out, vec2<i32>(x, y), vec4<f32>(h, h - prev[i], 0.0, 1.0));
    let dx = (at(x + 1, y) - at(x - 1, y)) * 0.5 * params.normal_scale;
    let dy = (at(x, y + 1) - at(x, y - 1)) * 0.5 * params.normal_scale;
    textureStore(normal_out, vec2<i32>(x, y), vec4<f32>(normalize(vec3<f32>(-dx, -dy, 1.0)), 1.0));
}
