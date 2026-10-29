//! A 3D ring split at the text plane (EG-20, `Element::gpu_split`): the orbit words demo's
//! ring round three lines of live text, at scale 1, at scale 1.5 and in the light look.
//!
//! (a) Where the front half crosses an older line, the text's pixels differ from the render
//!     without the ring.
//! (b) The back half stays under the text: text pixels with coverage 0.9 or more are as in
//!     the render without the ring (exactly at full coverage, and otherwise within the
//!     1 - coverage of linear light the text lets through).
//! (c) Inside the newest line's padded boxes every pixel equals the render without the front
//!     half, exactly, and the boxes do cut something.
//!
//! Then the same t draws the same bytes. GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

#[path = "../examples/orbit_words_demo.rs"]
#[allow(dead_code)]
mod demo;

use std::sync::Arc;

use demo::{Mode, Ring};
use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::Offscreen;
use vo_scene::display::DisplayList;
use vo_scene::gpu::{GpuPart, GpuSource};

/// All fifteen words are in.
const T: f64 = 4.5;

fn render(off: &mut Offscreen, list: &DisplayList, scale: f32) -> Vec<u8> {
    let size = [(demo::W * scale).round() as u32, (demo::H * scale).round() as u32];
    off.render_to_rgba(list, size, scale)
}

/// An sRGB byte as linear light.
fn linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn front(list: &mut DisplayList) -> &mut vo_scene::gpu::GpuItem {
    list.gpu.iter_mut().find(|g| g.part == GpuPart::Front).expect("a front half")
}

#[test]
fn split_ring_goes_behind_and_in_front_but_never_over_the_reading_line() {
    let gpu = support::gpu();
    let fonts = demo::fonts();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    install(&mut off.renderer);
    let mut errors = Vec::new();
    for (name, scale, light) in
        [("3d-split", 1.0f32, false), ("3d-split-1.5x", 1.5, false), ("3d-split-light", 1.0, true)]
    {
        let ring: Arc<dyn GpuSource> = item(Ring { still: false, light });
        let (w, h) = ((demo::W * scale).round() as u32, (demo::H * scale).round() as u32);
        let split_list = demo::sample(&fonts, &ring, Mode::Split, light, true, T);
        let split = render(&mut off, &split_list, scale);
        let plain = render(&mut off, &demo::sample(&fonts, &ring, Mode::None, light, true, T), scale);
        let mut back_list = split_list.clone();
        front(&mut back_list).opacity = 0.0;
        let back = render(&mut off, &back_list, scale);
        let mut uncut_list = split_list.clone();
        front(&mut uncut_list).cut.clear();
        let uncut = render(&mut off, &uncut_list, scale);
        // Text coverage: the text alone on a clear surface.
        let cover = render(&mut off, &demo::sample(&fonts, &ring, Mode::None, light, false, T), scale);

        let boxes: Vec<[f32; 4]> = front(&mut split_list.clone())
            .cut
            .iter()
            .map(|b| [b.x * scale, b.y * scale, b.right() * scale, b.bottom() * scale])
            .collect();
        assert!(!boxes.is_empty(), "{name}: no reading line boxes");
        let reading_top = boxes.iter().map(|b| b[1]).fold(f32::INFINITY, f32::min);
        let in_box = |x: u32, y: u32| {
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            boxes.iter().any(|b| cx >= b[0] && cy >= b[1] && cx < b[2] && cy < b[3])
        };
        let px = |img: &[u8], x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [img[i], img[i + 1], img[i + 2], img[i + 3]]
        };

        let (mut front_over_old, mut text_px, mut boxed, mut cut_something) = (0, 0, 0, 0);
        for y in 0..h {
            for x in 0..w {
                let a = px(&cover, x, y)[3];
                let (s, p, b) = (px(&split, x, y), px(&plain, x, y), px(&back, x, y));
                // (a) The front half over an older line's full-coverage text.
                if a == 255 && (y as f32) < reading_top && s != p {
                    front_over_old += 1;
                }
                // (b) The back half under text: what shows through is the 1 - coverage left,
                // in linear light (blending is linear; the bytes are sRGB).
                if a >= 230 {
                    text_px += 1;
                    let allowed = (255 - a) as f32 / 255.0 + 0.01;
                    let d = b.iter().zip(p).take(3).map(|(u, v)| (linear(*u) - linear(v)).abs()).fold(0.0, f32::max);
                    assert!(
                        if a == 255 { b == p } else { d <= allowed },
                        "{name}: back half shows through text at ({x}, {y}), coverage {a}: {b:?} vs {p:?}"
                    );
                }
                // (c) The reading line is as without the front half.
                if in_box(x, y) {
                    boxed += 1;
                    assert_eq!(s, b, "{name}: front half drawn at ({x}, {y}) on the reading line");
                    if px(&uncut, x, y) != b {
                        cut_something += 1;
                    }
                }
            }
        }
        assert!(text_px > 2000, "{name}: only {text_px} text pixels");
        assert!(front_over_old > 20, "{name}: the front half crosses older lines on only {front_over_old} pixels");
        assert!(boxed > 1000, "{name}: only {boxed} pixels in the reading line boxes");
        assert!(
            cut_something > 20,
            "{name}: the cut keeps only {cut_something} pixels of the front half off the reading line"
        );
        // Drawn whole, the ring is under all the text: the split draws something else.
        assert_ne!(split, render(&mut off, &demo::sample(&fonts, &ring, Mode::Whole, light, true, T), scale), "{name}");

        // Same t, same bytes: again, and from a fresh scene.
        assert!(render(&mut off, &split_list, scale) == split, "{name}: a second draw differs");
        let again = demo::sample(&fonts, &ring, Mode::Split, light, true, T);
        assert!(render(&mut off, &again, scale) == split, "{name}: a fresh scene differs");

        let img = Image { name: name.into(), w, h, channels: 4, px: split };
        if let Err(e) = support::check(&img, Tolerance { per_pixel: 3, max_fraction: 0.002 }) {
            errors.push(e);
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
