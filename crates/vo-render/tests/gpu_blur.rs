//! Layer blur and frost (EG-04).
//!
//! The impulse test blurs one white texel with the renderer's two passes and reads the
//! floats back. The goldens are a depth stack of three frosted sheets (the newest frosts the
//! two older lines behind it) at scale 1 and 1.5, and a blurred layer of a waveform and live
//! text whose newest line stays sharp.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::blur::gaussian_blur;
use vo_render::Offscreen;
use vo_scene::depth_stack::{DepthStack, Place, Sheet};
use vo_scene::display::{BackdropRes, DisplayList};
use vo_scene::element::{Element, Key, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::taffy;
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

type El = Element<()>;

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn scene_of(fonts: &FontRegistry, size: Size, view: El, reduced: bool) -> Scene<()> {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone()))).with_reduced_motion(reduced);
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view, 0.0);
    scene
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, size: Size, scale: f32) -> Image {
    let (w, h) = ((size.w * scale).round() as u32, (size.h * scale).round() as u32);
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

fn rgba(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]
}

// The impulse.

fn f16_to_f32(h: u16) -> f32 {
    let (s, e, m) = ((h >> 15) as u32, ((h >> 10) & 0x1f) as i32, (h & 0x3ff) as f32);
    let v = match e {
        0 => m * 2f32.powi(-24),
        31 => f32::INFINITY,
        e => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    };
    if s == 1 {
        -v
    } else {
        v
    }
}

/// One white texel in the middle of a 33 by 33 `Rgba16Float` texture, blurred at radius 4
/// (standard deviation 4 px, reach 12 px): the result is symmetric across both axes and the
/// diagonal, and each channel sums to the input within 1/255.
#[test]
fn impulse_is_symmetric_and_keeps_its_sum() {
    const N: u32 = 33;
    let gpu = support::gpu();
    let (d, q) = (&gpu.device, &gpu.queue);
    let size = wgpu::Extent3d { width: N, height: N, depth_or_array_layers: 1 };
    let src = d.create_texture(&wgpu::TextureDescriptor {
        label: Some("impulse"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut px = vec![0u16; (N * N * 4) as usize];
    let c = ((N / 2) * N + N / 2) as usize * 4;
    px[c..c + 4].copy_from_slice(&[0x3c00; 4]);
    q.write_texture(
        src.as_image_copy(),
        bytemuck::cast_slice(&px),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(N * 8), rows_per_image: Some(N) },
        size,
    );
    let out = gaussian_blur(d, q, &src, 4.0);
    let row = (N * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buf = d.create_buffer(&wgpu::BufferDescriptor {
        label: Some("impulse-readback"),
        size: (row * N) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = d.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        out.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        size,
    );
    q.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range().expect("mapped").to_vec();
    let at = |x: u32, y: u32, ch: u32| {
        let i = (y * row + x * 8 + ch * 2) as usize;
        f16_to_f32(u16::from_le_bytes([data[i], data[i + 1]]))
    };
    let tol = 1.0 / 255.0;
    for ch in 0..4 {
        let sum: f32 = (0..N).flat_map(|y| (0..N).map(move |x| (x, y))).map(|(x, y)| at(x, y, ch)).sum();
        assert!((sum - 1.0).abs() <= tol, "channel {ch} sums to {sum}");
        for y in 0..N {
            for x in 0..N {
                let v = at(x, y, ch);
                for (what, w) in [
                    ("left-right", at(N - 1 - x, y, ch)),
                    ("up-down", at(x, N - 1 - y, ch)),
                    ("diagonal", at(y, x, ch)),
                ] {
                    assert!((v - w).abs() <= tol, "{what} at {x},{y} ch {ch}: {v} against {w}");
                }
            }
        }
        let peak = at(N / 2, N / 2, ch);
        // A 2D Gaussian of sigma 4 holds 1 / (2 pi 16) at its centre.
        assert!((peak - 1.0 / (32.0 * std::f32::consts::PI)).abs() < 2e-3, "peak {peak}");
        assert!(at(N / 2 + 13, N / 2, ch) == 0.0, "the blur reaches past 12 px");
    }
}

// The depth stack.

const STACK_SIZE: Size = Size { w: 420.0, h: 200.0 };
const LINES: [&str; 3] = ["the oldest line goes back", "an older line behind glass", "the newest line reads"];

fn stack(fonts: &FontRegistry, frost: f32, reduced: bool) -> Scene<()> {
    let s = DepthStack {
        sheet: Some(Sheet {
            fill: Color::hex(0x16222c, 0.35),
            edge: Color::hex(0x9fd8ff, 0.35),
            edge_width: 1.0,
            radius: 6.0,
            pad: (14.0, 6.0),
        }),
        step_z: 70.0,
        step_rise: 18.0,
        frost,
        ..DepthStack::default()
    };
    let lines: Vec<(Key, &str)> = LINES.iter().map(|l| (Key::from(l.to_string()), *l)).collect();
    let content = |text: &&str, d: usize, _: &Place| {
        Element::label(format!("t{d}"), text.to_string())
            .text_style(TextStyle::new(0, 20.0, 26.0))
            .color(Color::hex(0xf2f3f5, 1.0))
    };
    let view = Element::panel("root").size(STACK_SIZE.w, STACK_SIZE.h).fill(Color::hex(0x0b0f14, 1.0)).child(s.view(
        "stack",
        STACK_SIZE,
        Size::new(320.0, 40.0),
        &lines,
        content,
        reduced,
    ));
    // Lines arrive one at a time; the stack settles by 3 s.
    let scene = scene_of(fonts, STACK_SIZE, view, reduced);
    scene.sample(3.0);
    scene
}

// The blurred layer.

const LAYER_SIZE: Size = Size { w: 360.0, h: 150.0 };

fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

fn words() -> Vec<Word> {
    ["blur", "softens", "the", "older", "line", "while", "the", "newest", "holds"]
        .iter()
        .enumerate()
        .map(|(i, w)| Word { id: i as u32 + 1, text: (*w).into() })
        .collect()
}

fn waveform() -> El {
    Element::row("wave").gap(3.0).align_items(taffy::AlignItems::CENTER).height(40.0).children((0..48u32).map(|i| {
        let x = i as f32;
        let h = 6.0 + 30.0 * (0.5 + 0.5 * (x * 0.37).sin() * (x * 0.11).cos()).abs();
        Element::panel(i as u64).size(4.0, h).radius(2.0).fill(Color::hex(0x6cb6ff, 1.0))
    }))
}

/// How the layer is drawn: blurred, or offscreen and sharp on the effect path (a backdrop
/// that only captures puts the frame there), so only the blur differs.
#[derive(Clone, Copy)]
enum Fx {
    Blur(f32),
    Sharp,
}

fn layer_view(fx: Fx) -> El {
    let live = Element::live_text("live", &words())
        .text_style(TextStyle::new(0, 22.0, 28.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .width(LAYER_SIZE.w - 40.0);
    let layer = Element::layer("fx").size(LAYER_SIZE.w, LAYER_SIZE.h);
    let layer = match fx {
        Fx::Blur(r) => layer.layer_blur(r),
        Fx::Sharp => layer.offscreen(),
    };
    let capture = Element::layer("capture").size(10.0, 10.0).backdrop(BackdropRes::Full, Color::TRANSPARENT);
    Element::panel("root")
        .size(LAYER_SIZE.w, LAYER_SIZE.h)
        .fill(Color::hex(0x14161b, 1.0))
        .child(at(capture, 0.0, 0.0))
        .child(at(layer.child(at(waveform(), 14.0, 10.0)).child(at(live, 20.0, 60.0)), 0.0, 0.0))
}

#[test]
fn blur_goldens() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let list = stack(&fonts, 3.0, false).sample(3.0);
    assert!(list.layers.iter().filter(|l| l.frost > 0.0).count() == 3, "three frosted sheets");
    let layer = scene_of(&fonts, LAYER_SIZE, layer_view(Fx::Blur(2.5)), false).sample(0.8);
    let images = [
        render(&mut off, "blur-stack", &list, STACK_SIZE, 1.0),
        render(&mut off, "blur-stack-x1.5", &list, STACK_SIZE, 1.5),
        render(&mut off, "blur-layer", &layer, LAYER_SIZE, 1.0),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// Frost changes the pixels behind the newest sheet: the frosted stack differs from the
/// same stack with a backdrop that only captures.
#[test]
fn frost_blurs_the_lines_behind() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let frosted = render(&mut off, "frosted", &stack(&fonts, 3.0, false).sample(3.0), STACK_SIZE, 1.0);
    let plain = render(&mut off, "plain", &stack(&fonts, 0.0, false).sample(3.0), STACK_SIZE, 1.0);
    let differ = frosted.px.chunks_exact(4).zip(plain.px.chunks_exact(4)).filter(|(a, b)| a != b).count();
    assert!(differ > 1000, "only {differ} pixels changed under the frost");
}

/// The blur softens the waveform and the older line, and leaves the reading line's pixels as
/// they are on the same layer drawn sharp.
#[test]
fn reading_line_stays_sharp() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let list = scene_of(&fonts, LAYER_SIZE, layer_view(Fx::Blur(2.5)), false).sample(0.8);
    let keep = list.layers.iter().find_map(|l| l.blur.and_then(|b| b.keep)).expect("a kept reading line");
    let blurred = render(&mut off, "blurred", &list, LAYER_SIZE, 1.0);
    let sharp_list = scene_of(&fonts, LAYER_SIZE, layer_view(Fx::Sharp), false).sample(0.8);
    let sharp = render(&mut off, "sharp", &sharp_list, LAYER_SIZE, 1.0);
    let (x0, y0) = (keep.x.ceil().max(0.0) as u32, keep.y.ceil().max(0.0) as u32);
    let (x1, y1) = ((keep.x + keep.w).floor() as u32, (keep.y + keep.h).floor() as u32);
    assert!(x1 > x0 + 50 && y1 > y0 + 10, "keep box {keep:?}");
    let mut differ = 0;
    for y in y0..y1.min(LAYER_SIZE.h as u32) {
        for x in x0..x1.min(LAYER_SIZE.w as u32) {
            if rgba(&blurred, x, y) != rgba(&sharp, x, y) {
                differ += 1;
            }
        }
    }
    assert_eq!(differ, 0, "reading line pixels blurred, keep box {keep:?}");
    let changed = (0..y0.saturating_sub(4))
        .flat_map(|y| (0..LAYER_SIZE.w as u32).map(move |x| (x, y)))
        .filter(|&(x, y)| rgba(&blurred, x, y) != rgba(&sharp, x, y))
        .count();
    assert!(changed > 500, "only {changed} pixels blurred above the reading line");
}

/// Radius 0 emits no blur and no frost: the same list as without them, the same bytes, and
/// no effect path.
#[test]
fn radius_zero_is_the_plain_layer() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let plain_view = |r: Option<f32>| {
        let layer = Element::layer("fx").size(LAYER_SIZE.w, LAYER_SIZE.h);
        let layer = match r {
            Some(r) => layer.layer_blur(r).frost(r),
            None => layer,
        };
        Element::panel("root").size(LAYER_SIZE.w, LAYER_SIZE.h).fill(Color::hex(0x14161b, 1.0)).child(at(
            layer.child(at(waveform(), 14.0, 10.0)),
            0.0,
            0.0,
        ))
    };
    let plain_list = scene_of(&fonts, LAYER_SIZE, plain_view(None), false).sample(0.8);
    let plain = render(&mut off, "plain", &plain_list, LAYER_SIZE, 1.0);
    let list = scene_of(&fonts, LAYER_SIZE, plain_view(Some(0.0)), false).sample(0.8);
    assert!(list.layers.iter().all(|l| l.blur.is_none() && l.frost == 0.0 && l.backdrop.is_none()));
    let img = render(&mut off, "zero", &list, LAYER_SIZE, 1.0);
    assert!(img.px == plain.px, "radius 0 bytes differ from the plain layer");
    assert!(!off.renderer.last_frame_had_effects(), "radius 0 took the effect path");
}

/// Frames at equal t are byte-identical, for the frosted stack and the blurred layer.
#[test]
fn blur_replays() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let s = stack(&fonts, 3.0, false);
    let l = scene_of(&fonts, LAYER_SIZE, layer_view(Fx::Blur(2.5)), false);
    for (name, list, size) in [("stack", s.sample(3.0), STACK_SIZE), ("layer", l.sample(0.8), LAYER_SIZE)] {
        let a = render(&mut off, "a", &list, size, 1.0);
        let b = render(&mut off, "b", &list, size, 1.0);
        assert!(a.px == b.px, "{name}: two renders at the same t differ");
    }
}

/// Under reduced motion a blur radius jumps to its target; with motion it springs there.
#[test]
fn reduced_motion_radius_jumps() {
    let fonts = fonts();
    let radius = |reduced: bool| {
        let mut s = scene_of(&fonts, LAYER_SIZE, layer_view(Fx::Blur(1.0)), reduced);
        s.sample(1.0);
        s.apply(layer_view(Fx::Blur(6.0)), 1.0);
        s.sample(1.05).layers.iter().find_map(|l| l.blur).map(|b| b.radius).unwrap()
    };
    assert_eq!(radius(true), 6.0);
    let moving = radius(false);
    assert!(moving > 1.0 && moving < 6.0, "{moving}");
}
