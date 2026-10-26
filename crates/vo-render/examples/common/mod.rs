//! Helpers shared by the phase 0 examples.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

pub const OUT_DIR: &str = "/var/tmp/claude/vo-p0";

/// Family of the Plasma UI font from `~/.config/kdeglobals` (`font=`), or
/// "Noto Sans" when unset.
pub fn plasma_font_family() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    std::fs::read_to_string(format!("{home}/.config/kdeglobals"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("font=").map(|v| v.split(',').next().unwrap_or("").to_string()))
        })
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "Noto Sans".into())
}

/// Resolves a family name to a font file through fontconfig.
pub fn fc_match(family: &str) -> PathBuf {
    let out = Command::new("fc-match").args(["-f", "%{file}", family]).output().expect("fc-match");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

/// `VO_FONT` if set, else the Plasma UI font.
pub fn ui_font_path() -> PathBuf {
    std::env::var_os("VO_FONT").map(PathBuf::from).unwrap_or_else(|| fc_match(&plasma_font_family()))
}

pub fn write_png_rgb(path: &str, w: u32, h: u32, rgb: &[u8]) {
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgb).unwrap();
}

pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let man = (h & 0x3ff) as f32;
    sign * match exp {
        0 => man * 2f32.powi(-24),
        31 => f32::INFINITY,
        e => (1.0 + man / 1024.0) * 2f32.powi(e - 15),
    }
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Headless Vulkan device, with timestamp queries when the adapter has them.
pub fn gpu() -> (wgpu::Adapter, wgpu::Device, wgpu::Queue) {
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("no GPU adapter");
    let features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vo-p0"),
        required_features: features,
        ..Default::default()
    }))
    .expect("no GPU device");
    (adapter, device, queue)
}
