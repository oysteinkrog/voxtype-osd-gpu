//! The built-in painter for images (EG-24, `vo_scene::image`).
//!
//! Every [`Renderer`](crate::Renderer) has one, so `Element::image` draws with no app code.
//! It is a GPU painter (`crate::gpu`): the renderer breaks its pass at the image and the
//! painter draws one textured quad in a pass of its own.
//!
//! - The raster comes from `vo_scene::image::ImageCache::global()`, at the box's size in
//!   surface pixels, decoded on first use. A source that fails to decode is logged once and
//!   then skipped.
//! - Each raster is uploaded once as an `Rgba16Float` texture of linear, premultiplied colour,
//!   so filtering and blending are in linear light. Textures are kept by (source, pixel size),
//!   least recently used out first, at most [`TEXTURE_BYTES`].
//! - The quad goes through the layer's transform and its stage warp, perspective correct.
//! - Output follows the painter contract: linear on the effect path and on sRGB targets,
//!   encoded by the shader on a plain UNORM target. Premultiplied over.
//!
//! Like every GPU element, an image gets the layer clip only as a scissor, and no motion
//! blur. The CPU first frame (`crate::cpu`) draws it when the raster is already decoded.

use std::sync::Arc;

use vo_scene::geom::Point;
use vo_scene::image::{fit, ImageCache, ImageSource, Raster};

use crate::gpu::{GpuPainter, PaintCx};

/// The most texture bytes the painter keeps: 8 MB.
pub const TEXTURE_BYTES: usize = 8 << 20;

const SHADER: &str = r#"
struct U { corners: array<vec4f, 4>, opacity: f32, encode: f32, pad0: f32, pad1: f32 }
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct V { @builtin(position) pos: vec4f, @location(0) uv: vec2f }

// Corners in order top left, top right, bottom left, bottom right, as a strip.
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    var v: V;
    v.pos = u.corners[i];
    v.uv = vec2f(f32(i & 1u), f32(i >> 1u));
    return v;
}

fn encode(c: vec3f) -> vec3f {
    return select(1.055 * pow(c, vec3f(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3f(0.0031308));
}

@fragment fn fs(v: V) -> @location(0) vec4f {
    // Soften edges that do not fall on pixel boundaries (a turned layer): full coverage from
    // half a pixel inside the edge.
    let edge = min(v.uv, 1.0 - v.uv) / max(fwidth(v.uv), vec2f(1e-6));
    let cov = clamp(min(edge.x, edge.y) + 0.5, 0.0, 1.0);
    var c = textureSample(tex, samp, v.uv) * (u.opacity * cov);
    if u.encode > 0.5 && c.a > 0.0 {
        c = vec4f(encode(c.rgb / c.a) * c.a, c.a);
    }
    return c;
}
"#;

struct Tex {
    key: (u64, [u32; 2]),
    view: wgpu::TextureView,
    bytes: usize,
    last: u64,
}

/// Draws `vo_scene::image::ImageSource` items. See the module docs.
pub struct ImagePainter {
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    sampler: Option<wgpu::Sampler>,
    textures: Vec<Tex>,
    bytes: usize,
    frame: u64,
    /// Sources whose failure is already logged.
    logged: Vec<u64>,
}

impl Default for ImagePainter {
    fn default() -> Self {
        ImagePainter::new()
    }
}

impl ImagePainter {
    pub fn new() -> ImagePainter {
        ImagePainter { pipelines: Vec::new(), sampler: None, textures: Vec::new(), bytes: 0, frame: 0, logged: Vec::new() }
    }

    /// Texture bytes held now.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        if let Some(i) = self.pipelines.iter().position(|(f, _)| *f == format) {
            return &self.pipelines[i].1;
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-image"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vo-image"),
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
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.push((format, pipeline));
        &self.pipelines.last().expect("pushed").1
    }

    /// The texture for `raster` under `key`, uploaded now if new.
    fn texture(&mut self, cx: &PaintCx<'_>, key: (u64, [u32; 2]), raster: &Raster) -> wgpu::TextureView {
        let frame = self.frame;
        if let Some(t) = self.textures.iter_mut().find(|t| t.key == key) {
            t.last = frame;
            return t.view.clone();
        }
        let size = wgpu::Extent3d { width: raster.w, height: raster.h, depth_or_array_layers: 1 };
        let tex = cx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vo-image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let texels = linear_premul_f16(raster);
        cx.queue.write_texture(
            tex.as_image_copy(),
            bytemuck::cast_slice(&texels),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(raster.w * 8), rows_per_image: Some(raster.h) },
            size,
        );
        let view = tex.create_view(&Default::default());
        let bytes = (raster.w * raster.h * 8) as usize;
        // Make room, never dropping one drawn this frame (its view is still in use).
        while self.bytes + bytes > TEXTURE_BYTES {
            let Some(i) = self
                .textures
                .iter()
                .enumerate()
                .filter(|(_, t)| t.last != frame)
                .min_by_key(|(_, t)| t.last)
                .map(|(i, _)| i)
            else {
                break;
            };
            self.bytes -= self.textures.swap_remove(i).bytes;
        }
        self.bytes += bytes;
        self.textures.push(Tex { key, view: view.clone(), bytes, last: frame });
        view
    }
}

/// A raster as linear, premultiplied half floats.
fn linear_premul_f16(r: &Raster) -> Vec<u16> {
    let lin: [f32; 256] = std::array::from_fn(|i| vo_scene::image::srgb_decode(i as f32 / 255.0));
    r.px.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = p[3] as f32 / 255.0;
            [lin[p[0] as usize] * a, lin[p[1] as usize] * a, lin[p[2] as usize] * a, a]
        })
        .map(|v| half::f16::from_f32(v).to_bits())
        .collect()
}

impl GpuPainter for ImagePainter {
    fn begin_frame(&mut self) {
        self.frame += 1;
    }

    fn paint(&mut self, cx: &mut PaintCx<'_>) -> bool {
        let Some(src) = cx.source.as_any().downcast_ref::<ImageSource>() else { return false };
        let m = cx.to_px;
        let ppu = (m.a * m.d - m.b * m.c).abs().sqrt();
        let want = [(cx.rect.w * ppu).round() as u32, (cx.rect.h * ppu).round() as u32];
        if want[0] == 0 || want[1] == 0 || cx.opacity <= 0.0 {
            return true;
        }
        let raster: Arc<Raster> = {
            let mut cache = ImageCache::global().lock().unwrap_or_else(|e| e.into_inner());
            match cache.raster(src, want) {
                Ok(r) => r,
                Err(e) => {
                    if !self.logged.contains(&src.id()) {
                        self.logged.push(src.id());
                        eprintln!("vo-render: image {:?} not drawn: {e}", src.data());
                    }
                    return true;
                }
            }
        };
        let view = self.texture(cx, (src.id(), want), &raster);
        // The quad: the fitted box, through the layer and the stage warp, in clip space.
        let r = fit(cx.rect, raster.w, raster.h);
        let [sw, sh] = cx.size.map(|v| v as f32);
        let corners = [(r.x, r.y), (r.x + r.w, r.y), (r.x, r.y + r.h), (r.x + r.w, r.y + r.h)].map(|(x, y)| {
            let p = m.apply(Point::new(x, y));
            let [x, y, w] = match cx.warp {
                Some(warp) => warp.homogeneous(p),
                None => [p.x, p.y, 1.0],
            };
            [2.0 * x / sw - w, w - 2.0 * y / sh, 0.0, w]
        });
        let mut data = [0.0f32; 20];
        for (i, c) in corners.iter().enumerate() {
            data[4 * i..4 * i + 4].copy_from_slice(c);
        }
        data[16] = cx.opacity.min(1.0);
        data[17] = if cx.encode_srgb { 1.0 } else { 0.0 };
        // A buffer per item: queue writes land before the whole command buffer.
        let buf = cx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-image"),
            size: std::mem::size_of_val(&data) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        cx.queue.write_buffer(&buf, 0, bytemuck::cast_slice(&data));
        let sampler = self
            .sampler
            .get_or_insert_with(|| {
                cx.device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("vo-image"),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                })
            })
            .clone();
        let pipeline = self.pipeline(cx.device, cx.format).clone();
        let bind = cx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-image"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let [x, y, w, h] = cx.scissor;
        let mut pass = cx.enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("vo-image"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: cx.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.set_scissor_rect(x, y, w, h);
        pass.draw(0..4, 0..1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasters_upload_as_linear_premultiplied() {
        let r = Raster { w: 2, h: 1, px: vec![255, 255, 255, 255, 255, 0, 0, 0] };
        let t = linear_premul_f16(&r);
        let f = |i: usize| half::f16::from_bits(t[i]).to_f32();
        assert_eq!([f(0), f(1), f(2), f(3)], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!([f(4), f(5), f(6), f(7)], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn the_shader_validates() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("parse");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .expect("valid");
    }
}
