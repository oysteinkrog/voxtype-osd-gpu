//! The chrome hero reflecting the popup itself (EG-17, `vo_3d::env`): a chrome orb over a
//! panel with a red bar below and left of it and a waveform of bars to its right, inside a
//! layer whose backdrop (EG-01) captures them. The red shows on the orb's lower left; the
//! environment is rebuilt only when what it shows changes, at most 30 times a second; and
//! without a backdrop the chrome draws its studio exactly as with no environment.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::{Renderer, Target};
use vo_scene::display::{BackdropRes, DisplayList};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::{taffy, Scene};

const W: u32 = 600;
const H: u32 = 300;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DARK: u32 = 0x0b0e16;
const LIGHT: u32 = 0xe9e4da;
/// The orb: centre and box size in logical pixels.
const CX: f32 = 300.0;
const CY: f32 = 140.0;
const D: f32 = 120.0;

#[derive(Debug)]
struct Look(HeroFrame);

impl HeroSource for Look {
    fn frame(&self, _t: f64, _size: Size) -> HeroFrame {
        self.0
    }

    fn until(&self) -> f64 {
        0.0
    }
}

fn orb(env: Option<Env>) -> HeroFrame {
    HeroFrame {
        shape: HeroShape::Orb,
        look: HeroLook::Chrome {
            tint: Color::hex(0xe8eef8, 1.0),
            top: Color::hex(0x9aa8c0, 1.0),
            bottom: Color::hex(0x0a0c12, 1.0),
            light: Color::hex(0xffffff, 1.0),
            env,
        },
        stretch: [0.9; 3],
        ..Default::default()
    }
}

type El = Element<()>;

fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

/// The panel, a red bar below left of the orb, a waveform to its right, and the orb, in a
/// layer that captures its backdrop when `backdrop`.
fn view(h: &Arc<dyn GpuSource>, back: u32, backdrop: bool) -> El {
    let wave = Element::row("wave").gap(4.0).align_items(taffy::AlignItems::CENTER).children((0..14).map(|i| {
        let hgt = 20.0 + 60.0 * (0.5 + 0.5 * (i as f32 * 0.9).sin());
        Element::panel(i as u64).size(6.0, hgt).radius(3.0).fill(Color::hex(0x35c2ff, 1.0))
    }));
    let layer = Element::layer("hero-layer").size(W as f32, H as f32);
    let layer = if backdrop { layer.backdrop(BackdropRes::Half, Color::TRANSPARENT) } else { layer };
    let layer = layer.child(at(Element::gpu("hero", h.clone()).size(D, D), CX - D / 2.0, CY - D / 2.0));
    Element::panel("root")
        .size(W as f32, H as f32)
        .fill(Color::hex(back, 1.0))
        .child(at(Element::panel("red").size(190.0, 100.0).radius(8.0).fill(Color::hex(0xff2020, 1.0)), 90.0, 180.0))
        .child(at(wave, 400.0, 100.0))
        .child(layer)
}

fn sample(h: &Arc<dyn GpuSource>, back: u32, backdrop: bool, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    scene.apply(view(h, back, backdrop), 0.0);
    scene.sample(t)
}

fn render(gpu: &support::Gpu, r: &mut Renderer, list: &DisplayList, scale: f32) -> (u32, u32, Vec<u8>) {
    let (w, h) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
    let mut target = Target::new(&gpu.device, w, h, FORMAT);
    r.render_at(list, scale, &mut target);
    (w, h, vo_3d::readback::read_rgba(&gpu.device, &gpu.queue, target.texture()))
}

fn renderer(gpu: &support::Gpu) -> Renderer {
    let mut r = Renderer::new(&gpu.device, &gpu.queue);
    install(&mut r);
    r
}

fn linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Mean linear red over the orb's pixels (inside 0.8 of its radius) whose offset from the
/// centre has signs `sx, sy` (y down).
fn quarter_red(px: &[u8], w: u32, sx: f32, sy: f32) -> f32 {
    let r = 0.5 * D * 0.9 * 0.8;
    let (mut sum, mut n) = (0.0, 0);
    for y in (CY - r) as u32..(CY + r) as u32 {
        for x in (CX - r) as u32..(CX + r) as u32 {
            let (dx, dy) = (x as f32 + 0.5 - CX, y as f32 + 0.5 - CY);
            if dx * dx + dy * dy > r * r || dx * sx <= 0.0 || dy * sy <= 0.0 {
                continue;
            }
            sum += linear(px[((y * w + x) * 4) as usize]);
            n += 1;
        }
    }
    sum / n as f32
}

#[test]
fn the_orb_reflects_the_red_bar_on_its_lower_left() {
    let gpu = support::gpu();
    let mut r = renderer(&gpu);
    let h = hero(Look(orb(Some(Env::default()))));
    let (w, _, px) = render(&gpu, &mut r, &sample(&h, DARK, true, 1.0), 1.0);
    let (lower_left, upper_right) = (quarter_red(&px, w, -1.0, 1.0), quarter_red(&px, w, 1.0, -1.0));
    eprintln!("mean linear red: lower left {lower_left:.3}, upper right {upper_right:.3}");
    assert!(lower_left > upper_right + 0.1, "lower left {lower_left:.3}, upper right {upper_right:.3}");
    // The studio alone has no such tilt toward red.
    let h = hero(Look(orb(None)));
    let (w, _, px) = render(&gpu, &mut r, &sample(&h, DARK, true, 1.0), 1.0);
    let (ll, ur) = (quarter_red(&px, w, -1.0, 1.0), quarter_red(&px, w, 1.0, -1.0));
    assert!(ll < ur + 0.1, "the studio alone: lower left {ll:.3}, upper right {ur:.3}");
}

#[test]
fn env_is_rebuilt_only_when_it_changes() {
    let gpu = support::gpu();
    let mut r = renderer(&gpu);
    // A key that stays the same: 30 frames at 60 fps, one rebuild.
    let h = hero(Look(orb(Some(Env { key: Some(7), ..Env::default() }))));
    let before = env::rebuilds();
    for i in 0..30 {
        render(&gpu, &mut r, &sample(&h, DARK, true, 1.0 + i as f64 / 60.0), 1.0);
    }
    let once = env::rebuilds() - before;
    eprintln!("unchanged backdrop, 30 frames: {once} rebuild(s)");
    assert_eq!(once, 1);
    // No key: rebuilt every 1/30 s tick, so 15 or 16 times over half a second at 60 fps.
    let h = hero(Look(orb(Some(Env::default()))));
    let before = env::rebuilds();
    for i in 0..30 {
        render(&gpu, &mut r, &sample(&h, DARK, true, 1.0 + i as f64 / 60.0), 1.0);
    }
    let ticks = env::rebuilds() - before;
    eprintln!("no key, 30 frames at 60 fps: {ticks} rebuilds");
    assert!((15..=16).contains(&ticks), "{ticks}");
    eprintln!("env texture: {} KB", vo_3d::env::ENV_BYTES / 1024);
}

#[test]
fn same_t_same_bytes() {
    let gpu = support::gpu();
    let h = hero(Look(orb(Some(Env::default()))));
    let list = sample(&h, DARK, true, 1.3);
    let a = render(&gpu, &mut renderer(&gpu), &list, 1.0).2;
    let mut r = renderer(&gpu);
    let b = render(&gpu, &mut r, &list, 1.0).2;
    let c = render(&gpu, &mut r, &list, 1.0).2;
    assert!(a == b && b == c, "same t, different bytes");
}

#[test]
fn without_a_backdrop_the_studio_is_unchanged() {
    let gpu = support::gpu();
    let mut r = renderer(&gpu);
    let with = render(&gpu, &mut r, &sample(&hero(Look(orb(Some(Env::default())))), DARK, false, 1.0), 1.0).2;
    let without = render(&gpu, &mut r, &sample(&hero(Look(orb(None))), DARK, false, 1.0), 1.0).2;
    assert!(with == without, "an environment with no backdrop changed the chrome");
}

#[test]
fn env_goldens() {
    let gpu = support::gpu();
    let h = hero(Look(orb(Some(Env::default()))));
    let mut errors = Vec::new();
    for (name, back, scale) in
        [("hero-env", DARK, 1.0f32), ("hero-env-1.5x", DARK, 1.5), ("hero-env-light", LIGHT, 1.0)]
    {
        // A fresh renderer each: the environment holds the last build within a 1/30 s tick.
        let (w, hgt, px) = render(&gpu, &mut renderer(&gpu), &sample(&h, back, true, 1.0), scale);
        if let Err(e) = support::check(&Image { name: name.into(), w, h: hgt, channels: 4, px }, Tolerance::default()) {
            errors.push(e);
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
