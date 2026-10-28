//! Ink in the fluid: dye and pushes from a scripted list of word onsets, written as PNG frames.
//!
//! `cargo run -p vo-sim --example fluid_demo -- [out_dir] [fps]`
//!
//! Each onset drops a blot of ink where its word would sit on a reading line and pushes it up
//! and along the line; voice level sets the push. The ink is drawn over paper white. Frames go
//! to `out_dir` (default `/var/tmp/claude/swarm/EG-11/fluid_demo`) at `fps` (default 10) for
//! 4 s, at 4 px per grid cell. Headless: no window opens.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use vo_sim::fluid::{Fluid, FluidConfig, FluidSplat};
use vo_sim::Timeline;

/// (time in seconds, word position along the line 0..1, voice level 0..1).
const ONSETS: &[(f64, f32, f32)] =
    &[(0.10, 0.12, 0.8), (0.42, 0.27, 0.5), (0.70, 0.40, 0.9), (1.05, 0.55, 0.6), (1.30, 0.70, 0.7), (1.80, 0.86, 1.0)];

/// Ink colours, cycled per word.
const INKS: &[[f32; 3]] = &[[0.10, 0.20, 0.55], [0.55, 0.10, 0.25], [0.05, 0.40, 0.35]];

const SCALE: u32 = 4;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().unwrap_or_else(|| "/var/tmp/claude/swarm/EG-11/fluid_demo".into()));
    let fps: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(10.0);
    std::fs::create_dir_all(&out).expect("create the output folder");

    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("no Vulkan adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).expect("no GPU device");

    let cfg = FluidConfig::for_size(600.0, 300.0);
    let (w, h) = (cfg.width, cfg.height);
    let mut tl = Timeline::new(Fluid::new(&device, &queue, cfg));
    for (k, &(t, x, level)) in ONSETS.iter().enumerate() {
        tl.push_event(t, FluidSplat::dye(x, 0.55, 0.035, INKS[k % INKS.len()], 1.0));
        tl.push_event(t, FluidSplat::push(x - 0.01, 0.6, 0.05, [0.15 * level, -0.5 * level]));
    }

    let frames = (4.0 * fps).round() as u32;
    for f in 0..=frames {
        let t = f as f64 / fps;
        let dye = tl.state_at(t).read_dye();
        let (pw, ph) = (w * SCALE, h * SCALE);
        let mut rgb = vec![0u8; (pw * ph * 3) as usize];
        for y in 0..ph {
            for x in 0..pw {
                let d = dye[((y / SCALE) * w + x / SCALE) as usize];
                let a = d[3].clamp(0.0, 1.0);
                let at = ((y * pw + x) * 3) as usize;
                for c in 0..3 {
                    // Premultiplied ink over white paper.
                    let v = d[c].clamp(0.0, 1.0) + (1.0 - a) * 0.96;
                    rgb[at + c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let path = out.join(format!("fluid_{f:03}.png"));
        let mut enc = png::Encoder::new(BufWriter::new(File::create(&path).expect("create png")), pw, ph);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().and_then(|mut wr| wr.write_image_data(&rgb)).expect("write png");
    }
    println!("fluid_demo: {} frames of {}x{} grid at {fps} fps in {}", frames + 1, w, h, out.display());
}
