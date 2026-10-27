//! GPU elements and painters: the renderer breaks the pass at a `GpuItem`, a painter draws in
//! its own pass, and the content after the item draws on top. Covered on the direct path, the
//! effect path (a bloom elsewhere in the frame), inside an offscreen layer at half opacity, and on a plain UNORM
//! target, where the painter encodes sRGB itself.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use std::any::Any;
use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::{GpuPainter, PaintCx, Renderer, Target};
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::Scene;

/// Puts the row in a layer, or leaves it alone.
type Wrap = fn(Element<()>) -> Element<()>;

const W: u32 = 240;
const H: u32 = 140;

/// A flat colour, linear and premultiplied.
#[derive(Debug)]
struct Swatch([f32; 4]);

impl GpuSource for Swatch {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn until(&self) -> f64 {
        0.0
    }
}

const SHADER: &str = r#"
struct U { color: vec4f, encode: vec4f }
@group(0) @binding(0) var<uniform> u: U;

@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

fn encode(c: vec3f) -> vec3f {
    return select(1.055 * pow(c, vec3f(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3f(0.0031308));
}

@fragment fn fs() -> @location(0) vec4f {
    var c = u.color;
    if u.encode.x > 0.5 && c.a > 0.0 {
        c = vec4f(encode(c.rgb / c.a) * c.a, c.a);
    }
    return c;
}
"#;

/// Fills the item's box with its swatch, scissored to the box and the layer's clip.
struct SwatchPainter {
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    painted: u32,
}

impl GpuPainter for SwatchPainter {
    fn begin_frame(&mut self) {
        self.painted = 0;
    }

    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(swatch) = cx.source.as_any().downcast_ref::<Swatch>() else { return false };
        if !self.pipelines.iter().any(|(f, _)| *f == cx.format) {
            let module = cx.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("swatch"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
            let pipeline = cx.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("swatch"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: cx.format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            self.pipelines.push((cx.format, pipeline));
        }
        let pipeline = &self.pipelines.iter().find(|(f, _)| *f == cx.format).unwrap().1;
        let [r, g, b, a] = swatch.0.map(|v| v * cx.opacity);
        let encode = if cx.encode_srgb { 1.0f32 } else { 0.0 };
        let data: [f32; 8] = [r, g, b, a, encode, 0.0, 0.0, 0.0];
        // A buffer per item: queue writes land before the whole command buffer.
        let buf = cx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("swatch"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        cx.queue.write_buffer(&buf, 0, bytemuck::cast_slice(&data));
        let bind = cx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("swatch"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
        });
        let [x0, y0, x1, y1] = cx.rect_px();
        let [sx, sy, sw, sh] = cx.scissor;
        let x0 = (x0.round().max(0.0) as u32).max(sx);
        let y0 = (y0.round().max(0.0) as u32).max(sy);
        let x1 = (x1.round().max(0.0) as u32).min(sx + sw);
        let y1 = (y1.round().max(0.0) as u32).min(sy + sh);
        if x1 <= x0 || y1 <= y0 {
            return true;
        }
        let mut pass = cx.enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("swatch"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: cx.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
        pass.draw(0..3, 0..1);
        self.painted += 1;
        true
    }
}

/// A backdrop, a swatch box at the centre, and a pill drawn after it that overlaps its right
/// half. `wrap` puts the box and the pill in a layer.
fn frame(swatch: &Arc<Swatch>, wrap: impl Fn(Element<()>) -> Element<()>) -> DisplayList {
    let source: Arc<dyn GpuSource> = swatch.clone();
    let row = Element::row("row")
        .align_items(vo_scene::taffy::AlignItems::CENTER)
        .child(Element::gpu("swatch", source).size(100.0, 60.0))
        .child(
            Element::shape("pill", ShapeKind::Capsule)
                .size(80.0, 30.0)
                .offset(-40.0, 0.0)
                .fill(Color::hex(0xffd27a, 1.0)),
        );
    let root = Element::panel("root")
        .size(W as f32, H as f32)
        .fill_gradient(Color::hex(0x203a5c, 1.0), Color::hex(0x5c2038, 1.0))
        .align_items(vo_scene::taffy::AlignItems::CENTER)
        .justify(vo_scene::taffy::JustifyContent::CENTER)
        .child(wrap(row));
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    scene.apply(root, 0.0);
    assert_eq!(scene.next_wake(1.0), None, "a swatch until 0 lets the scene settle");
    scene.sample(10.0)
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

fn px(img: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

#[test]
fn painter_draws_between_passes() {
    let gpu = support::gpu();
    let mut r = Renderer::new(&gpu.device, &gpu.queue);
    r.add_painter(Box::new(SwatchPainter { pipelines: Vec::new(), painted: 0 }));
    // Linear 0.2, 0.6, 0.4: sRGB about 124, 203, 170.
    let swatch = Arc::new(Swatch([0.2, 0.6, 0.4, 1.0]));
    let srgb = wgpu::TextureFormat::Rgba8UnormSrgb;
    let unorm = wgpu::TextureFormat::Rgba8Unorm;
    let cases: [(&str, wgpu::TextureFormat, Wrap); 4] = [
        ("gpu-direct", srgb, |e| e),
        // A small bloom layer below the row puts the frame on the effect path.
        ("gpu-bloom", srgb, |e| {
            let dot = Element::shape("dot", ShapeKind::Circle).size(8.0, 8.0).fill(Color::hex(0x9ad0ff, 1.0));
            Element::layer("w")
                .align_items(vo_scene::taffy::AlignItems::CENTER)
                .child(e)
                .child(Element::layer("glow").bloom(1.0, 6.0).child(dot))
        }),
        ("gpu-layer", srgb, |e| Element::layer("half").offscreen().opacity(0.5).child(e)),
        ("gpu-unorm", unorm, |e| e),
    ];
    let mut errors = Vec::new();
    let mut direct = Vec::new();
    for (name, format, wrap) in cases {
        let mut target = Target::new(&gpu.device, W, H, format);
        r.render_at(&frame(&swatch, wrap), 1.0, &mut target);
        let img = read(&gpu, &target);
        // The box spans x 50..150 before the pill moves; the pill covers about 110..190.
        let left = px(&img, 70, H / 2);
        let under_pill = px(&img, 140, H / 2);
        match name {
            "gpu-layer" => {
                // Half the swatch over the backdrop: green above red and blue, not full.
                assert!(left[1] > left[0] && left[1] < 203, "{name}: half swatch {left:?}");
            }
            _ => {
                for (got, want) in left.iter().zip([124u8, 203, 170, 255]) {
                    assert!(got.abs_diff(want) <= 2, "{name}: swatch {left:?}");
                }
            }
        }
        // The pill comes after the item, so it is on top: much redder than the swatch. At
        // half opacity it is half pill, half backdrop.
        let red = if name == "gpu-layer" { 170 } else { 200 };
        assert!(under_pill[0] > red && under_pill[2] < 160, "{name}: pill not on top {under_pill:?}");
        if name == "gpu-direct" {
            direct = img.clone();
        }
        if name == "gpu-unorm" {
            let off = img.iter().zip(&direct).filter(|(a, b)| a.abs_diff(**b) > 2).count();
            assert!(off < img.len() / 200, "{name}: {off} channels differ from the sRGB target");
        }
        let img = Image { name: name.into(), w: W, h: H, channels: 4, px: img };
        if let Err(e) = support::check(&img, Tolerance::default()) {
            errors.push(e);
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// No painter for the source: the item is skipped and the rest of the frame still draws.
#[test]
fn unknown_source_is_skipped() {
    let gpu = support::gpu();
    let mut r = Renderer::new(&gpu.device, &gpu.queue);
    let swatch = Arc::new(Swatch([0.2, 0.6, 0.4, 1.0]));
    let mut target = Target::new(&gpu.device, W, H, wgpu::TextureFormat::Rgba8UnormSrgb);
    r.render_at(&frame(&swatch, |e| e), 1.0, &mut target);
    let img = read(&gpu, &target);
    let left = px(&img, 70, H / 2);
    assert!(left[1] < 150, "nothing paints the swatch: {left:?}");
    let pill = px(&img, 140, H / 2);
    assert!(pill[0] > 200, "the pill still draws: {pill:?}");
}
