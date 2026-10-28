//! Displacement (EG-03): a layer redraws what is under it with each pixel moved.
//!
//! The demo is a bar waveform and two lines of live text under a layer that covers the whole
//! popup. Two ripple rings start at 0.2 s and 0.5 s and are seen at 0.8 s: the waveform and
//! the older line bend, the newest (reading) line stays exactly as it was. Each other source
//! (noise, curl, page curl, a texture of offsets) has a golden of its own on the same scene.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::displace::{Displace, PageCurl, Ripples};
use vo_scene::display::{BackdropRes, DisplayList};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Point, Size};
use vo_scene::taffy;
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

type El = Element<()>;

const W: f32 = 360.0;
const H: f32 = 150.0;

/// `e` placed at `x, y` in its parent, out of the flow.
fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

fn words() -> Vec<Word> {
    ["ripples", "bend", "the", "older", "line", "while", "the", "newest", "holds"]
        .iter()
        .enumerate()
        .map(|(i, w)| Word { id: i as u32 + 1, text: (*w).into() })
        .collect()
}

/// Bars of a waveform: 48 capsules whose heights follow two sines.
fn waveform() -> El {
    Element::row("wave").gap(3.0).align_items(vo_scene::taffy::AlignItems::CENTER).height(40.0).children(
        (0..48u32).map(|i| {
            let x = i as f32;
            let h = 6.0 + 30.0 * (0.5 + 0.5 * (x * 0.37).sin() * (x * 0.11).cos()).abs();
            Element::panel(i as u64).size(4.0, h).radius(2.0).fill(Color::hex(0x6cb6ff, 1.0))
        }),
    )
}

fn ripples() -> Displace {
    Displace::Ripples(
        Ripples::new().onset(0.2, Point::new(110.0, 60.0)).onset(0.5, Point::new(250.0, 70.0)).amp(5.0).wave(16.0, 9.0),
    )
}

/// What stands in for "no displacement": `None` draws the layer plain (the direct path),
/// `Capture` gives it a backdrop it does not tint, so the frame takes the effect path like a
/// displaced one and only the displacement differs.
#[derive(Clone)]
enum Fx {
    None,
    Capture,
    Displace(Displace, f32),
}

fn view(fx_kind: Fx) -> El {
    let live = Element::live_text("live", &words())
        .text_style(TextStyle::new(0, 22.0, 28.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .width(W - 40.0);
    let fx = Element::layer("fx").size(W, H);
    let fx = match fx_kind {
        Fx::None => fx,
        Fx::Capture => fx.backdrop(BackdropRes::Full, Color::TRANSPARENT),
        Fx::Displace(d, strength) => fx.displace(d).displace_strength(strength),
    };
    Element::panel("root")
        .size(W, H)
        .fill(Color::hex(0x14161b, 1.0))
        .child(at(waveform(), 14.0, 10.0))
        .child(at(live, 20.0, 60.0))
        .child(at(fx, 0.0, 0.0))
}

fn scene(fonts: &FontRegistry, fx: Fx, reduced: bool) -> Scene<()> {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone()))).with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(fx), 0.0);
    scene
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList) -> Image {
    let px = off.render_to_rgba(list, [W as u32, H as u32], 1.0);
    Image { name: name.into(), w: W as u32, h: H as u32, channels: 4, px }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn rgba(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]
}

/// A 2x2 texture holding `(1, 0)` in red and green everywhere.
fn constant_offsets(gpu: &support::Gpu) -> wgpu::TextureView {
    let size = wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 };
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offsets"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let px: [u8; 16] = [255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
    gpu.queue.write_texture(
        tex.as_image_copy(),
        &px,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(2) },
        size,
    );
    tex.create_view(&Default::default())
}

fn sources() -> [(&'static str, Displace); 4] {
    [
        ("dp-noise", Displace::Noise { scale: 60.0, amount: 2.5, speed: 0.25, seed: 7 }),
        ("dp-curl", Displace::Curl { scale: 70.0, amount: 3.0, speed: 0.25, seed: 11 }),
        (
            "dp-page",
            Displace::PageCurl(PageCurl {
                edge: Point::new(W, H),
                dir: Point::new(1.0, 0.6),
                radius: 18.0,
                reach: 120.0,
            }),
        ),
        ("dp-texture", Displace::Texture { slot: 0, amount: [3.0, 0.0], bias: [0.0, 0.0] }),
    ]
}

#[test]
fn displace_goldens() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    off.renderer.set_material_image(0, Some(constant_offsets(&gpu)));
    let mut images =
        vec![render(&mut off, "dp-ripples", &scene(&fonts, Fx::Displace(ripples(), 1.0), false).sample(0.8))];
    for (name, d) in sources() {
        images.push(render(&mut off, name, &scene(&fonts, Fx::Displace(d, 1.0), false).sample(0.8)));
    }
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// The ripples move the waveform and the older line, and leave the reading line's pixels as
/// they are without displacement.
#[test]
fn reading_line_stays_as_without_displacement() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let list = scene(&fonts, Fx::Displace(ripples(), 1.0), false).sample(0.8);
    let keep = list.layers.iter().find_map(|l| l.displace.and_then(|d| d.keep)).expect("a kept reading line");
    let moved = render(&mut off, "moved", &list);
    let plain = render(&mut off, "plain", &scene(&fonts, Fx::Capture, false).sample(0.8));
    let (x0, y0) = (keep.x.ceil() as u32, keep.y.ceil() as u32);
    let (x1, y1) = ((keep.x + keep.w).floor() as u32, (keep.y + keep.h).floor() as u32);
    assert!(x1 > x0 + 50 && y1 > y0 + 10, "keep box {keep:?}");
    let mut differ = 0;
    for y in y0..y1.min(H as u32) {
        for x in x0..x1.min(W as u32) {
            if rgba(&moved, x, y) != rgba(&plain, x, y) {
                differ += 1;
            }
        }
    }
    assert_eq!(differ, 0, "reading line pixels moved, keep box {keep:?}");
    // Above the kept box the waveform and the older line did move.
    let changed = (0..y0.saturating_sub(4))
        .flat_map(|y| (0..W as u32).map(move |x| (x, y)))
        .filter(|&(x, y)| rgba(&moved, x, y) != rgba(&plain, x, y))
        .count();
    assert!(changed > 500, "only {changed} pixels moved above the reading line");
}

/// A texture holding a constant offset of (3, 0) px shows the layer moved 3 px.
#[test]
fn constant_texture_offset_shifts_three_pixels() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    off.renderer.set_material_image(0, Some(constant_offsets(&gpu)));
    let d = Displace::Texture { slot: 0, amount: [3.0, 0.0], bias: [0.0, 0.0] };
    let s = scene(&fonts, Fx::Displace(d, 1.0), false);
    // The reading line is kept still, so compare everything above it.
    let list = s.sample(0.8);
    let keep = list.layers.iter().find_map(|l| l.displace.and_then(|d| d.keep)).unwrap();
    let moved = render(&mut off, "shifted", &list);
    let plain = render(&mut off, "plain", &scene(&fonts, Fx::Capture, false).sample(0.8));
    // The final pass dithers by target pixel, so a shifted pixel meets other dither: the
    // golden tolerance (2 levels, and 0.05 % of pixels past that) applies.
    let tol = Tolerance::default();
    let (mut n, mut off_by_more) = (0usize, 0usize);
    for y in 0..(keep.y.floor() as u32 - 4) {
        for x in 0..(W as u32 - 3) {
            let (a, b) = (rgba(&moved, x, y), rgba(&plain, x + 3, y));
            n += 1;
            if a.iter().zip(b).any(|(p, q)| p.abs_diff(q) > tol.per_pixel) {
                off_by_more += 1;
            }
        }
    }
    assert!(
        (off_by_more as f64) <= tol.max_fraction * n as f64,
        "{off_by_more} of {n} pixels differ from the input shifted 3 px"
    );
}

/// Strength 0 and reduced motion emit no displacement: the same list as the scene without
/// one, and the same bytes.
#[test]
fn zero_strength_and_reduced_motion_draw_the_layer_as_captured() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let plain_list = scene(&fonts, Fx::None, false).sample(0.8);
    let plain = render(&mut off, "plain", &plain_list);
    for (what, s) in [
        ("strength 0", scene(&fonts, Fx::Displace(ripples(), 0.0), false)),
        ("reduced motion", scene(&fonts, Fx::Displace(ripples(), 1.0), true)),
    ] {
        let list = s.sample(0.8);
        assert!(list.layers.iter().all(|l| l.displace.is_none() && l.backdrop.is_none()), "{what}");
        let img = render(&mut off, "still", &list);
        assert!(img.px == plain.px, "{what}: bytes differ from the undisplaced layer");
        assert!(!off.renderer.last_frame_had_effects(), "{what}: took the effect path");
    }
}

/// Frames at equal t are byte-identical: twice in a row, and after a seek back from 1.5 s.
#[test]
fn displace_replays_and_seeks() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let mut all = vec![("ripples", ripples())];
    all.extend(sources().into_iter().take(2).map(|(n, d)| (n, d)));
    for (name, d) in all {
        let s = scene(&fonts, Fx::Displace(d, 1.0), false);
        let a = render(&mut off, "a", &s.sample(0.8));
        let b = render(&mut off, "b", &s.sample(0.8));
        render(&mut off, "later", &s.sample(1.5));
        let c = render(&mut off, "c", &s.sample(0.8));
        assert!(a.px == b.px, "{name}: two renders at 0.8 s differ");
        assert!(a.px == c.px, "{name}: 0.8 s after a seek back from 1.5 s differs");
    }
}

/// The scene stays awake while a ring lives, wakes for the next onset, and settles after.
#[test]
fn ripples_wake_the_scene() {
    let fonts = fonts();
    let s = scene(&fonts, Fx::Displace(ripples(), 1.0), false);
    assert_eq!(s.next_wake(0.8), Some(0.8));
    assert_eq!(s.next_wake(3.0), None);
}
