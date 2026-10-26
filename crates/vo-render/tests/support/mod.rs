//! Shared code for the GPU test binaries (`tests/gpu_*.rs`): a headless wgpu
//! device, offscreen readback, and golden image comparison.
//!
//! Goldens live in `tests/goldens/<name>.png`: 8-bit grayscale coverage, or 8-bit RGBA
//! holding the renderer's premultiplied bytes as they are (`Image::channels` 1 or 4).
//! `VO_GOLDEN_UPDATE=1` writes them instead of comparing (`just golden-update`).
//! Every run writes the actual image, a diff and a contact sheet to
//! [`OUT_DIR`] and prints the contact sheet path.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const OUT_DIR: &str = "/var/tmp/claude/vo-golden";

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// Headless Vulkan device. Panics with a clear message when there is none:
/// GPU tests only run in the `gpu` nextest profile, where a missing GPU is
/// an error, not a skip.
pub fn gpu() -> Gpu {
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
    let (device, queue) = pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor { label: Some("vo-golden"), ..Default::default() }),
    )
    .expect("no GPU device");
    Gpu { device, queue }
}

/// Renders one pass into a `w` x `h` Rgba8Unorm texture cleared to
/// transparent and returns the alpha channel, row by row.
pub fn render_alpha(gpu: &Gpu, w: u32, h: u32, draw: impl FnOnce(&mut wgpu::RenderPass<'_>)) -> Vec<u8> {
    let (d, q) = (&gpu.device, &gpu.queue);
    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    let tex = d.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    let row = (w * 4).div_ceil(256) * 256;
    let rb = d.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = d.create_command_encoder(&Default::default());
    {
        let att = [Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
        })];
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &att,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        draw(&mut pass);
    }
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &rb,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        size,
    );
    q.submit([enc.finish()]);
    rb.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = rb.slice(..).get_mapped_range().expect("map");
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            out.push(data[(y * row + x * 4 + 3) as usize]);
        }
    }
    out
}

/// How far an image may drift from its golden before the test fails.
#[derive(Clone, Copy)]
pub struct Tolerance {
    /// A pixel counts as different when it is off by more than this (0 to 255).
    pub per_pixel: u8,
    /// Share of pixels that may differ.
    pub max_fraction: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self { per_pixel: 2, max_fraction: 0.0005 }
    }
}

pub struct Image {
    pub name: String,
    pub w: u32,
    pub h: u32,
    /// 1 for grayscale, 4 for premultiplied RGBA.
    pub channels: u32,
    pub px: Vec<u8>,
}

impl Image {
    fn color_type(&self) -> png::ColorType {
        if self.channels == 4 {
            png::ColorType::Rgba
        } else {
            png::ColorType::Grayscale
        }
    }

    /// One pixel as premultiplied RGBA (gray is white at that coverage).
    fn rgba(px: &[u8], channels: u32, i: usize) -> [u8; 4] {
        if channels == 4 {
            [px[i * 4], px[i * 4 + 1], px[i * 4 + 2], px[i * 4 + 3]]
        } else {
            [px[i]; 4]
        }
    }
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens").join(format!("{name}.png"))
}

fn write_png(path: &Path, w: u32, h: u32, color: png::ColorType, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(data).unwrap();
}

fn read_png(path: &Path, color: png::ColorType) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = std::fs::read(path).ok()?;
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    assert_eq!(info.color_type, color, "{} is not 8-bit {color:?}", path.display());
    buf.truncate(info.buffer_size());
    Some((info.width, info.height, buf))
}

/// Compares `img` with its golden, or writes the golden when
/// `VO_GOLDEN_UPDATE=1`. Returns a failure message instead of panicking, so
/// one test can check several images and report them all.
pub fn check(img: &Image, tol: Tolerance) -> Result<(), String> {
    let golden = golden_path(&img.name);
    let out = Path::new(OUT_DIR);
    write_png(&out.join(format!("{}.actual.png", img.name)), img.w, img.h, img.color_type(), &img.px);
    if std::env::var_os("VO_GOLDEN_UPDATE").is_some_and(|v| v == "1") {
        write_png(&golden, img.w, img.h, img.color_type(), &img.px);
        sheet_add(img, None);
        eprintln!("updated {}", golden.display());
        return Ok(());
    }
    let Some((gw, gh, gpx)) = read_png(&golden, img.color_type()) else {
        sheet_add(img, None);
        return Err(format!("{}: no golden at {} (run `just golden-update`)", img.name, golden.display()));
    };
    if (gw, gh) != (img.w, img.h) {
        sheet_add(img, None);
        return Err(format!("{}: size {}x{}, golden {gw}x{gh}", img.name, img.w, img.h));
    }
    let (mut bad, mut worst) = (0usize, 0u8);
    let c = img.channels as usize;
    for (a, b) in img.px.chunks_exact(c).zip(gpx.chunks_exact(c)) {
        let d = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        worst = worst.max(d);
        if d > tol.per_pixel {
            bad += 1;
        }
    }
    sheet_add(img, Some(&gpx));
    let frac = bad as f64 / (img.w * img.h) as f64;
    if frac > tol.max_fraction {
        return Err(format!(
            "{}: {bad} pixels ({:.3} %) differ by more than {}, worst {worst}; see {}",
            img.name,
            frac * 100.0,
            tol.per_pixel,
            out.join(format!("{}.diff.png", img.name)).display()
        ));
    }
    Ok(())
}

/// One contact sheet row: width, height, RGB pixels (golden, actual, diff).
type SheetRow = (u32, u32, Vec<[u8; 3]>);

/// Rows of the contact sheet, one per checked image.
static SHEET: Mutex<Vec<SheetRow>> = Mutex::new(Vec::new());

fn sheet_add(img: &Image, golden: Option<&[u8]>) {
    let (w, h) = (img.w, img.h);
    let gap = 8;
    let sw = w * 3 + gap * 2;
    let mut row = vec![[40u8, 40, 40]; (sw * h) as usize];
    let mut diff = vec![0u8; (w * h * 3) as usize];
    // Premultiplied pixels shown over a dark gray backdrop.
    let show = |p: [u8; 4]| -> [u8; 3] {
        let k = 255 - p[3] as u32;
        [0, 1, 2].map(|c| (p[c] as u32 + 40 * k / 255).min(255) as u8)
    };
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let a = Image::rgba(&img.px, img.channels, i);
            let g = golden.map_or([0; 4], |g| Image::rgba(g, img.channels, i));
            let d = (0..4).map(|c| a[c].abs_diff(g[c])).max().unwrap();
            let lum = a[3] / 4;
            // Diff: dim actual coverage, differences in red, amplified 8x.
            let dc = if golden.is_some() && d > 0 { [d.saturating_mul(8).max(64), lum, lum] } else { [lum; 3] };
            diff[i * 3..i * 3 + 3].copy_from_slice(&dc);
            let base = (y * sw) as usize;
            row[base + x as usize] = show(g);
            row[base + (w + gap + x) as usize] = show(a);
            row[base + (2 * (w + gap) + x) as usize] = dc;
        }
    }
    write_png(&Path::new(OUT_DIR).join(format!("{}.diff.png", img.name)), w, h, png::ColorType::Rgb, &diff);
    let mut sheet = SHEET.lock().unwrap();
    sheet.push((sw, h, row));
    let width = sheet.iter().map(|r| r.0).max().unwrap();
    let height: u32 = sheet.iter().map(|r| r.1 + 8).sum();
    let mut px = vec![[24u8, 24, 24]; (width * height) as usize];
    let mut y0 = 0;
    for (rw, rh, data) in sheet.iter() {
        for y in 0..*rh {
            let src = &data[(y * rw) as usize..((y + 1) * rw) as usize];
            let dst = ((y0 + y) * width) as usize;
            px[dst..dst + *rw as usize].copy_from_slice(src);
        }
        y0 += rh + 8;
    }
    let path = Path::new(OUT_DIR).join("contact-sheet.png");
    write_png(&path, width, height, png::ColorType::Rgb, px.as_flattened());
    if sheet.len() == 1 {
        eprintln!("contact sheet (golden | actual | diff): {}", path.display());
    }
}
