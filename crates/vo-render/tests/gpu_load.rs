//! `Renderer::encode_with` and `LoadOp::Load`: a second list drawn over what the target
//! already holds, on the direct path and on the effect path (bloom), where the last pass
//! blends the scene over the target instead of replacing it.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

const W: u32 = 240;
const H: u32 = 140;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn settled(view: Element<()>) -> DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    scene.apply(view, 0.0);
    scene.sample(10.0)
}

/// The layers below: a gradient over the whole target.
fn backdrop() -> DisplayList {
    settled(
        Element::panel("root")
            .size(W as f32, H as f32)
            .fill_gradient(Color::hex(0x203a5c, 1.0), Color::hex(0x5c2038, 1.0)),
    )
}

/// A card on top, at the centre, with or without bloom.
fn card(bloom: bool) -> DisplayList {
    let pill = Element::shape("pill", ShapeKind::Capsule).size(120.0, 36.0).fill(Color::hex(0xffd27a, 0.9));
    let mut layer = Element::layer("glow").child(pill);
    if bloom {
        layer = layer.bloom(1.2, 18.0);
    }
    settled(
        Element::panel("root")
            .size(W as f32, H as f32)
            .align_items(vo_scene::taffy::AlignItems::CENTER)
            .justify(vo_scene::taffy::JustifyContent::CENTER)
            .child(layer),
    )
}

fn read(gpu: &support::Gpu, target: &Target) -> Vec<u8> {
    let (w, h) = (target.width(), target.height());
    let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        target.texture().as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buf.slice(..).get_mapped_range().expect("map");
    (0..h as usize).flat_map(|y| data[y * row as usize..][..(w * 4) as usize].to_vec()).collect()
}

#[test]
fn second_list_draws_over_the_first() {
    let gpu = support::gpu();
    let mut r = Renderer::new(&gpu.device, &gpu.queue);
    let mut target = Target::new(&gpu.device, W, H, FORMAT);
    let under = {
        r.render_at(&backdrop(), 1.0, &mut target);
        read(&gpu, &target)
    };
    let mut errors = Vec::new();
    for (name, bloom) in [("load-direct", false), ("load-bloom", true)] {
        r.render_at(&backdrop(), 1.0, &mut target);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        r.encode_with(&card(bloom), 1.0, &target, wgpu::LoadOp::Load, &mut enc);
        gpu.queue.submit([enc.finish()]);
        assert_eq!(r.last_frame_had_effects(), bloom, "{name}: wrong path");
        let px = read(&gpu, &target);
        // A corner far from the card and its bloom keeps the backdrop exactly.
        for (x, y) in [(2, 2), (W - 3, H - 3)] {
            let i = ((y * W + x) * 4) as usize;
            assert_eq!(px[i..i + 4], under[i..i + 4], "{name}: backdrop changed at ({x}, {y})");
        }
        // The card covers the centre.
        let c = ((H / 2 * W + W / 2) * 4) as usize;
        assert_ne!(px[c..c + 4], under[c..c + 4], "{name}: the card is missing");
        let img = Image { name: name.into(), w: W, h: H, channels: 4, px };
        if let Err(e) = support::check(&img, Tolerance::default()) {
            errors.push(e);
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
