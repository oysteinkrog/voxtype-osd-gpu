// Stable fluids on a grid: velocity and dye, semi-Lagrangian advection, a Jacobi pressure
// solve (red-black SOR) with a fixed sweep count, vorticity confinement, splats and the output pass.
//
// Velocity is in grid cells per second, at cell centres. Walls are closed: the normal velocity
// is zero on the edge cells, pressure has a zero gradient across the edge, and a back-traced
// sample point is clamped inside the grid. Every pass writes each cell from one invocation and
// reads only cells no other invocation writes in that pass (the projection and the splat touch
// only their own cell, an SOR half sweep reads only the other colour), so results never depend
// on scheduling.

struct Params {
    width: u32,
    height: u32,
    // Seconds per tick.
    dt: f32,
    // Velocity kept per tick (fade).
    vel_keep: f32,
    // Dye kept per tick (fade).
    dye_keep: f32,
    // Vorticity confinement strength.
    curl: f32,
    // Over-relaxation factor of the pressure solve.
    sor: f32,
    // 1: restore the dye total after advection.
    conserve: u32,
}

struct Splat {
    // Centre and radius in cells.
    x: f32,
    y: f32,
    radius: f32,
    _pad: f32,
    // Velocity added at the centre, cells per second.
    vx: f32,
    vy: f32,
    // Dye added at the centre: premultiplied colour and amount.
    _pad2: f32,
    _pad3: f32,
    dye: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> vel: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> vel_tmp: array<vec2<f32>>;
@group(0) @binding(3) var<storage, read_write> dye: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> dye_tmp: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> pres: array<f32>;
// Curl in the first `width * height` cells, divergence in the second.
@group(0) @binding(6) var<storage, read_write> scratch: array<f32>;
// Dye totals: [0] before advection, [1] after.
@group(0) @binding(7) var<storage, read_write> sums: array<f32>;

// Group 1 differs per entry point: the splat for `splat`, the textures for `output`.
@group(1) @binding(0) var dye_out: texture_storage_2d<rgba16float, write>;
@group(1) @binding(1) var vel_out: texture_storage_2d<rgba16float, write>;
@group(1) @binding(2) var<uniform> sp: Splat;

fn cells() -> u32 {
    return params.width * params.height;
}

fn idx(x: i32, y: i32) -> u32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return u32(cy) * params.width + u32(cx);
}

fn inside(id: vec3<u32>) -> bool {
    return id.x < params.width && id.y < params.height;
}

fn curl_at(x: i32, y: i32) -> f32 {
    return scratch[idx(x, y)];
}

// The curl (z of the 2D vorticity) of `vel` at a cell, by central differences.
fn vel_curl(x: i32, y: i32) -> f32 {
    return 0.5 * ((vel[idx(x + 1, y)].y - vel[idx(x - 1, y)].y) - (vel[idx(x, y + 1)].x - vel[idx(x, y - 1)].x));
}

// The back-traced sample point of a cell, clamped inside the grid.
fn back_trace(id: vec3<u32>, v: vec2<f32>) -> vec2<f32> {
    let p = vec2<f32>(f32(id.x), f32(id.y)) - params.dt * v;
    return clamp(p, vec2<f32>(0.0), vec2<f32>(f32(params.width - 1u), f32(params.height - 1u)));
}

// Bilinear weights and the four corner indices of point `p` (in cells, inside the grid).
struct Corners {
    i00: u32,
    i10: u32,
    i01: u32,
    i11: u32,
    f: vec2<f32>,
}

fn corners(p: vec2<f32>) -> Corners {
    let b = floor(p);
    let x = i32(b.x);
    let y = i32(b.y);
    return Corners(idx(x, y), idx(x + 1, y), idx(x, y + 1), idx(x + 1, y + 1), p - b);
}

// 1. The curl of the velocity, for confinement.
@compute @workgroup_size(8, 8)
fn curl(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    scratch[id.y * params.width + id.x] = vel_curl(i32(id.x), i32(id.y));
}

// 2. Vorticity confinement and the velocity fade: vel -> vel_tmp.
@compute @workgroup_size(8, 8)
fn confine(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * params.width + id.x;
    let c = curl_at(x, y);
    // Points up the gradient of |curl|, towards the vortex centre.
    var n = 0.5 * vec2<f32>(abs(curl_at(x + 1, y)) - abs(curl_at(x - 1, y)), abs(curl_at(x, y + 1)) - abs(curl_at(x, y - 1)));
    n = n / (length(n) + 1e-5);
    let force = params.curl * c * vec2<f32>(n.y, -n.x);
    vel_tmp[i] = (vel[i] + params.dt * force) * params.vel_keep;
}

// 3. Advects the velocity by itself: vel_tmp -> vel.
@compute @workgroup_size(8, 8)
fn advect_vel(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    let i = id.y * params.width + id.x;
    let c = corners(back_trace(id, vel_tmp[i]));
    let a = mix(vel_tmp[c.i00], vel_tmp[c.i10], c.f.x);
    let b = mix(vel_tmp[c.i01], vel_tmp[c.i11], c.f.x);
    vel[i] = mix(a, b, c.f.y);
}

// 4. The divergence of the velocity, with walls reflecting the normal component.
@compute @workgroup_size(8, 8)
fn divergence(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * params.width + id.x;
    let vc = vel[i];
    var l = vel[idx(x - 1, y)].x;
    var r = vel[idx(x + 1, y)].x;
    var t = vel[idx(x, y - 1)].y;
    var b = vel[idx(x, y + 1)].y;
    if x == 0 {
        l = -vc.x;
    }
    if x == i32(params.width) - 1 {
        r = -vc.x;
    }
    if y == 0 {
        t = -vc.y;
    }
    if y == i32(params.height) - 1 {
        b = -vc.y;
    }
    scratch[cells() + i] = 0.5 * ((r - l) + (b - t));
}

// One red-black SOR half sweep on `pres`, in place: a cell of colour `parity` reads only its
// neighbours, which are the other colour, so no invocation reads a cell another one writes.
// Walls have a zero pressure gradient: only neighbours inside the grid count.
fn sor(id: vec3<u32>, parity: u32) {
    if !inside(id) || ((id.x + id.y) & 1u) != parity {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * params.width + id.x;
    var s = 0.0;
    var n = 0.0;
    if x > 0 {
        s += pres[i - 1u];
        n += 1.0;
    }
    if x < i32(params.width) - 1 {
        s += pres[i + 1u];
        n += 1.0;
    }
    if y > 0 {
        s += pres[i - params.width];
        n += 1.0;
    }
    if y < i32(params.height) - 1 {
        s += pres[i + params.width];
        n += 1.0;
    }
    let gs = (s - scratch[cells() + i]) / n;
    pres[i] = mix(pres[i], gs, params.sor);
}

// 5. Pressure: a fixed number of red-black SOR sweeps, warm-started from the last tick.
@compute @workgroup_size(8, 8)
fn sor_red(@builtin(global_invocation_id) id: vec3<u32>) {
    sor(id, 0u);
}

@compute @workgroup_size(8, 8)
fn sor_black(@builtin(global_invocation_id) id: vec3<u32>) {
    sor(id, 1u);
}

// 6. Subtracts the pressure gradient, then closes the walls.
@compute @workgroup_size(8, 8)
fn project(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * params.width + id.x;
    let g = 0.5 * vec2<f32>(pres[idx(x + 1, y)] - pres[idx(x - 1, y)], pres[idx(x, y + 1)] - pres[idx(x, y - 1)]);
    var v = vel[i] - g;
    if x == 0 || x == i32(params.width) - 1 {
        v.x = 0.0;
    }
    if y == 0 || y == i32(params.height) - 1 {
        v.y = 0.0;
    }
    vel[i] = v;
}

// 7. Advects the dye by the velocity: dye -> dye_tmp.
@compute @workgroup_size(8, 8)
fn advect_dye(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    let i = id.y * params.width + id.x;
    let c = corners(back_trace(id, vel[i]));
    let a = mix(dye[c.i00], dye[c.i10], c.f.x);
    let b = mix(dye[c.i01], dye[c.i11], c.f.x);
    dye_tmp[i] = mix(a, b, c.f.y);
}

// The dye total, by one workgroup in a fixed order (each thread a fixed stride of cells, then a
// fixed tree), so the sum is the same bits every run.
const SUM_THREADS: u32 = 256u;
var<workgroup> partial: array<f32, SUM_THREADS>;

fn dye_total(tid: u32, after: bool) {
    var s = 0.0;
    for (var k = tid; k < cells(); k += SUM_THREADS) {
        if after {
            s += dye_tmp[k].a;
        } else {
            s += dye[k].a;
        }
    }
    partial[tid] = s;
    workgroupBarrier();
    for (var half = SUM_THREADS / 2u; half > 0u; half = half / 2u) {
        if tid < half {
            partial[tid] = partial[tid] + partial[tid + half];
        }
        workgroupBarrier();
    }
    if tid == 0u {
        sums[select(0u, 1u, after)] = partial[0];
    }
}

@compute @workgroup_size(256)
fn sum_before(@builtin(local_invocation_index) tid: u32) {
    dye_total(tid, false);
}

@compute @workgroup_size(256)
fn sum_after(@builtin(local_invocation_index) tid: u32) {
    dye_total(tid, true);
}

// 8. dye_tmp -> dye, scaled so the total is what it was before advection (when `conserve` is
// on), then faded. The scale is limited to 10 % a tick so an empty grid cannot blow up.
@compute @workgroup_size(8, 8)
fn store_dye(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    let i = id.y * params.width + id.x;
    var scale = 1.0;
    if params.conserve == 1u && sums[1] > 1e-6 {
        scale = clamp(sums[0] / sums[1], 0.9, 1.1);
    }
    dye[i] = dye_tmp[i] * (scale * params.dye_keep);
}

// A splat: a smooth bump of velocity and dye.
@compute @workgroup_size(8, 8)
fn splat(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    let d = distance(vec2<f32>(f32(id.x), f32(id.y)), vec2<f32>(sp.x, sp.y));
    if d >= sp.radius {
        return;
    }
    let w = 0.5 * (1.0 + cos(3.14159265 * d / sp.radius));
    let i = id.y * params.width + id.x;
    vel[i] = vel[i] + w * vec2<f32>(sp.vx, sp.vy);
    dye[i] = dye[i] + w * sp.dye;
}

// Dye texture: the dye as is (premultiplied rgb, a = amount). Velocity texture: xy = velocity
// in uv per second, z = curl in radians per second, w = 1.
@compute @workgroup_size(8, 8)
fn output(@builtin(global_invocation_id) id: vec3<u32>) {
    if !inside(id) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * params.width + id.x;
    textureStore(dye_out, vec2<i32>(x, y), dye[i]);
    let uv = vel[i] / vec2<f32>(f32(params.width), f32(params.height));
    textureStore(vel_out, vec2<i32>(x, y), vec4<f32>(uv, vel_curl(x, y), 1.0));
}
