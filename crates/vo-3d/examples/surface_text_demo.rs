//! Text on curved surfaces (`vo_3d::SurfaceText`, `Scroll`, `PageTurn`, EG-14): two lines
//! of text printed on a scroll that unrolls and on a page that turns over.
//!
//!   cargo run --release -p vo-3d --example surface_text_demo -- [--out DIR]
//!   cargo run --release -p vo-3d --example surface_text_demo -- --bench
//!
//! The default writes `DIR/surface_text_demo.png` (default DIR `/var/tmp/claude/swarm/EG-14`):
//! the scroll, the page, the page on a light background and the scroll under reduced motion,
//! at six values of `unroll` or `turn` from 0 to 1, one row each. `--bench` prints the GPU
//! time per frame (timestamp queries) of a 600x120 page moving in a 600x300 frame, and the
//! texture memory the text holds; without timestamp queries it says so and exits 0.

use std::path::PathBuf;

use vo_3d::surface_text::{magnification, texel_scale};
use vo_3d::*;
use vo_render::offscreen::write_png;
use vo_render::Renderer;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The page in logical pixels.
const PAGE: [f32; 2] = [600.0, 120.0];

fn page_text(fonts: &FontRegistry, ink: Color) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(PAGE[0], PAGE[1]), 1.0);
    let line = |key: &'static str, text: &str| {
        Element::label(key, text).text_style(TextStyle::new(0, 24.0, 31.0)).color(ink)
    };
    let view = Element::column("root")
        .size(PAGE[0], PAGE[1])
        .pad(16.0)
        .child(line("a", "Older lines roll away on a scroll,"))
        .child(line("b", "and the page turns to show what comes next."));
    scene.apply(view, 0.0);
    scene.sample(10.0)
}

#[derive(Clone, Copy)]
enum Shape {
    Scroll(Scroll),
    Page(PageTurn),
}

impl Shape {
    fn mesh(&self) -> Mesh {
        match self {
            Shape::Scroll(s) => s.mesh(),
            Shape::Page(p) => p.mesh(),
        }
    }

    fn vertices(&self, x: f32, out: &mut Vec<Vertex>) {
        match self {
            Shape::Scroll(s) => s.vertices(x, out),
            Shape::Page(p) => p.vertices(x, out),
        }
    }

    fn length(&self) -> f32 {
        match self {
            Shape::Scroll(s) => s.length,
            Shape::Page(p) => p.length,
        }
    }
}

struct Row {
    shape: Shape,
    background: Color,
    paper: Color,
    back: Color,
    ink: Color,
}

fn rows() -> Vec<Row> {
    let dark = Color::hex(0x07090f, 1.0);
    let (paper, back, ink) = (Color::hex(0xf3ecd8, 1.0), Color::hex(0xc9bf9f, 1.0), Color::hex(0x1d2030, 1.0));
    vec![
        Row { shape: Shape::Scroll(Scroll::default()), background: dark, paper, back, ink },
        Row { shape: Shape::Page(PageTurn::default()), background: dark, paper, back, ink },
        Row {
            shape: Shape::Page(PageTurn::default()),

            background: Color::hex(0xf2efe8, 1.0),
            paper: Color::WHITE,
            back: Color::hex(0xdfe6f5, 1.0),
            ink: Color::hex(0x2a4bd8, 1.0),
        },
        Row {
            shape: Shape::Scroll(Scroll { reduced_motion: true, ..Scroll::default() }),

            background: dark,
            paper,
            back,
            ink,
        },
    ]
}

/// A surface with its text, drawn into frames.
struct Stage {
    flat: Renderer,
    r3: Renderer3d,
    text: SurfaceText,
    tex: Option<TextureId>,
    generation: u64,
    mesh: MeshId,
    shape: Shape,
    verts: Vec<Vertex>,
}

impl Stage {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, fonts: &FontRegistry, shape: Shape) -> Stage {
        let mut r3 = Renderer3d::new(device, queue);
        let mesh = r3.add_mesh(&shape.mesh());
        Stage {
            flat: Renderer::with_fonts(device, queue, fonts.clone()),
            r3,
            text: SurfaceText::new(device),
            tex: None,
            generation: 0,
            mesh,
            shape,
            verts: Vec::new(),
        }
    }

    /// Records the frame at `x` into `enc`; the text texture is drawn first when it changed.
    #[allow(clippy::too_many_arguments)]
    fn encode(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        list: &DisplayList,
        row: &Row,
        x: f32,
        view: &wgpu::TextureView,
        size: [u32; 2],
        scale: f32,
    ) {
        let camera = Camera::orbit(Vec3::new(0.0, 0.0, -0.1), -0.3, 0.3, 2.8);
        self.shape.vertices(x, &mut self.verts);
        self.r3.update_mesh(self.mesh, &self.verts);
        let world_per_px = self.shape.length() / PAGE[0];
        let m = magnification(&camera, size, &Mat4::IDENTITY, &self.verts, world_per_px, scale);
        self.text.update(&mut self.flat, list, PAGE, texel_scale(PAGE, scale, m), enc);
        let tv = self.text.view().unwrap();
        match self.tex {
            None => self.tex = Some(self.r3.add_texture(tv)),
            Some(id) if self.generation != self.text.generation => self.r3.set_texture(id, tv),
            Some(_) => {}
        }
        self.generation = self.text.generation;
        let frame = Frame3d {
            camera,
            background: Background::Solid(row.background),
            meshes: vec![MeshDraw {
                mesh: self.mesh,
                model: Mat4::IDENTITY,
                material: Material {
                    color: row.paper,
                    back: Some(row.back),
                    specular: 0.2,
                    emissive: 0.3,
                    texture: self.tex,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        self.r3.encode_with(enc, Output { view, size, format: FORMAT, scale }, &frame, None, None);
    }
}

fn gpu(timestamps: bool) -> Option<(wgpu::Device, wgpu::Queue)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a Vulkan adapter");
    let features = if timestamps { vo_3d::timer::FEATURES } else { wgpu::Features::empty() };
    if !adapter.features().contains(features) {
        return None;
    }
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("surface-text-demo"),
            required_features: features,
            ..Default::default()
        }))
        .expect("a GPU device"),
    )
}

fn target(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("surface-text-demo"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).expect("the test font");
    fonts
}

fn bench() {
    let Some((device, queue)) = gpu(true) else {
        println!("surface_text_demo bench: this GPU has no timestamp queries; no timing");
        return;
    };
    let fonts = fonts();
    let (w, h) = (600u32, 300u32);
    let tex = target(&device, w, h);
    let view = tex.create_view(&Default::default());
    let mut timer = GpuTimer::new(&device, &queue).expect("timestamp queries");
    for row in rows().into_iter().take(2) {
        let list = page_text(&fonts, row.ink);
        let mut stage = Stage::new(&device, &queue, &fonts, row.shape);
        let mut times = Vec::new();
        for i in 0..240 {
            // Unroll or turn back and forth, a full cycle every 4 s at 60 fps.
            let x = 0.5 - 0.5 * (i as f32 / 240.0 * std::f32::consts::TAU).cos();
            let mut enc = device.create_command_encoder(&Default::default());
            timer.begin();
            timer.mark(&mut enc, "start");
            stage.encode(&mut enc, &list, &row, x, &view, [w, h], 1.0);
            timer.mark(&mut enc, "frame");
            queue.submit([enc.finish()]);
            let ms: f64 = timer.read(&device, &queue).iter().map(|(_, ms)| ms).sum();
            if i >= 40 {
                times.push(ms);
            }
        }
        times.sort_by(f64::total_cmp);
        let (median, p95) = (times[times.len() / 2], times[times.len() * 95 / 100]);
        let name = match row.shape {
            Shape::Scroll(_) => "scroll",
            Shape::Page(_) => "page",
        };
        let [tw, th] = stage.text.size().unwrap();
        println!(
            "surface_text_demo bench: {name:6} {w}x{h}, 600x120 page: {median:.3} ms median, {p95:.3} ms p95; \
             text drawn {} times; texture {tw}x{th}, {:.2} MB with mipmaps and the 2D target",
            stage.text.draws,
            stage.text.bytes() as f64 / (1024.0 * 1024.0),
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/swarm/EG-14");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    let (w, h) = (300u32, 150u32);
    let (device, queue) = gpu(false).expect("a GPU device");
    let fonts = fonts();
    let tex = target(&device, w, h);
    let view = tex.create_view(&Default::default());
    let rows = rows();
    let (cols, gap) = (6u32, 6u32);
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows.len() as u32 * h + (rows.len() as u32 + 1) * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (r, row) in rows.iter().enumerate() {
        let list = page_text(&fonts, row.ink);
        let mut stage = Stage::new(&device, &queue, &fonts, row.shape);
        for c in 0..cols {
            let x = c as f32 / (cols - 1) as f32;
            let mut enc = device.create_command_encoder(&Default::default());
            stage.encode(&mut enc, &list, row, x, &view, [w, h], 1.0);
            queue.submit([enc.finish()]);
            let rgba = readback::read_rgba(&device, &queue, &tex);
            let (x0, y0) = (gap + c * (w + gap), gap + r as u32 * (h + gap));
            for y in 0..h {
                let s = (y * w * 4) as usize;
                let d = (((y0 + y) * sw + x0) * 4) as usize;
                sheet[d..d + (w * 4) as usize].copy_from_slice(&rgba[s..s + (w * 4) as usize]);
            }
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("surface_text_demo.png");
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("surface_text_demo: wrote {}", path.display());
}
