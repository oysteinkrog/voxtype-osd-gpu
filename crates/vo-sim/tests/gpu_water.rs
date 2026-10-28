//! GPU water surface: interference, damping, frame-rate independence, seeking, reduced motion
//! and the GPU cost per tick.
//!
//! GPU test: `just test-gpu water`. Prints the heights it compares and the cost it measures.

use vo_sim::water::{Water, WaterConfig, WaterDrop};
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
        label: Some("vo-sim-water"),
        required_features,
        ..Default::default()
    }))
    .expect("no GPU device");
    Gpu { device, queue }
}

const W: u32 = 128;
const H: u32 = 64;
const ROW: u32 = 32;
const LEFT: u32 = 44;
const RIGHT: u32 = 84;
const MID: u32 = (LEFT + RIGHT) / 2;
const INTERFERE_SPEED: f32 = 0.26;

fn cfg() -> WaterConfig {
    WaterConfig { width: W, height: H, reduced_motion: false, ..WaterConfig::default() }
}

/// A drop of strength 1 and radius 4 cells centred on cell `(x, ROW)`.
fn drop_at(x: u32) -> WaterDrop {
    WaterDrop {
        x: (x as f32 + 0.5) / W as f32,
        y: (ROW as f32 + 0.5) / H as f32,
        radius: 4.0 / W as f32,
        strength: 1.0,
    }
}

fn timeline(gpu: &Gpu, cfg: WaterConfig, drops: &[(f64, WaterDrop)]) -> Timeline<Water> {
    let mut tl = Timeline::new(Water::new(&gpu.device, &gpu.queue, cfg));
    for &(t, d) in drops {
        tl.push_event(t, d);
    }
    tl
}

fn two_drops() -> Vec<(f64, WaterDrop)> {
    vec![(0.0, drop_at(LEFT)), (0.1, drop_at(RIGHT))]
}

fn at(h: &[f32], x: u32, y: u32) -> f32 {
    h[(y * W + x) as usize]
}

fn max_abs(h: &[f32]) -> f32 {
    h.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

#[test]
fn two_drops_interfere_and_die_down() {
    let gpu = gpu();
    // The 0.1 s head start puts the left ring 0.1 * speed ahead of the right one at the
    // midpoint, so whether they add up there depends on the speed. At 0.26 widths/s (33 cells/s)
    // the left crest reaches the midpoint at 0.6 s while the right ring's leading slope is
    // 3 cells short of it: both are up. Other speeds are printed for reference.
    let midway = |speed: f32| {
        let cfg = WaterConfig { speed, ..cfg() };
        let both = timeline(&gpu, cfg, &two_drops()).state_at(0.6).read_heights();
        let a = timeline(&gpu, cfg, &two_drops()[..1]).state_at(0.6).read_heights();
        let b = timeline(&gpu, cfg, &two_drops()[1..]).state_at(0.6).read_heights();
        let (hb, ha, hr) = (at(&both, MID, ROW), at(&a, MID, ROW), at(&b, MID, ROW));
        println!("water speed {speed}, t=0.6 s midway: both {hb:.5}, left only {ha:.5}, right only {hr:.5}");
        (hb, ha, hr)
    };
    for speed in [0.22, 0.24, 0.28, 0.3, 0.35, 0.4] {
        midway(speed);
    }
    let (hb, ha, hr) = midway(INTERFERE_SPEED);
    assert!((hb - (ha + hr)).abs() < 1e-4, "the wave is linear: both = left + right");
    assert!(hb.abs() > ha.abs().max(hr.abs()) * 1.05, "constructive interference midway");

    // Damping: the peak over the first 0.3 s against the largest height at 4 s.
    let mut tl = timeline(&gpu, cfg(), &two_drops());
    let mut peak = 0.0f32;
    for tick in 0..=36 {
        tl.goto_tick(tick);
        peak = peak.max(max_abs(&tl.sim().read_heights()));
    }
    let late = max_abs(&tl.state_at(4.0).read_heights());
    println!("water peak {peak:.4}, max |height| at 4 s {late:.5} ({:.2} %)", late / peak * 100.0);
    assert!(peak > 0.9);
    assert!(late < 0.05 * peak, "damped below 5 % of the peak by 4 s");
}

/// Draws frames every `dt` seconds to `end` (flushing each like a host), then samples `end`.
fn run_frames(gpu: &Gpu, dt: f64, end: f64) -> Vec<u8> {
    let mut tl = timeline(gpu, cfg(), &two_drops());
    let mut frame = 0u64;
    loop {
        let t = frame as f64 * dt;
        if t >= end {
            break;
        }
        tl.state_at(t).flush();
        frame += 1;
    }
    tl.state_at(end).read_height_texture()
}

#[test]
fn frame_rate_does_not_change_the_surface() {
    let gpu = gpu();
    for end in [0.6, 2.0] {
        let a = run_frames(&gpu, 1.0 / 144.0, end);
        let b = run_frames(&gpu, 1.0 / 60.0, end);
        assert_eq!(a.len(), (W * H * 8) as usize);
        assert!(a.iter().any(|&x| x != 0), "the surface moved by {end} s");
        assert!(a == b, "height textures differ at t = {end}");
    }
}

#[test]
fn seek_back_equals_a_fresh_run() {
    let gpu = gpu();
    let mut tl = timeline(&gpu, cfg(), &two_drops());
    tl.state_at(5.0);
    let heights = tl.state_at(2.0).read_heights();
    let texture = tl.sim().read_height_texture();
    let mut fresh = timeline(&gpu, cfg(), &two_drops());
    let fresh_heights = fresh.state_at(2.0).read_heights();
    assert!(max_abs(&fresh_heights) > 0.0);
    assert_eq!(heights, fresh_heights, "heights after the seek");
    assert!(texture == fresh.sim().read_height_texture(), "height texture after the seek");
    println!("water seek 5 s -> 2 s: snapshots {} ({} bytes)", tl.snapshots().len(), tl.snapshots().bytes());
}

#[test]
fn reduced_motion_keeps_the_surface_flat() {
    let gpu = gpu();
    let flag = WaterConfig { reduced_motion: true, ..cfg() };
    let h = timeline(&gpu, flag, &two_drops()).state_at(1.0).read_heights();
    assert!(h.iter().all(|&v| v == 0.0));

    std::env::set_var("VO_REDUCED_MOTION", "1");
    let from_env = WaterConfig { width: W, height: H, ..WaterConfig::default() };
    std::env::remove_var("VO_REDUCED_MOTION");
    assert!(from_env.reduced_motion);
    let mut tl = timeline(&gpu, from_env, &two_drops());
    assert!(tl.state_at(1.0).read_heights().iter().all(|&v| v == 0.0));
    let tex = tl.sim().read_height_texture();
    // Every texel is (0, 0, 0, 1) in f16: 1.0 is 0x3c00.
    assert!(tex.chunks(8).all(|t| t == [0, 0, 0, 0, 0, 0, 0x00, 0x3c]));
}

#[test]
fn gpu_cost_per_tick_at_600x300() {
    let gpu = gpu();
    let cfg = WaterConfig { reduced_motion: false, ..WaterConfig::for_size(600.0, 300.0) };
    let mut water = Water::new(&gpu.device, &gpu.queue, cfg);
    water.apply(&drop_at(LEFT));
    for tick in 1..=60 {
        water.step(tick, 1.0 / DEFAULT_TICK_HZ);
    }
    let before = water.read_heights();
    // Warm up once, then measure.
    water.gpu_cost(16);
    let Some(cost) = water.gpu_cost(240) else {
        println!("water cost: no TIMESTAMP_QUERY on this adapter, skipped");
        return;
    };
    let total = cost.tick_ms + cost.output_ms;
    println!(
        "water cost {}x{}: {:.4} ms per tick + {:.4} ms output pass = {total:.4} ms (budget 0.3 ms); snapshot {} bytes",
        cfg.width,
        cfg.height,
        cost.tick_ms,
        cost.output_ms,
        water.snapshot_size()
    );
    assert_eq!(water.read_heights(), before, "measuring puts the state back");
    assert!(total < 0.3, "tick plus output pass over 0.3 ms: {total:.4}");
}
