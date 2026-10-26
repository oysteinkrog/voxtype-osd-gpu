//! P0-07: Slug glyph coverage against a 256-sample CPU reference, and GPU
//! time for 300 glyphs.
//!
//! Renders a fixed text set offscreen at 14, 24, 56 and 120 px, scales 1,
//! 1.25, 1.5 and 2, rotations 0, 7 and 45 degrees. The reference is tiny-skia
//! at 16x, box-filtered down: with its 4 x 4 anti-aliasing (64 x 64 samples
//! per pixel, the main number) and without (16 x 16 point samples). Both sides composite glyphs with premultiplied "over"
//! and are compared as raw coverage, with no gamma. Contact sheets for scale 1
//! go to /var/tmp/claude/vo-p0/.
//!
//!   cargo run --release -p vo-render --example p0_slug

mod common;

use std::time::Instant;

use common::*;
use skrifa::prelude::{LocationRef, Size};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use vo_render::text::slug_encode::Encoder;
use vo_render::text::slug_gpu::{Atlas, Footprint, GlyphInstance, SlugRenderer, Uniforms};

const TEXTS: [&str; 4] = [
    "Hamburgefonstiv ÆØÅ æøå",
    "0123456789 .,;:!?()[]{}&@%/",
    "The quick brown fox jumps over the lazy dog",
    "Blåbærsyltetøy på skjøre kjøkkenbord",
];
const SS: u32 = 16;

struct Placed {
    gid: GlyphId,
    inst: GlyphInstance,
}

/// Lays the texts out with cmap and advances (no shaping), wrapping at
/// `wrap_px`, rotated by `deg` about the first origin, then moves everything
/// so the ink starts at `pad` pixels. Returns glyphs and canvas size.
fn layout(font: &FontRef, atlas: &mut Atlas, px: f32, deg: f32, wrap_px: f32, texts: &[&str]) -> (Vec<Placed>, u32, u32) {
    let upem = font.head().unwrap().units_per_em() as f32;
    let k = px / upem;
    let cmap = font.charmap();
    let adv = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    let angle = deg.to_radians();
    let (s, c) = angle.sin_cos();
    let line_h = px * 1.4;
    let mut out = Vec::new();
    let mut line = 0.0f32;
    for text in texts {
        let mut x = 0.0f32;
        for word in text.split(' ') {
            let gids: Vec<GlyphId> = word.chars().map(|ch| cmap.map(ch).unwrap_or_default()).collect();
            let w: f32 = gids.iter().map(|&g| adv.advance_width(g).unwrap_or(0.0) * k).sum();
            if x > 0.0 && x + w > wrap_px {
                x = 0.0;
                line += 1.0;
            }
            for g in gids {
                if let Some(e) = atlas.glyph(font, g) {
                    let (lx, ly) = (x, line * line_h + px);
                    // Rotate the pen position with the glyph (screen Y down).
                    let origin = [lx * c + ly * s, -lx * s + ly * c];
                    out.push(Placed { gid: g, inst: GlyphInstance::new(&e, origin, k, angle) });
                }
                x += adv.advance_width(g).unwrap_or(0.0) * k;
            }
            x += adv.advance_width(cmap.map(' ').unwrap_or_default()).unwrap_or(0.0) * k;
        }
        line += 1.0;
    }
    let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in &out {
        for q in corners(&p.inst) {
            min = [min[0].min(q[0]), min[1].min(q[1])];
            max = [max[0].max(q[0]), max[1].max(q[1])];
        }
    }
    let pad = 8.0;
    for p in &mut out {
        p.inst.offset[0] += pad - min[0].floor();
        p.inst.offset[1] += pad - min[1].floor();
    }
    let w = (max[0].ceil() - min[0].floor() + 2.0 * pad) as u32;
    let h = (max[1].ceil() - min[1].floor() + 2.0 * pad) as u32;
    (out, w, h)
}

fn corners(g: &GlyphInstance) -> [[f32; 2]; 4] {
    let e = g.ext;
    let l = g.lin;
    let f = |x: f32, y: f32| [l[0] * x + l[2] * y + g.offset[0], l[1] * x + l[3] * y + g.offset[1]];
    [f(e[0], e[1]), f(e[2], e[1]), f(e[0], e[3]), f(e[2], e[3])]
}

struct PathPen(tiny_skia::PathBuilder);

impl skrifa::outline::OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y)
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y)
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.quad_to(cx, cy, x, y)
    }
    fn curve_to(&mut self, a: f32, b: f32, c: f32, d: f32, x: f32, y: f32) {
        self.0.cubic_to(a, b, c, d, x, y)
    }
    fn close(&mut self) {
        self.0.close()
    }
}

fn glyph_path(font: &FontRef, gid: GlyphId) -> Option<tiny_skia::Path> {
    let glyph = font.outline_glyphs().get(gid)?;
    let mut pen = PathPen(tiny_skia::PathBuilder::new());
    glyph.draw(skrifa::outline::DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).ok()?;
    pen.0.finish()
}

/// Reference coverage, composited with premultiplied "over". Without `fine`
/// it is 16 x 16 point samples per pixel, which resolves an axis-aligned edge
/// only in steps of 1/16. With `fine`, tiny-skia's own 4 x 4 anti-aliasing
/// runs on the 16x raster, so 64 x 64 samples per pixel.
fn reference(font: &FontRef, glyphs: &[Placed], w: u32, h: u32, fine: bool) -> Vec<f32> {
    let mut img = vec![0f32; (w * h) as usize];
    let mut cache = std::collections::HashMap::new();
    for p in glyphs {
        let path = cache.entry(p.gid).or_insert_with(|| glyph_path(font, p.gid));
        let Some(path) = path.as_ref() else { continue };
        let cs = corners(&p.inst);
        let bx = cs.iter().map(|c| c[0]).fold(f32::MAX, f32::min).floor() as i32 - 1;
        let by = cs.iter().map(|c| c[1]).fold(f32::MAX, f32::min).floor() as i32 - 1;
        let ex = cs.iter().map(|c| c[0]).fold(f32::MIN, f32::max).ceil() as i32 + 1;
        let ey = cs.iter().map(|c| c[1]).fold(f32::MIN, f32::max).ceil() as i32 + 1;
        let (bw, bh) = ((ex - bx) as u32, (ey - by) as u32);
        let mut mask = tiny_skia::Mask::new(bw * SS, bh * SS).unwrap();
        let l = p.inst.lin;
        let t = tiny_skia::Transform::from_row(l[0], l[1], l[2], l[3], p.inst.offset[0], p.inst.offset[1])
            .post_translate(-bx as f32, -by as f32)
            .post_scale(SS as f32, SS as f32);
        mask.fill_path(path, tiny_skia::FillRule::Winding, fine, t);
        let data = mask.data();
        for y in 0..bh {
            for x in 0..bw {
                let mut n = 0u32;
                for sy in 0..SS {
                    let row = ((y * SS + sy) * bw * SS + x * SS) as usize;
                    n += data[row..row + SS as usize].iter().map(|&v| v as u32).sum::<u32>();
                }
                if n == 0 {
                    continue;
                }
                let (px, py) = (bx + x as i32, by + y as i32);
                if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                    continue;
                }
                let a = n as f32 / (SS * SS * 255) as f32;
                let d = &mut img[(py as u32 * w + px as u32) as usize];
                *d = a + *d * (1.0 - a);
            }
        }
    }
    img
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn render_coverage(gpu: &Gpu, r: &mut SlugRenderer, atlas: &Atlas, glyphs: &[Placed], w: u32, h: u32) -> Vec<f32> {
    let (d, q) = (&gpu.device, &gpu.queue);
    let tex = d.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    r.upload_atlas(d, q, atlas);
    r.set_uniforms(q, &Uniforms::pixels(w as f32, h as f32));
    let inst: Vec<GlyphInstance> = glyphs.iter().map(|p| p.inst).collect();
    r.set_instances(d, q, &inst);
    let row = (w * 8).div_ceil(256) * 256;
    let rb = d.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = d.create_command_encoder(&Default::default());
    {
        let att = clear_att(&view);
        let mut pass = enc.begin_render_pass(&pass_desc(&att, None));
        r.draw(&mut pass);
    }
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &rb,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    q.submit([enc.finish()]);
    rb.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = rb.slice(..).get_mapped_range().expect("map");
    let mut out = vec![0f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let o = (y * row + x * 8 + 6) as usize;
            out[(y * w + x) as usize] = f16_to_f32(u16::from_le_bytes([data[o], data[o + 1]]));
        }
    }
    out
}

fn clear_att(view: &wgpu::TextureView) -> [Option<wgpu::RenderPassColorAttachment<'_>>; 1] {
    [Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
    })]
}

fn pass_desc<'a>(
    att: &'a [Option<wgpu::RenderPassColorAttachment<'a>>],
    tw: Option<wgpu::RenderPassTimestampWrites<'a>>,
) -> wgpu::RenderPassDescriptor<'a> {
    wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: att,
        depth_stencil_attachment: None,
        timestamp_writes: tw,
        occlusion_query_set: None,
        multiview_mask: None,
    }
}

#[derive(Default)]
struct Err {
    max: f32,
    mean_ink: f64,
    mean_edge: f64,
    p99_ink: f32,
    over_10: f64,
    ink_px: usize,
}

fn compare(gpu: &[f32], reference: &[f32]) -> Err {
    let mut errs = Vec::new();
    let (mut sum_edge, mut n_edge) = (0f64, 0usize);
    for (g, r) in gpu.iter().zip(reference) {
        if *g <= 1.0 / 512.0 && *r <= 1.0 / 512.0 {
            continue;
        }
        let e = (g - r).abs();
        errs.push(e);
        if *r > 0.0 && *r < 1.0 {
            sum_edge += e as f64;
            n_edge += 1;
        }
    }
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = errs.len().max(1);
    Err {
        max: errs.last().copied().unwrap_or(0.0),
        mean_ink: errs.iter().map(|&e| e as f64).sum::<f64>() / n as f64,
        mean_edge: sum_edge / n_edge.max(1) as f64,
        p99_ink: errs.get((errs.len().saturating_sub(1) as f64 * 0.99) as usize).copied().unwrap_or(0.0),
        over_10: errs.iter().filter(|&&e| e > 0.1).count() as f64 / n as f64,
        ink_px: errs.len(),
    }
}

/// Three panels stacked: GPU, reference, error (red, 4x gain). Black ink on
/// white, raw coverage, upscaled by `zoom` with nearest neighbour.
fn contact_sheet(path: &str, gpu: &[f32], reference: &[f32], w: u32, h: u32, zoom: u32) {
    let (zw, zh) = (w * zoom, h * zoom);
    let gap = 6 * zoom;
    let total_h = zh * 3 + gap * 2;
    let mut rgb = vec![200u8; (zw * total_h * 3) as usize];
    for panel in 0..3u32 {
        for y in 0..zh {
            for x in 0..zw {
                let i = ((y / zoom) * w + x / zoom) as usize;
                let px = match panel {
                    0 => [(255.0 * (1.0 - gpu[i])) as u8; 3],
                    1 => [(255.0 * (1.0 - reference[i])) as u8; 3],
                    _ => {
                        let e = ((gpu[i] - reference[i]).abs() * 4.0).min(1.0);
                        [255, (255.0 * (1.0 - e)) as u8, (255.0 * (1.0 - e)) as u8]
                    }
                };
                let o = (((panel * (zh + gap)) + y) * zw + x) as usize * 3;
                rgb[o..o + 3].copy_from_slice(&px);
            }
        }
    }
    write_png_rgb(path, zw, total_h, &rgb);
}

fn time_glyphs(gpu: &Gpu, r: &mut SlugRenderer, font: &FontRef, atlas: &mut Atlas, px: f32) -> Option<(usize, f64, f64, f64)> {
    let (d, q) = (&gpu.device, &gpu.queue);
    if !d.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
        return None;
    }
    // Phase 1 card surface: about 2100 x 1350 physical pixels.
    let (w, h) = (2100u32, 1350u32);
    let mut glyphs = Vec::new();
    while glyphs.len() < 300 {
        let (more, _, _) = layout(font, atlas, px, 0.0, w as f32 - 40.0, &TEXTS);
        glyphs.extend(more.into_iter().map(|p| p.inst));
    }
    glyphs.truncate(300);
    // Rows of 300 glyphs: restack by line so they stay on the surface.
    let line_h = px * 1.4;
    let mut x = 20.0f32;
    let mut y = px + 20.0;
    for g in &mut glyphs {
        let adv = (g.ext[2] - g.ext[0]) * g.lin[0] + px * 0.1;
        if x + adv > w as f32 - 20.0 {
            x = 20.0;
            y += line_h;
        }
        g.offset = [x - g.ext[0] * g.lin[0], y];
        x += adv;
    }
    let tex = d.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    r.upload_atlas(d, q, atlas);
    r.set_uniforms(q, &Uniforms::pixels(w as f32, h as f32));
    r.set_instances(d, q, &glyphs);
    let frames = 600u32;
    let set = d.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 4 });
    let resolve = d.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 32,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let rb = d.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 32 * frames as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    for f in 0..frames {
        let mut enc = d.create_command_encoder(&Default::default());
        // Pass 1: clear only. Pass 2: clear plus 300 glyphs.
        for (i, draw) in [(0u32, false), (2, true)] {
            let tw = wgpu::RenderPassTimestampWrites {
                query_set: &set,
                beginning_of_pass_write_index: Some(i),
                end_of_pass_write_index: Some(i + 1),
            };
            let att = clear_att(&view);
            let mut pass = enc.begin_render_pass(&pass_desc(&att, Some(tw)));
            if draw {
                r.draw(&mut pass);
            }
        }
        enc.resolve_query_set(&set, 0..4, &resolve, 0);
        enc.copy_buffer_to_buffer(&resolve, 0, &rb, 32 * f as u64, 32);
        q.submit([enc.finish()]);
    }
    rb.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let period = q.get_timestamp_period() as f64;
    let data = rb.slice(..).get_mapped_range().expect("map");
    let ts: &[u64] = bytemuck::cast_slice(&data);
    let (mut clear, mut glyph) = (Vec::new(), Vec::new());
    // Skip the first 200 frames so the GPU clock can ramp up from idle.
    for f in 200..frames as usize {
        let t = &ts[f * 4..f * 4 + 4];
        clear.push((t[1] - t[0]) as f64 * period / 1e6);
        glyph.push((t[3] - t[2]) as f64 * period / 1e6);
    }
    clear.sort_by(|a, b| a.partial_cmp(b).unwrap());
    glyph.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some((glyphs.len(), percentile(&clear, 0.5), percentile(&glyph, 0.5), percentile(&glyph, 0.99)))
}

fn main() {
    let font_path = ui_font_path();
    let data = std::fs::read(&font_path).expect("font");
    let font = FontRef::new(&data).expect("parse font");
    std::fs::create_dir_all(OUT_DIR).unwrap();
    println!("font {}", font_path.display());

    let (adapter, device, queue) = gpu();
    let info = adapter.get_info();
    println!("adapter {} ({:?}, driver {})", info.name, info.backend, info.driver_info);
    let gpu = Gpu { device, queue };
    let t = Instant::now();
    let _ = SlugRenderer::new(&gpu.device, wgpu::TextureFormat::Rgba16Float);
    println!("pipeline create {:.1} ms", t.elapsed().as_secs_f64() * 1e3);

    // Encode cost: every glyph the texts use, cold.
    let mut encoder = Encoder::new();
    let cmap = font.charmap();
    let mut gids: Vec<GlyphId> = TEXTS.iter().flat_map(|t| t.chars()).filter_map(|c| cmap.map(c)).collect();
    gids.sort();
    gids.dedup();
    let t = Instant::now();
    let mut bytes = 0;
    for &g in &gids {
        bytes += encoder.encode_glyph(&font, g).map_or(0, |e| e.texels.len() * 8);
    }
    let enc_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "encode {} glyphs: {:.2} ms ({:.1} us each), {} KiB as i16, {} KiB in the i32 buffer",
        gids.len(),
        enc_ms,
        enc_ms * 1e3 / gids.len() as f64,
        bytes / 1024,
        bytes * 2 / 1024
    );

    println!("\nreference 64x64 samples/px | 16x16 point samples/px");
    println!("footprint size_px scale rot_deg ink_px max_err p99_err mean_err_ink mean_err_edge share_over_0.1 | max_err mean_err_edge");
    let mut atlas = Atlas::default();
    let footprints = [(Footprint::Fwidth, "fwidth"), (Footprint::AxisLength, "axis")];
    let mut rs: Vec<SlugRenderer> = footprints
        .iter()
        .map(|&(fp, _)| SlugRenderer::with_footprint(&gpu.device, wgpu::TextureFormat::Rgba16Float, fp))
        .collect();
    let mut worst = [Err::default(), Err::default()];
    for px in [14.0f32, 24.0, 56.0, 120.0] {
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            for deg in [0.0f32, 7.0, 45.0] {
                let eff = px * scale;
                let (glyphs, w, h) = layout(&font, &mut atlas, eff, deg, 1800.0, &TEXTS);
                let coarse_ref = reference(&font, &glyphs, w, h, false);
                let rf = reference(&font, &glyphs, w, h, true);
                for (i, (_, name)) in footprints.iter().enumerate() {
                    let g = render_coverage(&gpu, &mut rs[i], &atlas, &glyphs, w, h);
                    let coarse = compare(&g, &coarse_ref);
                    let e = compare(&g, &rf);
                    println!(
                        "{name:<6} {px:>5} {scale:>5} {deg:>4} {:>8} {:.4} {:.4} {:.5} {:.5} {:.4} | {:.4} {:.5}",
                        e.ink_px, e.max, e.p99_ink, e.mean_ink, e.mean_edge, e.over_10, coarse.max, coarse.mean_edge
                    );
                    if scale == 1.0 {
                        let zoom = if px <= 14.0 { 4 } else if px <= 24.0 { 2 } else { 1 };
                        contact_sheet(&format!("{OUT_DIR}/slug-{px}px-rot{deg}-{name}.png"), &g, &rf, w, h, zoom);
                    }
                    let wst = &mut worst[i];
                    wst.max = wst.max.max(e.max);
                    wst.mean_ink = wst.mean_ink.max(e.mean_ink);
                    wst.mean_edge = wst.mean_edge.max(e.mean_edge);
                }
            }
        }
    }
    for ((_, name), wst) in footprints.iter().zip(&worst) {
        println!(
            "\nworst over all runs, {name}: max {:.4}, mean (ink) {:.5}, mean (edge) {:.5}",
            wst.max, wst.mean_ink, wst.mean_edge
        );
    }

    println!("\nGPU time, 300 glyphs on 2100x1350 Rgba8Unorm, 400 frames after 200 warm-up:");
    for &(fp, name) in &footprints {
        let mut r8 = SlugRenderer::with_footprint(&gpu.device, wgpu::TextureFormat::Rgba8Unorm, fp);
        for px in [14.0f32, 24.0, 56.0] {
            match time_glyphs(&gpu, &mut r8, &font, &mut atlas, px) {
                Some((n, clear, p50, p99)) => println!(
                    "{name:<6} {px:>5} px: {n} glyphs, clear-only pass p50 {clear:.3} ms, clear+glyphs p50 {p50:.3} ms p99 {p99:.3} ms"
                ),
                None => println!("no TIMESTAMP_QUERY on this adapter"),
            }
        }
    }
    println!("atlas {} glyphs, {} KiB", atlas.entries.len(), atlas.bytes() / 1024);
}
