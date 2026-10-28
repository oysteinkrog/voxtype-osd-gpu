//! GPU fluid: a push carries dye and curls it, dye is kept, frame-rate independence, seeking,
//! reduced motion and the GPU cost per tick.
//!
//! GPU test: `just test-gpu fluid`. Prints the numbers it compares and the cost it measures.

use vo_sim::fluid::{Fluid, FluidConfig, FluidSplat};
use vo_sim::{Sim, Timeline, DEFAULT_TICK_HZ};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

/// Headless Vulkan device, with timestamp queries when the adapter has them.
fn gpu() -> Gpu {
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("no Vulkan adapter; GPU tests need one (run `just test` for the CPU suite)");
    let required_features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vo-sim-fluid"),
        required_features,
        ..Default::default()
    }))
    .expect("no GPU device");
    Gpu { device, queue }
}

const W: u32 = 128;
const H: u32 = 64;

/// The dye-weighted mean |curl| at 2 s must be above this, in radians per second. A dye blob
/// pushed sideways rolls up into a vortex pair; with the push left out (curl only from
/// numerical noise) the same measure is near 0.
const CURL_THRESHOLD: f32 = 0.5;

fn cfg() -> FluidConfig {
    FluidConfig { width: W, height: H, reduced_motion: false, ..FluidConfig::default() }
}

/// Blue ink at the left third with a sideways push to the right, both at t = 0.
fn ink_and_push() -> Vec<(f64, FluidSplat)> {
    vec![
        (0.0, FluidSplat::dye(0.3, 0.5, 0.06, [0.2, 0.4, 1.0], 1.0)),
        (0.0, FluidSplat::push(0.3, 0.5, 0.06, [0.6, 0.0])),
    ]
}

fn timeline(gpu: &Gpu, cfg: FluidConfig, splats: &[(f64, FluidSplat)]) -> Timeline<Fluid> {
    let mut tl = Timeline::new(Fluid::new(&gpu.device, &gpu.queue, cfg));
    for &(t, s) in splats {
        tl.push_event(t, s);
    }
    tl
}

/// Total dye amount and its centre of mass in cells.
fn mass(dye: &[[f32; 4]]) -> (f32, f32, f32) {
    let (mut m, mut mx, mut my) = (0.0f64, 0.0f64, 0.0f64);
    for (i, d) in dye.iter().enumerate() {
        let a = d[3] as f64;
        m += a;
        mx += a * (i as u32 % W) as f64;
        my += a * (i as u32 / W) as f64;
    }
    (m as f32, (mx / m) as f32, (my / m) as f32)
}

/// The dye-weighted mean |curl| in radians per second (velocity in cells per second).
fn dye_curl(dye: &[[f32; 4]], vel: &[[f32; 2]]) -> f32 {
    let at = |x: u32, y: u32| vel[(y.min(H - 1) * W + x.min(W - 1)) as usize];
    let (mut sum, mut m) = (0.0f64, 0.0f64);
    for y in 0..H {
        for x in 0..W {
            let c = 0.5
                * ((at(x + 1, y)[1] - at(x.saturating_sub(1), y)[1])
                    - (at(x, y + 1)[0] - at(x, y.saturating_sub(1))[0]));
            let a = dye[(y * W + x) as usize][3] as f64;
            sum += a * c.abs() as f64;
            m += a;
        }
    }
    (sum / m) as f32
}

#[test]
fn a_push_carries_the_dye_and_curls_it() {
    let gpu = gpu();
    let mut tl = timeline(&gpu, cfg(), &ink_and_push());
    let (m0, x0, y0) = mass(&tl.state_at(0.0).read_dye());
    let late = tl.state_at(2.0);
    let dye = late.read_dye();
    let (m2, x2, y2) = mass(&dye);
    let curl = dye_curl(&dye, &late.read_velocity());
    let still = timeline(&gpu, cfg(), &ink_and_push()[..1]);
    let mut still = still;
    let still_curl = {
        let s = still.state_at(2.0);
        dye_curl(&s.read_dye(), &s.read_velocity())
    };
    println!(
        "fluid push: dye {m0:.2} at ({x0:.2}, {y0:.2}) at 0 s, {m2:.2} at ({x2:.2}, {y2:.2}) at 2 s; \
         dye-weighted |curl| {curl:.3} rad/s (no push: {still_curl:.4}, threshold {CURL_THRESHOLD})"
    );
    assert!(m0 > 1.0, "dye landed");
    assert!(x2 - x0 >= 10.0, "the dye moved right by {:.2} cells, want at least 10", x2 - x0);
    assert!((y2 - y0).abs() < 2.0, "a sideways push keeps the dye on its row");
    assert!(curl > CURL_THRESHOLD, "the dye curls: {curl:.3} rad/s");
    assert!(still_curl < CURL_THRESHOLD * 0.1, "no push, no curl: {still_curl:.4}");
}

#[test]
fn closed_walls_keep_the_dye() {
    let gpu = gpu();
    let mut tl = timeline(&gpu, cfg(), &ink_and_push());
    let (early, ..) = mass(&tl.state_at(0.1).read_dye());
    let (late, ..) = mass(&tl.state_at(3.0).read_dye());
    let change = (late - early) / early * 100.0;
    let mut raw = timeline(&gpu, FluidConfig { conserve_dye: false, ..cfg() }, &ink_and_push());
    let (raw_early, ..) = mass(&raw.state_at(0.1).read_dye());
    let (raw_late, ..) = mass(&raw.state_at(3.0).read_dye());
    println!(
        "fluid dye total: {early:.3} at 0.1 s, {late:.3} at 3 s ({change:+.2} %); without conserve_dye {:+.2} %",
        (raw_late - raw_early) / raw_early * 100.0
    );
    assert!(change.abs() < 5.0, "dye total changed by {change:.2} %");
}

/// Draws frames every `dt` seconds to `end` (flushing each like a host), then samples `end`.
fn run_frames(gpu: &Gpu, dt: f64, end: f64) -> (Vec<u8>, Vec<u8>) {
    let mut tl = timeline(gpu, cfg(), &ink_and_push());
    let mut frame = 0u64;
    loop {
        let t = frame as f64 * dt;
        if t >= end {
            break;
        }
        tl.state_at(t).flush();
        frame += 1;
    }
    let f = tl.state_at(end);
    (f.read_dye_texture(), f.read_velocity_texture())
}

#[test]
fn frame_rate_does_not_change_the_flow() {
    let gpu = gpu();
    for end in [0.6, 2.0] {
        let (dye_a, vel_a) = run_frames(&gpu, 1.0 / 144.0, end);
        let (dye_b, vel_b) = run_frames(&gpu, 1.0 / 60.0, end);
        assert_eq!(dye_a.len(), (W * H * 8) as usize);
        assert!(dye_a.iter().any(|&x| x != 0), "dye is there at {end} s");
        assert!(dye_a == dye_b, "dye textures differ at t = {end}");
        assert!(vel_a == vel_b, "velocity textures differ at t = {end}");
    }
}

#[test]
fn seek_back_equals_a_fresh_run() {
    let gpu = gpu();
    let mut tl = timeline(&gpu, cfg(), &ink_and_push());
    tl.state_at(5.0);
    let dye = tl.state_at(2.0).read_dye();
    let tex = tl.sim().read_dye_texture();
    let vel = tl.sim().read_velocity();
    let mut fresh = timeline(&gpu, cfg(), &ink_and_push());
    let fresh_dye = fresh.state_at(2.0).read_dye();
    assert_eq!(dye, fresh_dye, "dye after the seek");
    assert_eq!(vel, fresh.sim().read_velocity(), "velocity after the seek");
    assert!(tex == fresh.sim().read_dye_texture(), "dye texture after the seek");
    println!(
        "fluid seek 5 s -> 2 s: snapshots {} ({} bytes, {} each)",
        tl.snapshots().len(),
        tl.snapshots().bytes(),
        tl.sim().snapshot_size()
    );
}

#[test]
fn reduced_motion_keeps_the_dye_still() {
    let gpu = gpu();
    let check = |cfg: FluidConfig| {
        let mut tl = timeline(&gpu, cfg, &ink_and_push());
        let landed = tl.state_at(0.0).read_dye();
        assert!(mass(&landed).0 > 1.0, "dye still lands");
        let later = tl.state_at(2.0).read_dye();
        assert_eq!(landed, later, "dye did not move");
        assert!(tl.sim().read_velocity().iter().all(|v| *v == [0.0, 0.0]), "the push was dropped");
    };
    check(FluidConfig { reduced_motion: true, ..cfg() });

    std::env::set_var("VO_REDUCED_MOTION", "1");
    let from_env = FluidConfig { width: W, height: H, ..FluidConfig::default() };
    std::env::remove_var("VO_REDUCED_MOTION");
    assert!(from_env.reduced_motion);
    check(from_env);
}

#[test]
fn gpu_cost_per_tick_at_600x300() {
    let gpu = gpu();
    let cfg = FluidConfig { reduced_motion: false, ..FluidConfig::for_size(600.0, 300.0) };
    let mut fluid = Fluid::new(&gpu.device, &gpu.queue, cfg);
    for (_, s) in ink_and_push() {
        fluid.apply(&s);
    }
    for tick in 1..=60 {
        fluid.step(tick, 1.0 / DEFAULT_TICK_HZ);
    }
    let before = fluid.read_dye();
    // Warm up once, then measure.
    fluid.gpu_cost(16);
    let Some(cost) = fluid.gpu_cost(240) else {
        println!("fluid cost: no TIMESTAMP_QUERY on this adapter, skipped");
        return;
    };
    println!(
        "fluid cost {}x{} ({} pressure sweeps): {:.4} ms per tick (budget 0.4 ms) + {:.4} ms output pass; snapshot {} bytes",
        cfg.width,
        cfg.height,
        cfg.iterations(),
        cost.tick_ms,
        cost.output_ms,
        fluid.snapshot_size()
    );
    assert_eq!(fluid.read_dye(), before, "measuring puts the state back");
    assert!(cost.tick_ms < 0.4, "tick over 0.4 ms: {:.4}", cost.tick_ms);
}
