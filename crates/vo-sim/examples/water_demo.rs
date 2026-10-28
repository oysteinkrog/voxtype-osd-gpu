//! The water surface as grey: drops from a scripted list of word onsets, written as PNG frames.
//!
//! `cargo run -p vo-sim --example water_demo -- [out_dir] [fps]`
//!
//! Each onset drops a ring where its word would sit on a reading line; voice level sets the
//! strength. Mid grey is flat water, lighter is up, darker is down. Frames go to `out_dir`
//! (default `/var/tmp/claude/swarm/EG-09/water_demo`) at `fps` (default 10) for 4 s, at
//! 4 px per grid cell. Headless: no window opens.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use vo_sim::water::{Water, WaterConfig, WaterDrop};
use vo_sim::Timeline;

/// (time in seconds, word position along the line 0..1, voice level 0..1).
const ONSETS: &[(f64, f32, f32)] =
    &[(0.10, 0.12, 0.8), (0.42, 0.27, 0.5), (0.70, 0.40, 0.9), (1.05, 0.55, 0.6), (1.30, 0.70, 0.7), (1.80, 0.86, 1.0)];

const SCALE: u32 = 4;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().unwrap_or_else(|| "/var/tmp/claude/swarm/EG-09/water_demo".into()));
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

    let cfg = WaterConfig::for_size(600.0, 300.0);
    let (w, h) = (cfg.width, cfg.height);
    let mut tl = Timeline::new(Water::new(&device, &queue, cfg));
    for &(t, x, level) in ONSETS {
        tl.push_event(t, WaterDrop { x, y: 0.5, radius: 0.02 + 0.02 * level, strength: -0.6 * level });
    }

    let frames = (4.0 * fps).round() as u32;
    for f in 0..=frames {
        let t = f as f64 / fps;
        let heights = tl.state_at(t).read_heights();
        let (pw, ph) = (w * SCALE, h * SCALE);
        let mut grey = vec![0u8; (pw * ph) as usize];
        for y in 0..ph {
            for x in 0..pw {
                let v = heights[((y / SCALE) * w + x / SCALE) as usize];
                grey[(y * pw + x) as usize] = (128.0 + v * 255.0).clamp(0.0, 255.0) as u8;
            }
        }
        let path = out.join(format!("water_{f:03}.png"));
        let mut enc = png::Encoder::new(BufWriter::new(File::create(&path).expect("create png")), pw, ph);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().and_then(|mut wr| wr.write_image_data(&grey)).expect("write png");
    }
    println!("water_demo: {} frames of {}x{} grid at {fps} fps in {}", frames + 1, w, h, out.display());
}
