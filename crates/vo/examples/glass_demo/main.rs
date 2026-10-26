//! Glass variants of the swiss dictation popup, dark and light (FX06 glass, FX07 compositor
//! blur, FX08 rim), drawn with `vo_render::glass`.
//!
//! Offline, a contact sheet over four backdrops (busy with compositor blur, busy without,
//! pure white, pure black) at several times, the AX01 contrast numbers measured from the
//! rendered panel, GPU time per frame and the cold first frame:
//!   cargo run --release -p vo --example glass_demo -- --offline /var/tmp/claude/m4-glass
//!
//! Live, on Wayland (only inside a nested compositor when run by an agent), with the blur
//! region sent through the host when the compositor offers `ext-background-effect-v1`:
//!   cargo run --release -p vo --example glass_demo -- --live [--secs S] [--no-blur] [--light]

mod live;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use vo::scene::display::{DisplayList, ShapeKind};
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Point, Size};
use vo::scene::taffy::{AlignItems, AlignSelf};
use vo::scene::timed::Wave;
use vo::scene::Scene;
use vo::{FontRegistry, ShapedLayout};
use vo_render::glass::{Glass, GlassBlend, GlassDraw, GlassPass};
use vo_render::{Renderer, Target};

const SANS: u32 = 0;
const MONO: u32 = 1;
pub const PANEL_W: f32 = 560.0;
pub const MARGIN: f32 = 24.0;
pub const RADIUS: f32 = 18.0;
/// The sheen crosses once in this many seconds after show.
const SHEEN_SECS: f64 = 0.55;
/// One lap of the orbiting rim segment.
const ORBIT_SECS: f64 = 6.0;

/// A glass look: the material and the text colours that go with it. The material's interior
/// tint is what the contrast gate (AX01) checks the text against.
#[derive(Clone, Copy)]
pub struct Look {
    pub name: &'static str,
    pub glass: Glass,
    pub foreground: Color,
    pub inactive: Color,
    pub accent: Color,
    pub hues: [Color; 3],
    pub rule: Color,
}

pub const DARK: Look = Look {
    name: "dark",
    glass: Glass::dark(),
    foreground: Color::rgba(0.96, 0.97, 0.98, 1.0),
    inactive: Color::rgba(0.82, 0.84, 0.88, 1.0),
    accent: Color::rgba(0.70, 0.85, 1.0, 1.0),
    hues: [Color::rgba(0.45, 0.70, 1.0, 1.0), Color::rgba(0.70, 0.55, 1.0, 1.0), Color::rgba(1.0, 0.55, 0.75, 1.0)],
    rule: Color::rgba(1.0, 1.0, 1.0, 0.14),
};

pub const LIGHT: Look = Look {
    name: "light",
    glass: Glass::light(),
    foreground: Color::rgba(0.07, 0.08, 0.10, 1.0),
    inactive: Color::rgba(0.24, 0.26, 0.30, 1.0),
    accent: Color::rgba(0.04, 0.24, 0.60, 1.0),
    hues: [Color::rgba(0.15, 0.45, 1.0, 1.0), Color::rgba(0.50, 0.30, 0.95, 1.0), Color::rgba(0.90, 0.25, 0.55, 1.0)],
    rule: Color::rgba(0.0, 0.0, 0.0, 0.12),
};

/// The swiss popup layout with no panel fill: the glass pass draws the panel under it.
/// `slide` moves the panel up from its rest position, for the show animation.
pub fn view(look: &Look, t: f64, slide: f32) -> Element<()> {
    let mono = |px: f32| TextStyle::new(MONO, px, (px * 1.3).round());
    let caption = |key: &'static str, text: &str, px: f32, tracking: f32| {
        Element::label(key, text).text_style(mono(px).tracking(tracking)).color(look.inactive)
    };
    let stat = |key: &'static str, value: &str, name: &'static str| {
        Element::column(key)
            .gap(2.0)
            .child(Element::label("value", value).text_style(mono(14.5)).color(look.foreground))
            .child(caption("caption", name, 9.5, 1.5))
    };
    let mic = Element::column("mic")
        .size(14.0, 14.0)
        .align_items(AlignItems::CENTER)
        .gap(1.5)
        .child(Element::shape("body", ShapeKind::Capsule).size(6.0, 9.0).fill(look.accent))
        .child(Element::shape("stand", ShapeKind::RRect).size(1.5, 2.0).fill(look.accent))
        .child(Element::shape("base", ShapeKind::RRect).size(7.0, 1.5).fill(look.accent));
    let secs = 3 + t as u32;
    let header = Element::row("header")
        .align_items(AlignItems::START)
        .child(mic)
        .child(caption("title", "VOICE DICTATION", 11.0, 2.0).margin_edges(0.0, 0.0, 0.0, 8.0))
        .child(Element::row("spacer").grow(1.0))
        .child(stat("time", &format!("0:{secs:02}"), "DURATION"))
        .child(
            Element::shape("rule", ShapeKind::RRect)
                .width(1.0)
                .fill(look.rule)
                .margin_edges(0.0, 16.0, 0.0, 16.0)
                .layout(|s| s.align_self = Some(AlignSelf::STRETCH)),
        )
        .child(stat("words", "14", "WORDS"));
    // A made-up level history, the same at every time so frames compare.
    let levels: Arc<[f32]> = (0..240)
        .map(|i| {
            let x = i as f32 * 0.21;
            (0.25 + 0.55 * (x.sin() * (x * 0.37).cos()).abs() * (0.6 + 0.4 * (x * 1.7).sin())).clamp(0.05, 1.0)
        })
        .collect();
    let [left, centre, right] = look.hues;
    let wave = Wave {
        levels,
        newest_at: t,
        rate: 40.0,
        window: 6.0,
        stops: vec![(0.0, left), (0.5, centre), (1.0, right)],
        min_half: 0.5,
        baseline: Some(look.rule),
    };
    let text = "open the configuration file and set the popup theme to glass so the dictation panel \
                shows the desktop through a frosted pane";
    let status = Element::row("status")
        .align_items(AlignItems::CENTER)
        .margin_edges(10.0, 0.0, 0.0, 0.0)
        .child(Element::shape("dot", ShapeKind::RRect).size(7.0, 7.0).fill(look.accent))
        .child(
            Element::label("state", "LISTENING")
                .text_style(mono(12.0))
                .color(look.accent)
                .margin_edges(0.0, 0.0, 0.0, 10.0),
        )
        .child(
            Element::label("detail", "  release the key to finish")
                .text_style(mono(12.0).clamp_lines(1))
                .color(look.inactive),
        );
    let panel = Element::panel("panel")
        .width(PANEL_W)
        .pad_edges(17.0, 23.0, 13.0, 23.0)
        .radius(RADIUS)
        .child(header)
        .child(Element::wave("wave", wave).height(40.0).margin_edges(10.0, 0.0, 10.0, 0.0))
        .child(
            Element::label("text", text)
                .text_style(TextStyle::new(SANS, 22.0, 30.0))
                .color(look.foreground)
                .margin_edges(0.0, 0.0, 4.0, 0.0),
        )
        .child(Element::shape("rule", ShapeKind::RRect).height(1.0).fill(look.rule).margin_edges(12.0, 0.0, 0.0, 0.0))
        .child(status);
    Element::column("root").pad_edges(MARGIN - slide, MARGIN, MARGIN + slide, MARGIN).child(panel)
}

pub fn load_fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    for f in ["/usr/share/fonts/noto/NotoSans-Regular.ttf", "/usr/share/fonts/noto/NotoSansMono-Regular.ttf"] {
        fonts.add_file(f.as_ref()).unwrap_or_else(|e| panic!("cannot load {f}: {e}"));
    }
    fonts
}

/// The scene laid out once; returns it with the surface size and the panel rect.
pub fn layout(fonts: &FontRegistry, look: &Look) -> (Scene<()>, Size, [f32; 4]) {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(PANEL_W + 2.0 * MARGIN, 1000.0), 1.0);
    scene.apply(view(look, 0.0, 0.0), 0.0);
    let id = scene.find(&["root", "panel"]).expect("panel");
    let s = scene.sampled(id, 0.0).expect("panel drawn");
    let o = s.transform.apply(Point::new(0.0, 0.0));
    let size = Size::new(PANEL_W + 2.0 * MARGIN, (s.size.h + 2.0 * MARGIN).ceil());
    scene.set_viewport(0.0, size, 1.0);
    (scene, size, [o.x, o.y, s.size.w, s.size.h])
}

/// The glass panel at `t` seconds after show.
pub fn glass_at(look: &Look, rect: [f32; 4], t: f64, blur_behind: bool) -> GlassDraw {
    let orbit_t = t - SHEEN_SECS;
    GlassDraw {
        rect,
        radius: RADIUS,
        glass: Glass { accent: look.hues[0], ..look.glass },
        time: t as f32,
        orbit: (orbit_t >= 0.0).then(|| (orbit_t / ORBIT_SECS).fract() as f32),
        orbit_strength: (orbit_t / 0.4).clamp(0.0, 1.0) as f32 * 1.1,
        sheen: (t < SHEEN_SECS).then(|| (t / SHEEN_SECS) as f32),
        opacity: 1.0,
        blur_behind,
    }
}

/// Whether anything on the glass moves at `t`. Ambient loops run at 30 fps (FX08).
pub fn glass_fps(t: f64) -> f64 {
    if t < SHEEN_SECS {
        60.0
    } else {
        30.0
    }
}

// ---- offline ---------------------------------------------------------------------------

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    timestamps: bool,
    adapter: String,
}

fn gpu() -> Gpu {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("adapter");
    let want = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    let features = adapter.features() & want;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("glass-demo"),
        required_features: features,
        ..Default::default()
    }))
    .expect("device");
    Gpu { device, queue, timestamps: features == want, adapter: adapter.get_info().name }
}

fn read_rgba(g: &Gpu, target: &Target) -> Vec<u8> {
    let (w, h) = (target.width(), target.height());
    let row = (w * 4).div_ceil(256) * 256;
    let buf = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("glass-readback"),
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = g.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        target.texture().as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    let sub = g.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    g.device.poll(wgpu::PollType::Wait { submission_index: Some(sub), timeout: None }).expect("poll");
    let data = buf.slice(..).get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let s = (y * row) as usize;
        out.extend_from_slice(&data[s..s + (w * 4) as usize]);
    }
    out
}

struct Frame {
    rgba: Vec<u8>,
}

struct Offline {
    g: Gpu,
    renderer: Renderer,
    glass: GlassPass,
    fonts: FontRegistry,
    size: Size,
    rect: [f32; 4],
    list: DisplayList,
    query: Option<(wgpu::QuerySet, wgpu::Buffer, wgpu::Buffer)>,
}

impl Offline {
    /// Draws one frame at `scale`: the scene, then the glass behind it. With `glass_only` the
    /// scene is left out, which the contrast measurement needs. With `timed`, returns the GPU
    /// time of the whole frame and of the glass pass in milliseconds.
    fn frame(
        &mut self,
        look: &Look,
        t: f64,
        scale: f32,
        blur: bool,
        glass_only: bool,
        timed: bool,
    ) -> (Frame, Option<[f64; 2]>) {
        let (w, h) = ((self.size.w * scale).round() as u32, (self.size.h * scale).round() as u32);
        let target = Target::new(&self.g.device, w, h, vo_render::offscreen::FORMAT);
        // A fresh scene per frame, so a change of look or a step back in time starts settled
        // instead of animating from the last frame.
        let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(self.fonts.clone())));
        scene.set_viewport(t, self.size, 1.0);
        scene.apply(view(look, t, 0.0), t);
        scene.sample_into(t, &mut self.list);
        if glass_only {
            self.list.clear();
        }
        let view = target.texture().create_view(&Default::default());
        let draw = glass_at(look, self.rect, t, blur);
        self.glass.prepare(&self.g.device, &self.g.queue, [w, h], scale, &[draw]);
        let mut enc = self.g.device.create_command_encoder(&Default::default());
        let q = if timed { self.query.as_ref() } else { None };
        if let Some((set, _, _)) = q {
            enc.write_timestamp(set, 0);
        }
        self.renderer.encode(&self.list, scale, &target, &mut enc);
        let ts = q.map(|(set, _, _)| wgpu::RenderPassTimestampWrites {
            query_set: set,
            beginning_of_pass_write_index: Some(1),
            end_of_pass_write_index: Some(2),
        });
        self.glass.encode(&mut enc, &view, ts);
        if let Some((set, resolve, read)) = q {
            enc.resolve_query_set(set, 0..3, resolve, 0);
            enc.copy_buffer_to_buffer(resolve, 0, read, 0, 24);
        }
        let sub = self.g.queue.submit([enc.finish()]);
        self.g.device.poll(wgpu::PollType::Wait { submission_index: Some(sub), timeout: None }).expect("poll");
        let times = q.map(|(_, _, read)| {
            read.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
            self.g.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
            let ticks: Vec<u64> = {
                let d = read.slice(..).get_mapped_range().expect("mapped");
                d.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect()
            };
            read.unmap();
            let ns = self.g.queue.get_timestamp_period() as f64;
            [(ticks[2] - ticks[0]) as f64 * ns / 1e6, (ticks[2] - ticks[1]) as f64 * ns / 1e6]
        });
        (Frame { rgba: read_rgba(&self.g, &target) }, times)
    }
}

// ---- compositing for the sheet ---------------------------------------------------------

fn to_lin(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5) as u8
}

#[derive(Clone, Copy, PartialEq)]
enum Backdrop {
    Busy,
    White,
    Black,
}

/// Linear RGB backdrop, `w` by `h`, offset by `ox` so cells differ.
fn backdrop(kind: Backdrop, w: u32, h: u32, ox: u32) -> Vec<[f32; 3]> {
    (0..w * h)
        .map(|i| match kind {
            Backdrop::White => [1.0; 3],
            Backdrop::Black => [0.0; 3],
            Backdrop::Busy => {
                let (x, y) = (i % w + ox, i / w);
                let bars =
                    [[230, 40, 50], [250, 200, 30], [40, 180, 80], [30, 120, 230], [150, 50, 200], [240, 240, 240]];
                let mut c = bars[((x / 96 + y / 120) % 6) as usize];
                if (x / 6).is_multiple_of(2) && (y / 40).is_multiple_of(3) {
                    c = [0, 0, 0];
                }
                if y % 24 < 2 {
                    c = [255, 255, 255];
                }
                [to_lin(c[0]), to_lin(c[1]), to_lin(c[2])]
            }
        })
        .collect()
}

/// Three box blurs, close to a Gaussian, standing in for KWin's blur in the offline sheet.
fn blur(img: &[[f32; 3]], w: u32, h: u32, r: i32) -> Vec<[f32; 3]> {
    let mut a = img.to_vec();
    let mut b = a.clone();
    let (w, h) = (w as i32, h as i32);
    for _ in 0..3 {
        for y in 0..h {
            for x in 0..w {
                let mut s = [0.0f32; 3];
                for k in -r..=r {
                    let p = a[(y * w + (x + k).clamp(0, w - 1)) as usize];
                    s = [s[0] + p[0], s[1] + p[1], s[2] + p[2]];
                }
                let n = (2 * r + 1) as f32;
                b[(y * w + x) as usize] = [s[0] / n, s[1] / n, s[2] / n];
            }
        }
        for y in 0..h {
            for x in 0..w {
                let mut s = [0.0f32; 3];
                for k in -r..=r {
                    let p = b[((y + k).clamp(0, h - 1) * w + x) as usize];
                    s = [s[0] + p[0], s[1] + p[1], s[2] + p[2]];
                }
                let n = (2 * r + 1) as f32;
                a[(y * w + x) as usize] = [s[0] / n, s[1] / n, s[2] / n];
            }
        }
    }
    a
}

fn sd_rrect(px: f32, py: f32, r: [f32; 4], rad: f32) -> f32 {
    let (hw, hh) = (r[2] / 2.0, r[3] / 2.0);
    let qx = (px - r[0] - hw).abs() - hw + rad;
    let qy = (py - r[1] - hh).abs() - hh + rad;
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - rad
}

/// Overlay (premultiplied, sRGB encoded per channel) over a linear backdrop; with `blur`, the
/// backdrop under the panel shape is the blurred one, as the compositor would show it. With
/// `srgb_blend` the two are mixed on sRGB values, as a compositor without linear blending
/// does; otherwise in linear light.
fn composite(
    over: &[u8],
    back: &[[f32; 3]],
    blurred: Option<&[[f32; 3]]>,
    w: u32,
    rect: [f32; 4],
    srgb_blend: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(over.len());
    for (i, px) in over.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = ((i as u32 % w) as f32 + 0.5, (i as u32 / w) as f32 + 0.5);
        let mut b = back[i];
        if let Some(bl) = blurred {
            let cov = (0.5 - sd_rrect(x, y, rect, RADIUS)).clamp(0.0, 1.0);
            let q = bl[i];
            b = [b[0] + (q[0] - b[0]) * cov, b[1] + (q[1] - b[1]) * cov, b[2] + (q[2] - b[2]) * cov];
        }
        let a = px[3] as f32 / 255.0;
        for c in 0..3 {
            if srgb_blend {
                // Premultiplied sRGB of the overlay: its straight sRGB colour times alpha.
                let straight = if a > 0.0 { to_srgb(to_lin(px[c]) / a) } else { 0.0 };
                out.push((((straight * a) + to_srgb(b[c]) * (1.0 - a)) * 255.0 + 0.5).clamp(0.0, 255.0) as u8);
            } else {
                out.push(to_srgb8(to_lin(px[c]) + b[c] * (1.0 - a)));
            }
        }
        out.push(255);
    }
    out
}

fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

fn contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn lin_color(c: Color) -> [f32; 3] {
    [to_lin((c.r * 255.0).round() as u8), to_lin((c.g * 255.0).round() as u8), to_lin((c.b * 255.0).round() as u8)]
}

/// Highest and lowest luminance of the composited panel inside the text area (the panel minus
/// its padding): the worst cases for light and for dark text.
fn panel_luminance(rgb8: &[u8], w: u32, scale: f32, rect: [f32; 4]) -> (f32, f32) {
    let (x0, y0) = (((rect[0] + 23.0) * scale) as u32, ((rect[1] + 17.0) * scale) as u32);
    let (x1, y1) = (((rect[0] + rect[2] - 23.0) * scale) as u32, ((rect[1] + rect[3] - 13.0) * scale) as u32);
    let mut all = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            all.push((luminance([to_lin(rgb8[i]), to_lin(rgb8[i + 1]), to_lin(rgb8[i + 2])]), x, y));
        }
    }
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let p = |q: f32| all[((all.len() - 1) as f32 * q) as usize];
    (p(1.0).0, p(0.0).0)
}

fn run_offline(dir: &Path) {
    std::fs::create_dir_all(dir).expect("output folder");
    let t0 = Instant::now();
    let g = gpu();
    let t_dev = t0.elapsed().as_secs_f64() * 1e3;
    let fonts = load_fonts();
    let (_, size, rect) = layout(&fonts, &DARK);
    let t1 = Instant::now();
    let renderer = Renderer::with_fonts(&g.device, &g.queue, fonts.clone());
    let t_renderer = t1.elapsed().as_secs_f64() * 1e3;
    let t2 = Instant::now();
    let glass = GlassPass::new(&g.device, vo_render::offscreen::FORMAT, GlassBlend::Behind);
    let t_glass = t2.elapsed().as_secs_f64() * 1e3;
    let query = g.timestamps.then(|| {
        let set = g.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("glass"),
            ty: wgpu::QueryType::Timestamp,
            count: 3,
        });
        let buffer = |usage| {
            g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 24, usage, mapped_at_creation: false })
        };
        let resolve = buffer(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
        let read = buffer(wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
        (set, resolve, read)
    });
    let mut off = Offline { g, renderer, glass, fonts, size, rect, list: DisplayList::default(), query };
    let t3 = Instant::now();
    let _ = off.frame(&DARK, 0.0, 1.0, true, false, false);
    let t_first = t3.elapsed().as_secs_f64() * 1e3;
    println!("glass_demo: adapter {}", off.g.adapter);
    println!(
        "glass_demo: cold start: device {t_dev:.1} ms, renderer {t_renderer:.1} ms, glass pipeline {t_glass:.1} ms, \
         first frame with readback {t_first:.1} ms, total {:.1} ms",
        t0.elapsed().as_secs_f64() * 1e3
    );
    println!(
        "glass_demo: surface {}x{} logical, panel {:.0}x{:.0} at ({:.0}, {:.0})",
        size.w, size.h, rect[2], rect[3], rect[0], rect[1]
    );

    // GPU time per frame at 1x and 2x, during the ambient orbit.
    if off.query.is_some() {
        for scale in [1.0f32, 2.0] {
            let mut frames = Vec::new();
            for i in 0..60 {
                if let (_, Some(ms)) = off.frame(&DARK, 0.6 + i as f64 / 30.0, scale, true, false, true) {
                    frames.push(ms);
                }
            }
            let mut total: Vec<f64> = frames.iter().map(|m| m[0]).collect();
            let mut glass: Vec<f64> = frames.iter().map(|m| m[1]).collect();
            total.sort_by(f64::total_cmp);
            glass.sort_by(f64::total_cmp);
            let med = |v: &[f64]| v[v.len() / 2];
            println!(
                "glass_demo: GPU at {scale}x ({}x{} px): frame median {:.3} ms max {:.3} ms, glass pass median {:.3} ms max {:.3} ms",
                (size.w * scale) as u32,
                (size.h * scale) as u32,
                med(&total),
                total[total.len() - 1],
                med(&glass),
                glass[glass.len() - 1],
            );
        }
    } else {
        println!("glass_demo: no timestamp queries on this adapter, no GPU times");
    }

    // AX01, measured: the panel alone over pure white and pure black, blended in linear light
    // and in sRGB, the worst pixel in the text area against each text colour. The orbit is
    // on at t = 3, so its glow is counted.
    let (w, h) = (size.w as u32, size.h as u32);
    let mut gate_ok = true;
    for look in [&DARK, &LIGHT] {
        let (panel, _) = off.frame(look, 3.0, 1.0, false, true, false);
        let texts = [("body", look.foreground), ("captions", look.inactive), ("status", look.accent)];
        let mut worst = [f32::INFINITY; 3];
        let mut lums = Vec::new();
        for kind in [Backdrop::White, Backdrop::Black] {
            for srgb in [false, true] {
                let c = composite(&panel.rgba, &backdrop(kind, w, h, 0), None, w, rect, srgb);
                let (max, min) = panel_luminance(&c, w, 1.0, rect);
                lums.push(format!("{:.3}..{:.3}", min, max));
                for (k, (_, color)) in texts.iter().enumerate() {
                    let tl = luminance(lin_color(*color));
                    // Light text is worst on the brightest pixel, dark text on the darkest.
                    let l = if tl > max { max } else { min };
                    worst[k] = worst[k].min(contrast(tl, l));
                }
            }
        }
        let ok = worst.iter().all(|&c| c >= 4.5);
        gate_ok &= ok;
        println!(
            "glass_demo: AX01 {} glass: panel luminance {} (white lin, white sRGB, black lin, black sRGB); worst body {:.2}:1, captions {:.2}:1, status {:.2}:1 ({})",
            look.name,
            lums.join(", "),
            worst[0],
            worst[1],
            worst[2],
            if ok { "pass" } else { "FAIL" }
        );
    }

    // The contact sheet: for each look, rows are backdrops, columns are times after show.
    let times = [0.18, 0.36, 1.6, 3.4];
    let rows: [(&str, Backdrop, bool); 4] = [
        ("busy, compositor blur", Backdrop::Busy, true),
        ("busy, no blur", Backdrop::Busy, false),
        ("pure white", Backdrop::White, false),
        ("pure black", Backdrop::Black, false),
    ];
    let gap = 8u32;
    let nrows = 2 * rows.len() as u32;
    let (sw, sh) = (times.len() as u32 * (w + gap) + gap, nrows * (h + gap) + gap);
    let mut sheet = vec![40u8; (sw * sh * 4) as usize];
    let mut ri = 0u32;
    for look in [&DARK, &LIGHT] {
        for (label, kind, blur_on) in rows.iter() {
            for (ci, &t) in times.iter().enumerate() {
                let (f, _) = off.frame(look, t, 1.0, *blur_on, false, false);
                let back = backdrop(*kind, w, h, ci as u32 * 53);
                let blurred = blur_on.then(|| blur(&back, w, h, 9));
                let cell = composite(&f.rgba, &back, blurred.as_deref(), w, rect, false);
                if *blur_on {
                    let name = format!("glass-{}-t{t:.2}.png", look.name);
                    vo_render::offscreen::write_png(&dir.join(name), w, h, &cell).expect("png");
                }
                let (ox, oy) = (gap + ci as u32 * (w + gap), gap + ri * (h + gap));
                for y in 0..h {
                    let s = (y * w * 4) as usize;
                    let d = (((oy + y) * sw + ox) * 4) as usize;
                    sheet[d..d + (w * 4) as usize].copy_from_slice(&cell[s..s + (w * 4) as usize]);
                }
            }
            println!("glass_demo: sheet row {ri}: {} glass, {label}", look.name);
            ri += 1;
        }
    }
    let path = dir.join("glass-sheet.png");
    vo_render::offscreen::write_png(&path, sw, sh, &sheet).expect("png");
    println!("glass_demo: columns at t = {times:?} s; contact sheet {}", path.display());
    if !gate_ok {
        std::process::exit(1);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    if let Some(dir) = val("--offline") {
        run_offline(&PathBuf::from(dir));
    } else if args.iter().any(|a| a == "--live") {
        let secs = val("--secs").and_then(|s| s.parse().ok()).unwrap_or(8.0);
        let look = if args.iter().any(|a| a == "--light") { LIGHT } else { DARK };
        live::run(look, secs, !args.iter().any(|a| a == "--no-blur"));
    } else {
        eprintln!("usage: glass_demo --offline DIR | --live [--secs S] [--no-blur] [--light]");
        std::process::exit(2);
    }
}
