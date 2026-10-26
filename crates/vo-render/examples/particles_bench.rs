//! Particle costs: pipeline creation, GPU time per tick and per frame at several particle
//! counts, word particle costs, and buffer memory.
//!
//! `just bench particles_bench`. Needs timestamp queries inside encoders; without them only
//! wall times are printed.
#![allow(clippy::disallowed_methods, reason = "benchmark: every clock read here times a step")]

use std::time::Instant;

use vo_render::particles::{EmitterLook, EmitterSim, FrameParams, ParticleConfig, Particles, SlotLook, WordGlyph};
use vo_scene::element::TextStyle;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Affine, Color, Point};
use vo_scene::text::{ShapedLayout, TextLayout};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const W: u32 = 1280;
const H: u32 = 400;

fn sim(_: u32, t: f64) -> EmitterSim {
    let l = (0.5 + 0.5 * (t * 3.0).sin()) as f32;
    let r = 120.0 + 50.0 * l;
    EmitterSim {
        swirl: 1.5 + 3.0 * l,
        level: l,
        noise: 1500.0 + 2500.0 * l,
        noise_scale: 0.015,
        life: 1.4,
        ..EmitterSim::ring(r, 0.3)
    }
}

fn look(_: u32) -> EmitterLook {
    EmitterLook {
        origin: Point::new(640.0, 200.0),
        size: 1.6,
        brightness: 0.8,
        ramp: [Color::hex(0xa8e6ff, 0.9), Color::hex(0x5b8cff, 0.7), Color::hex(0xb04cff, 0.0)],
        ..Default::default()
    }
}

struct Timer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    period: f64,
}

impl Timer {
    fn new(d: &wgpu::Device, q: &wgpu::Queue) -> Timer {
        let set =
            d.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
        let resolve = d.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = d.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Timer { set, resolve, read, period: q.get_timestamp_period() as f64 }
    }

    fn finish(&self, enc: &mut wgpu::CommandEncoder) {
        enc.resolve_query_set(&self.set, 0..2, &self.resolve, 0);
        enc.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, 16);
    }

    /// Milliseconds between the two timestamps, after the submit.
    fn ms(&self, d: &wgpu::Device) -> f64 {
        self.read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let v: [u64; 2] = {
            let data = self.read.slice(..).get_mapped_range().expect("mapped");
            bytemuck::pod_read_unaligned(&data[..16])
        };
        self.read.unmap();
        (v[1].wrapping_sub(v[0])) as f64 * self.period / 1e6
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() {
    let t_dev = Instant::now();
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("no GPU adapter");
    let want = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    let features = adapter.features() & want;
    let (d, q) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("particles-bench"),
        required_features: features,
        ..Default::default()
    }))
    .expect("no GPU device");
    let timed = features == want;
    println!("adapter: {} ({:?})", adapter.get_info().name, adapter.get_info().backend);
    println!("device: {:.1} ms; timestamps: {timed}", t_dev.elapsed().as_secs_f64() * 1e3);

    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();

    // Cold start: shader modules and the two compute pipelines, then the two sprite pipelines.
    let t0 = Instant::now();
    let mut probe = Particles::new(
        &d,
        &q,
        fonts.clone(),
        ParticleConfig { free_capacity: 64, word_slots: 1, ..Default::default() },
    );
    d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let new_ms = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = Instant::now();
    probe.ensure_pipelines(FORMAT);
    let draw_ms = t1.elapsed().as_secs_f64() * 1e3;
    println!("cold start: compute pipelines and buffers {new_ms:.1} ms, sprite pipelines {draw_ms:.1} ms");
    drop(probe);

    let target = d.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let params = |t: f64| FrameParams {
        t,
        to_px: Affine::IDENTITY,
        px_per_unit: 1.0,
        width: W,
        height: H,
        format: FORMAT,
        stretch: 1.0 / 60.0,
        camera: None,
        blend: Default::default(),
        depth: None,
    };

    println!();
    println!("free particles, ring emitter, {W}x{H} target, 120 Hz tick");
    println!(
        "{:>8} {:>12} {:>14} {:>12} {:>12} {:>10}",
        "count", "tick GPU ms", "120 ticks ms", "draw GPU ms", "frame CPU ms", "buffers MB"
    );
    for count in [2_048u32, 20_480, 102_400] {
        let mut p = Particles::new(
            &d,
            &q,
            fonts.clone(),
            ParticleConfig { free_capacity: count, word_slots: 1, ..Default::default() },
        );
        p.add_ring(count, 3, 1.0);
        p.ensure_pipelines(FORMAT);
        // Warm up two seconds so every particle is alive.
        p.advance(2.0, &sim);
        d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let timer = Timer::new(&d, &q);
        let (mut tick, mut draw, mut cpu) = (Vec::new(), Vec::new(), Vec::new());
        for f in 0..60 {
            let t = 2.0 + f as f64 / 120.0;
            if timed {
                let mut enc = d.create_command_encoder(&Default::default());
                enc.write_timestamp(&timer.set, 0);
                let n = (p.ticks_for(t) - p.tick()) as u32;
                p.encode_ticks(&mut enc, n.max(1), &sim);
                enc.write_timestamp(&timer.set, 1);
                timer.finish(&mut enc);
                q.submit([enc.finish()]);
                tick.push(timer.ms(&d) / n.max(1) as f64);
            }
            let c0 = Instant::now();
            p.advance(t, &sim);
            p.prepare(&params(t), &look, &[]);
            let mut enc = d.create_command_encoder(&Default::default());
            {
                let att = [Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })];
                let tw = timed.then_some(wgpu::RenderPassTimestampWrites {
                    query_set: &timer.set,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                });
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &att,
                    depth_stencil_attachment: None,
                    timestamp_writes: tw,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                p.draw(&mut pass);
            }
            if timed {
                timer.finish(&mut enc);
            }
            q.submit([enc.finish()]);
            cpu.push(c0.elapsed().as_secs_f64() * 1e3);
            if timed {
                draw.push(timer.ms(&d));
            } else {
                d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            }
        }
        // A batch: one second of ticks in one go, as a seek replays them.
        let mut p2 = Particles::new(
            &d,
            &q,
            fonts.clone(),
            ParticleConfig { free_capacity: count, word_slots: 1, ..Default::default() },
        );
        p2.add_ring(count, 3, 1.0);
        let b1 = Instant::now();
        p2.advance(1.0 - 1e-9, &sim);
        d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let batch_ms = b1.elapsed().as_secs_f64() * 1e3;
        println!(
            "{count:>8} {:>12.4} {:>14.2} {:>12.4} {:>12.3} {:>10.2}",
            if timed { median(tick) } else { f64::NAN },
            batch_ms,
            if timed { median(draw) } else { f64::NAN },
            median(cpu),
            p.gpu_bytes() as f64 / 1e6
        );
    }

    // Word particles: a sentence of 8 words at 28 px.
    println!();
    let slots = 8u32;
    let mut p = Particles::new(
        &d,
        &q,
        fonts.clone(),
        ParticleConfig { free_capacity: 64, word_slots: slots, slot_capacity: 1024, ..Default::default() },
    );
    p.ensure_pipelines(FORMAT);
    let layout = ShapedLayout::new(fonts.clone());
    let words = ["particle", "words", "form", "from", "glyph", "outlines", "then", "settle"];
    let mut looks = Vec::new();
    let mut set_cold = Vec::new();
    let mut set_warm = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let shaped = layout.layout(w, &TextStyle::new(0, 28.0, 36.0), None);
        let first = shaped.glyphs[0].pos;
        let glyphs: Vec<WordGlyph> = shaped
            .glyphs
            .iter()
            .map(|g| WordGlyph {
                font: 0,
                glyph: g.glyph,
                size: 28.0,
                pos: Point::new(g.pos.x - first.x, g.pos.y - first.y),
            })
            .collect();
        let c = Instant::now();
        p.set_word(i as u32, 0.1 * i as f64, &glyphs, 1.0);
        set_cold.push(c.elapsed().as_secs_f64() * 1e3);
        let c = Instant::now();
        p.set_word(i as u32, 0.1 * i as f64 + 0.05, &glyphs, 1.0);
        set_warm.push(c.elapsed().as_secs_f64() * 1e3);
        looks.push(SlotLook {
            origin: Point::new(40.0 + 150.0 * (i % 8) as f32, 200.0),
            color: Color::WHITE,
            visible: true,
            ..SlotLook::new(Point::ZERO, Color::WHITE)
        });
    }
    let timer = Timer::new(&d, &q);
    let mut draw = Vec::new();
    for f in 0..30 {
        let t = 0.75 + f as f64 / 120.0;
        p.prepare(&params(t), &look, &looks);
        let mut enc = d.create_command_encoder(&Default::default());
        {
            let att = [Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })];
            let tw = timed.then_some(wgpu::RenderPassTimestampWrites {
                query_set: &timer.set,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: Some(1),
            });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &att,
                depth_stencil_attachment: None,
                timestamp_writes: tw,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.draw(&mut pass);
        }
        if timed {
            timer.finish(&mut enc);
        }
        q.submit([enc.finish()]);
        if timed {
            draw.push(timer.ms(&d));
        }
    }
    println!(
        "word particles: {slots} slots x 1024; set_word {:.2} ms first time (glyph sampling), {:.3} ms cached; draw {:.4} ms GPU; buffers {:.2} MB",
        median(set_cold),
        median(set_warm),
        if timed { median(draw) } else { f64::NAN },
        p.gpu_bytes() as f64 / 1e6
    );
}
