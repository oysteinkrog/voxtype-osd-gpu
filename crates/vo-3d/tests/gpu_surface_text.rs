//! Text on curved surfaces (EG-14): goldens of the scroll at unroll 0.5 and the page at turn
//! 0.5 at scale 1 and 1.5, on a light background and under reduced motion; a flat page that
//! matches the 2D renderer; the texture drawn once for settled text; the same `t` giving the
//! same bytes. GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_3d::surface_text::{magnification, texel_scale};
use vo_3d::*;
use vo_render::{Renderer, Target};
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const TOL: Tolerance = Tolerance { per_pixel: 3, max_fraction: 0.002 };
/// The page in logical pixels.
const PAGE: [f32; 2] = [600.0, 120.0];

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

/// Two lines of `size` px text in `ink` on a `PAGE` sized page.
fn page_text(fonts: &FontRegistry, size: f32, ink: Color) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(PAGE[0], PAGE[1]), 1.0);
    let line = |key: &'static str, text: &str| {
        Element::label(key, text).text_style(TextStyle::new(0, size, size * 1.3)).color(ink)
    };
    let view = Element::column("root")
        .size(PAGE[0], PAGE[1])
        .pad(16.0)
        .child(line("a", "Older lines roll away on a scroll,"))
        .child(line("b", "and the page turns to show what comes next."));
    scene.apply(view, 0.0);
    scene.sample(10.0)
}

enum Shape {
    Scroll(Scroll, f32),
    Page(PageTurn, f32),
}

impl Shape {
    fn mesh(&self) -> Mesh {
        match self {
            Shape::Scroll(s, _) => s.mesh(),
            Shape::Page(p, _) => p.mesh(),
        }
    }

    fn vertices(&self, out: &mut Vec<Vertex>) {
        match self {
            Shape::Scroll(s, u) => s.vertices(*u, out),
            Shape::Page(p, t) => p.vertices(*t, out),
        }
    }

    fn length(&self) -> f32 {
        match self {
            Shape::Scroll(s, _) => s.length,
            Shape::Page(p, _) => p.length,
        }
    }
}

struct Look {
    camera: Camera,
    background: Background,
    light: Light,
    paper: Color,
    back: Color,
    specular: f32,
    emissive: f32,
}

fn dark() -> Look {
    Look {
        camera: Camera::orbit(Vec3::new(0.0, 0.0, -0.1), -0.3, 0.3, 2.8),
        background: Background::Solid(Color::hex(0x07090f, 1.0)),
        light: Light::default(),
        paper: Color::hex(0xf3ecd8, 1.0),
        back: Color::hex(0xc9bf9f, 1.0),
        specular: 0.2,
        emissive: 0.3,
    }
}

/// Everything one surface needs on the GPU.
struct Rig {
    flat: Renderer,
    r3: Renderer3d,
    text: SurfaceText,
    tex: Option<TextureId>,
    generation: u64,
}

impl Rig {
    fn new(gpu: &support::Gpu, fonts: FontRegistry) -> Rig {
        Rig {
            flat: Renderer::with_fonts(&gpu.device, &gpu.queue, fonts),
            r3: Renderer3d::new(&gpu.device, &gpu.queue),
            text: SurfaceText::new(&gpu.device),
            tex: None,
            generation: 0,
        }
    }

    /// Draws the text into the surface texture at `k` when it changed; registers or moves
    /// the texture id.
    fn update_text(&mut self, gpu: &support::Gpu, list: &DisplayList, k: f32) -> bool {
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let drawn = self.text.update(&mut self.flat, list, PAGE, k, &mut enc);
        gpu.queue.submit([enc.finish()]);
        let view = self.text.view().unwrap();
        match self.tex {
            None => self.tex = Some(self.r3.add_texture(view)),
            Some(id) if self.generation != self.text.generation => self.r3.set_texture(id, view),
            Some(_) => {}
        }
        self.generation = self.text.generation;
        drawn
    }

    /// Draws `shape` with the text on it into a `w` x `h` logical target at `scale`.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
        gpu: &support::Gpu,
        list: &DisplayList,
        shape: &Shape,
        look: &Look,
        w: u32,
        h: u32,
        scale: f32,
        format: wgpu::TextureFormat,
    ) -> (u32, u32, Vec<u8>) {
        let (pw, ph) = ((w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32);
        let mesh = shape.mesh();
        let id = self.r3.add_mesh(&mesh);
        let mut v = Vec::new();
        shape.vertices(&mut v);
        self.r3.update_mesh(id, &v);
        let world_per_px = shape.length() / PAGE[0];
        let m = magnification(&look.camera, [pw, ph], &Mat4::IDENTITY, &v, world_per_px, scale);
        self.update_text(gpu, list, texel_scale(PAGE, scale, m));
        let frame = Frame3d {
            camera: look.camera,
            background: look.background,
            light: look.light,
            meshes: vec![MeshDraw {
                mesh: id,
                model: Mat4::IDENTITY,
                material: Material {
                    color: look.paper,
                    back: Some(look.back),
                    specular: look.specular,
                    emissive: look.emissive,
                    texture: self.tex,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gpu_surface_text"),
            size: wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        self.r3.encode_with(&mut enc, Output { view: &view, size: [pw, ph], format, scale }, &frame, None, None);
        gpu.queue.submit([enc.finish()]);
        (pw, ph, readback::read_rgba(&gpu.device, &gpu.queue, &tex))
    }
}

#[test]
fn surface_text_goldens() {
    let gpu = support::gpu();
    let fonts = fonts();
    let ink = page_text(&fonts, 24.0, Color::hex(0x1d2030, 1.0));
    let (w, h) = (300, 150);
    let mut results = Vec::new();
    // A fresh rig per image, so no texture carries over from the one before.
    let mut check = |name: &str, list: &DisplayList, shape: Shape, look: &Look, scale: f32| {
        let mut rig = Rig::new(&gpu, fonts.clone());
        let (pw, ph, px) = rig.draw(&gpu, list, &shape, look, w, h, scale, FORMAT);
        results.push(support::check(&Image { name: name.into(), w: pw, h: ph, channels: 4, px }, TOL));
    };
    let scroll = || Shape::Scroll(Scroll::default(), 0.5);
    let page = || Shape::Page(PageTurn::default(), 0.5);
    check("3d-scroll-0.5", &ink, scroll(), &dark(), 1.0);
    check("3d-scroll-0.5-1.5x", &ink, scroll(), &dark(), 1.5);
    check("3d-page-0.5", &ink, page(), &dark(), 1.0);
    check("3d-page-0.5-1.5x", &ink, page(), &dark(), 1.5);
    let light = Look {
        background: Background::Solid(Color::hex(0xf2efe8, 1.0)),
        paper: Color::hex(0xffffff, 1.0),
        back: Color::hex(0xdfe6f5, 1.0),
        ..dark()
    };
    let blue = page_text(&fonts, 24.0, Color::hex(0x2a4bd8, 1.0));
    check("3d-page-light", &blue, page(), &light, 1.0);
    let reduced = Shape::Scroll(Scroll { reduced_motion: true, ..Scroll::default() }, 0.5);
    check("3d-scroll-reduced", &ink, reduced, &dark(), 1.0);
    let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// A flat page facing an orthographic camera one world unit per pixel, lit by plain white
/// ambient light, shows exactly what the 2D renderer draws.
#[test]
fn flat_page_matches_the_2d_renderer() {
    let gpu = support::gpu();
    let fonts = fonts();
    let list = page_text(&fonts, 24.0, Color::hex(0xf2f4fa, 1.0));
    let mut rig = Rig::new(&gpu, fonts);
    let (w, h) = (PAGE[0] as u32, PAGE[1] as u32);
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let shape = Shape::Page(PageTurn { length: PAGE[0], width: PAGE[1], ..PageTurn::default() }, 0.0);
    let look = Look {
        camera: Camera {
            projection: Projection::Orthographic { height: PAGE[1] },
            ..Camera::orbit(Vec3::ZERO, 0.0, 0.0, 10.0)
        },
        background: Background::Transparent,
        light: Light { intensity: 0.0, ambient: Color::WHITE, rim: 0.0, ..Light::default() },
        paper: Color::TRANSPARENT,
        back: Color::TRANSPARENT,
        specular: 0.0,
        emissive: 0.0,
    };
    let (_, _, px3) = rig.draw(&gpu, &list, &shape, &look, w, h, 1.0, format);
    assert_eq!(rig.text.size(), Some([w, h]), "one texel per pixel");
    let target = Target::new(&gpu.device, w, h, format);
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    rig.flat.encode(&list, 1.0, &target, &mut enc);
    gpu.queue.submit([enc.finish()]);
    let px2 = readback::read_rgba(&gpu.device, &gpu.queue, target.texture());
    let ink = px2.as_chunks::<4>().0.iter().filter(|p| p[3] > 128).count();
    assert!(ink > 2000, "the 2D text has only {ink} inked pixels");
    let sum: u64 = px3.iter().zip(&px2).map(|(a, b)| a.abs_diff(*b) as u64).sum();
    let mean = sum as f64 / px2.len() as f64;
    let worst = px3.iter().zip(&px2).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
    eprintln!("flat page against the 2D renderer: mean difference {mean:.4}/255, worst {worst}/255");
    assert!(mean <= 2.0, "mean difference {mean:.3}/255");
}

/// The text is drawn into the texture once while it stays the same, and the texture follows
/// the scale.
#[test]
fn settled_text_is_drawn_once() {
    let gpu = support::gpu();
    let fonts = fonts();
    let list = page_text(&fonts, 24.0, Color::WHITE);
    let mut rig = Rig::new(&gpu, fonts.clone());
    let shape = |u: f32| Shape::Scroll(Scroll::default(), u);
    for i in 0..60 {
        rig.draw(&gpu, &list, &shape(i as f32 / 59.0), &dark(), 160, 80, 1.0, FORMAT);
    }
    assert_eq!(rig.text.draws, 1, "settled text drawn {} times in 60 frames", rig.text.draws);
    let other = page_text(&fonts, 24.0, Color::hex(0xff8040, 1.0));
    assert!(rig.update_text(&gpu, &other, 1.0), "new text");
    assert_eq!(rig.text.draws, 2);

    let at = |scale: f32| {
        let mut rig = Rig::new(&gpu, fonts.clone());
        rig.update_text(&gpu, &list, texel_scale(PAGE, scale, 1.0));
        rig.text.size().unwrap()
    };
    let (one, more) = (at(1.0), at(1.5));
    assert_eq!(one, [600, 120]);
    assert_eq!(more, [900, 180], "1.5x the size at scale 1");
}

/// The same `t` gives the same bytes, whatever was drawn in between.
#[test]
fn same_t_same_bytes() {
    let gpu = support::gpu();
    let fonts = fonts();
    let list = page_text(&fonts, 24.0, Color::hex(0x1d2030, 1.0));
    let mut rig = Rig::new(&gpu, fonts);
    let mut shot = |shape: Shape| rig.draw(&gpu, &list, &shape, &dark(), 200, 100, 1.0, FORMAT).2;
    let a = shot(Shape::Page(PageTurn::default(), 0.37));
    let b = shot(Shape::Page(PageTurn::default(), 0.71));
    let c = shot(Shape::Page(PageTurn::default(), 0.37));
    assert_ne!(a, b, "the page did not move");
    assert_eq!(a, c, "drawing turn 0.37 again changed the pixels");
    let d = shot(Shape::Scroll(Scroll::default(), 0.37));
    let e = shot(Shape::Scroll(Scroll::default(), 0.37));
    assert_eq!(d, e);
}
