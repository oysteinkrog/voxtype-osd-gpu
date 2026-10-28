//! Composing an offscreen layer with bloom, a style and a mask in one pass (`compose.wgsl`),
//! the bands of a shaped bloom (bloom v2), exact `Multiply`, and the blue noise the effect
//! path's last pass dithers with.
//!
//! - **Order.** Content, plus bloom, then the style (it bends and darkens content and glow
//!   alike), then the mask, then opacity and clips, then the blend mode. Every combination of
//!   the three draws in one pass, with only the textures it needs.
//! - **Bloom v2.** A shaped bloom only runs the pyramid's downsamples. Each band reads the
//!   level that matches its spread, two levels mixed so the spread is continuous, enlarged
//!   with a cubic B-spline, then saturated, tinted and weighted.
//! - **Multiply** is drawn twice. The first draw adds `src * dst + dst * (1 - src alpha)` and
//!   leaves alpha alone; the second adds `src * (1 - dst alpha)` and writes alpha. Together
//!   they are the exact premultiplied multiply, also over a transparent target. A direct layer
//!   with `Multiply` is drawn offscreen so it can be composited this way.
//! - **Dither.** A 64 by 64 blue-noise tile (`bluenoise64.bin`, made by the tests'
//!   `blue_noise`), moved by a new offset each frame when [`Renderer::set_dither_motion`] is
//!   on, so the pattern does not sit still on a slow gradient. Off by default, so goldens and
//!   exports are exact.

use bytemuck::{Pod, Zeroable};
use vo_scene::display::{BlendMode, BloomShape};

use super::{texture_entry, Pipe, Renderer, Surface};
use crate::color::srgb_to_linear;

/// The blue-noise tile: 64 by 64 ranks from 0 to 255, row by row.
const BLUE_NOISE: &[u8; 4096] = include_bytes!("../bluenoise64.bin");

/// Bytes per `Compose` block, a multiple of the uniform offset alignment on every backend.
const COMPOSE_STRIDE: usize = 256;

/// The second draw of an exact multiply: adds `src * (1 - dst alpha)`, and alpha as "over".
pub(super) const MULTIPLY_REST: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

/// One block of `Compose` in `compose.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub(super) struct ComposeUniform {
    flags: [f32; 4],
    tint: [[f32; 4]; 3],
    band: [[f32; 4]; 3],
}

/// What `compose.wgsl` binds besides the layer: made on the first frame that needs it.
pub(super) struct ComposeGpu {
    pub(super) layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// A 1 by 1 empty texture for bindings a composite does not use.
    dummy: wgpu::TextureView,
    pub(super) noise: wgpu::TextureView,
    /// This frame's `Compose` blocks, and the buffer they go to.
    uniforms: Vec<u8>,
    buf: Option<wgpu::Buffer>,
}

/// A band of a shaped bloom: the pool indices of the two levels it mixes (the mix itself is
/// in the band's uniform).
#[derive(Clone, Copy, Debug)]
pub(super) struct BandPlan {
    lo: usize,
    hi: usize,
}

/// The pyramid level, from 1, whose blur matches `radius_px` physical pixels: level `l` blurs
/// about `2^l` pixels wide. Fractional between two levels; at least 1, at most `levels`.
pub(super) fn band_level(radius_px: f32, levels: u32) -> f32 {
    (1.0 + (radius_px.max(1e-3) / 2.0).log2()).clamp(1.0, levels as f32)
}

/// The levels a bloom needs: enough for its widest band.
pub(super) fn shape_reach(shape: &BloomShape) -> f32 {
    shape.bands.iter().filter(|b| b.weight > 0.0).map(|b| b.reach).fold(0.0, f32::max)
}

/// The pipe of the second draw of an exact multiply.
pub(super) fn rest_pipe(pipe: Pipe) -> Option<Pipe> {
    match pipe {
        Pipe::Composite => Some(Pipe::CompositeRest),
        Pipe::CompositeFx => Some(Pipe::CompositeFxRest),
        _ => None,
    }
}

/// The first draw of a multiply composite leaves alpha for the second one to write.
pub(super) fn write_mask(pipe: Pipe, blend: BlendMode) -> wgpu::ColorWrites {
    if blend == BlendMode::Multiply && rest_pipe(pipe).is_some() {
        wgpu::ColorWrites::COLOR
    } else {
        wgpu::ColorWrites::ALL
    }
}

/// The blue-noise offset for frame `n`: the R2 sequence, so successive frames land far apart.
fn dither_offset(n: u64) -> [f32; 2] {
    let (a1, a2) = (0.754_877_7_f64, 0.569_840_3_f64);
    let k = (n % 4096) as f64;
    [((0.5 + a1 * k).fract() * 64.0).floor() as f32, ((0.5 + a2 * k).fract() * 64.0).floor() as f32]
}

impl Renderer {
    /// Moves the blue-noise dither of the effect path to a new offset every frame, so it
    /// reads as fine grain instead of a fixed pattern on slow gradients. Off by default:
    /// goldens, exports and determinism checks want the same bytes for the same list.
    pub fn set_dither_motion(&mut self, on: bool) {
        self.dither_motion = on;
    }

    /// The final pass's dither offset in pixels for this frame.
    pub(super) fn dither_offset(&self) -> [f32; 2] {
        if self.dither_motion {
            dither_offset(self.frame)
        } else {
            [0.0; 2]
        }
    }

    pub(super) fn ensure_compose(&mut self) {
        if let Some(c) = &mut self.compose {
            c.uniforms.clear();
            return;
        }
        let d = &self.device;
        let mut entries = vec![texture_entry(0, false), texture_entry(1, true)];
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
        entries.extend((3..9).map(|b| texture_entry(b, true)));
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 9,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<ComposeUniform>() as u64),
            },
            count: None,
        });
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-compose"),
            entries: &entries,
        });
        let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("vo-compose"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let r8 = |label, size: u32, data: &[u8]| {
            let tex = d.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                tex.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size), rows_per_image: None },
                wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            );
            tex.create_view(&Default::default())
        };
        let dummy = r8("vo-empty", 1, &[0]);
        let noise = r8("vo-blue-noise", 64, BLUE_NOISE);
        self.compose = Some(ComposeGpu { layout, sampler, dummy, noise, uniforms: Vec::new(), buf: None });
    }

    /// Plans the bands of a shaped bloom on surface `si` from its downsamples, and writes the
    /// `Compose` block of its composite. Returns the block's byte offset.
    pub(super) fn plan_compose(&mut self, si: usize) -> u32 {
        let s = &self.surfaces[si];
        let mut u = ComposeUniform::default();
        if let Some(b) = &s.bloom {
            match &b.shape {
                None => {
                    u.flags[0] = 1.0;
                    u.band[0] = [b.strength, 1.0, 0.0, 0.0];
                }
                Some(shape) => {
                    u.flags[0] = 2.0;
                    for (k, band) in shape.bands.iter().enumerate() {
                        let t = band_level(band.reach * b.radius_px, b.levels);
                        let lo = t.floor();
                        u.band[k] = [b.strength * band.weight, band.saturation, t - lo, 0.0];
                        let tint = band.tint;
                        u.tint[k] = [
                            srgb_to_linear(tint.r),
                            srgb_to_linear(tint.g),
                            srgb_to_linear(tint.b),
                            tint.a.clamp(0.0, 1.0),
                        ];
                    }
                }
            }
        }
        u.flags[1] = s.style as u32 as f32;
        u.flags[2] = s.mask.is_some_and(|(m, _)| self.surfaces[m].size[0] != 0) as u32 as f32;
        u.flags[3] = self.light as u32 as f32;
        let c = self.compose.as_mut().expect("ensure_compose ran");
        let offset = c.uniforms.len() as u32;
        c.uniforms.extend_from_slice(bytemuck::bytes_of(&u));
        c.uniforms.resize(offset as usize + COMPOSE_STRIDE, 0);
        offset
    }

    /// The pool textures each band reads, once the surface's pyramid is allocated.
    pub(super) fn band_textures(&self, si: usize) -> [Option<BandPlan>; 3] {
        let s = &self.surfaces[si];
        let Some(b) = &s.bloom else { return [None; 3] };
        let Some(shape) = &b.shape else {
            let r = b.result();
            return [Some(BandPlan { lo: r, hi: r }), None, None];
        };
        shape.bands.map(|band| {
            let t = band_level(band.reach * b.radius_px, b.levels);
            let lo = t.floor() as usize;
            let hi = (lo + 1).min(b.levels as usize);
            (band.weight > 0.0).then(|| BandPlan { lo: b.down[lo - 1], hi: b.down[hi - 1] })
        })
    }

    pub(super) fn upload_compose(&mut self) {
        let Some(c) = &mut self.compose else { return };
        if c.uniforms.is_empty() {
            return;
        }
        let need = c.uniforms.len() as u64;
        if c.buf.as_ref().is_none_or(|b| b.size() < need) {
            c.buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vo-compose"),
                size: need.next_power_of_two(),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        self.queue.write_buffer(c.buf.as_ref().unwrap(), 0, &c.uniforms);
    }

    /// The bind group of a `CompositeFx` draw of surface `si`.
    pub(super) fn compose_bind(&self, si: usize) -> wgpu::BindGroup {
        let c = self.compose.as_ref().expect("ensure_compose ran");
        let s: &Surface = &self.surfaces[si];
        let view = |i: usize| &self.pool[i].view;
        let mask = s.mask.map(|(m, _)| &self.surfaces[m]).filter(|m| m.size[0] != 0).map(|m| view(m.tex.unwrap()));
        let bands = self.band_textures(si);
        let mut levels = [&c.dummy; 6];
        for (k, b) in bands.iter().enumerate() {
            if let Some(b) = b {
                levels[2 * k] = view(b.lo);
                levels[2 * k + 1] = view(b.hi);
            }
        }
        fn tex(binding: u32, v: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(v) }
        }
        let mut entries = vec![
            tex(0, view(s.tex.unwrap())),
            tex(1, mask.unwrap_or(&c.dummy)),
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&c.sampler) },
        ];
        entries.extend(levels.iter().enumerate().map(|(i, v)| tex(3 + i as u32, v)));
        entries.push(wgpu::BindGroupEntry {
            binding: 9,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: c.buf.as_ref().expect("uploaded"),
                offset: s.compose as u64,
                size: wgpu::BufferSize::new(std::mem::size_of::<ComposeUniform>() as u64),
            }),
        });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-compose"),
            layout: &c.layout,
            entries: &entries,
        })
    }

    /// The final pass's bind group: the scene and the blue-noise tile.
    pub(super) fn final_bind(&self, scene: usize) -> wgpu::BindGroup {
        let fx = self.fx.as_ref().expect("effect frame");
        let c = self.compose.as_ref().expect("ensure_compose ran");
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-final"),
            layout: &fx.sampled_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.pool[scene].view),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&c.noise) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&fx.sampler) },
            ],
        })
    }

    /// After a composite: the second draw of an exact multiply. Bind groups stay as they are.
    pub(super) fn draw_rest(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        format: wgpu::TextureFormat,
        pipe: Pipe,
        blend: BlendMode,
    ) {
        if blend != BlendMode::Multiply {
            return;
        }
        if let Some(rest) = rest_pipe(pipe) {
            pass.set_pipeline(self.cached_pipeline(format, rest, blend));
            pass.draw(0..3, 0..1);
        }
    }
}

/// A 64 by 64 blue-noise tile by void and cluster (Ulichney 1993): ranks 0 to 4095 scaled to
/// bytes. Toroidal Gaussian energy with sigma 1.5. `bluenoise64.bin` holds its output.
#[cfg(test)]
pub(crate) fn blue_noise() -> Vec<u8> {
    const N: usize = 64;
    const NN: usize = N * N;
    let kernel: Vec<f32> = (0..NN)
        .map(|i| {
            let (x, y) = ((i % N) as i32, (i / N) as i32);
            let dx = x.min(N as i32 - x) as f32;
            let dy = y.min(N as i32 - y) as f32;
            (-(dx * dx + dy * dy) / (2.0 * 1.5 * 1.5)).exp()
        })
        .collect();
    let splat = |energy: &mut [f32], at: usize, sign: f32| {
        let (ax, ay) = (at % N, at / N);
        for (i, e) in energy.iter_mut().enumerate() {
            let (x, y) = (i % N, i / N);
            *e += sign * kernel[(x + N - ax) % N + ((y + N - ay) % N) * N];
        }
    };
    // Tightest cluster: the set pixel with the most energy; largest void: the empty one with
    // the least.
    let pick = |energy: &[f32], set: &[bool], want: bool| {
        let mut best = None::<(usize, f32)>;
        for i in 0..NN {
            if set[i] == want {
                let e = if want { energy[i] } else { -energy[i] };
                if best.is_none_or(|(_, b)| e > b) {
                    best = Some((i, e));
                }
            }
        }
        best.expect("a pixel").0
    };
    // A seeded random start with about one pixel in ten set.
    let mut state = 0x2545_f491_u32;
    let mut set = vec![false; NN];
    let mut energy = vec![0.0f32; NN];
    let mut ones = 0;
    while ones < NN / 10 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let i = state as usize % NN;
        if !set[i] {
            set[i] = true;
            splat(&mut energy, i, 1.0);
            ones += 1;
        }
    }
    // Spread it out: move the tightest cluster into the largest void until that is a no-op.
    loop {
        let c = pick(&energy, &set, true);
        set[c] = false;
        splat(&mut energy, c, -1.0);
        let v = pick(&energy, &set, false);
        set[v] = true;
        splat(&mut energy, v, 1.0);
        if v == c {
            break;
        }
    }
    let mut rank = vec![0u32; NN];
    // Phase 1: rank the start pattern from its tightest cluster down.
    let (mut set1, mut energy1) = (set.clone(), energy.clone());
    for r in (0..ones).rev() {
        let c = pick(&energy1, &set1, true);
        set1[c] = false;
        splat(&mut energy1, c, -1.0);
        rank[c] = r as u32;
    }
    // Phase 2: fill the largest void up to half.
    let mut r = ones;
    while r < NN / 2 {
        let v = pick(&energy, &set, false);
        set[v] = true;
        splat(&mut energy, v, 1.0);
        rank[v] = r as u32;
        r += 1;
    }
    // Phase 3: the empty pixels are now the minority; fill their tightest cluster first.
    let mut empty_energy = vec![0.0f32; NN];
    for i in (0..NN).filter(|i| !set[*i]) {
        splat(&mut empty_energy, i, 1.0);
    }
    let mut empty: Vec<bool> = set.iter().map(|s| !s).collect();
    while r < NN {
        let c = pick(&empty_energy, &empty, true);
        empty[c] = false;
        splat(&mut empty_energy, c, -1.0);
        rank[c] = r as u32;
        r += 1;
    }
    rank.iter().map(|&r| (r * 256 / NN as u32) as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blue_noise_tile_matches_its_generator() {
        let fresh = blue_noise();
        if std::env::var_os("VO_BLUE_NOISE_WRITE").is_some() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/bluenoise64.bin");
            std::fs::write(path, &fresh).unwrap();
        }
        assert_eq!(fresh.as_slice(), BLUE_NOISE.as_slice(), "run with VO_BLUE_NOISE_WRITE=1 to refresh the file");
    }

    #[test]
    fn blue_noise_is_even_at_every_scale() {
        // Every byte value appears 16 times.
        let mut hist = [0u32; 256];
        BLUE_NOISE.iter().for_each(|&v| hist[v as usize] += 1);
        assert!(hist.iter().all(|&n| n == 16));
        // 8 by 8 blocks average close to the middle: in white noise a block's mean strays by
        // about 9 on average and the worst of 64 by about 20; blue noise has no low frequencies,
        // so its worst block stays under 10.
        let mut worst = 0.0f32;
        for by in 0..8 {
            for bx in 0..8 {
                let sum: u32 = (0..64).map(|i| BLUE_NOISE[(by * 8 + i / 8) * 64 + bx * 8 + i % 8] as u32).sum();
                worst = worst.max((sum as f32 / 64.0 - 127.5).abs());
            }
        }
        assert!(worst < 10.0, "an 8 by 8 block averages {worst} away from the middle");
    }

    #[test]
    fn band_levels_follow_the_spread() {
        assert_eq!(band_level(2.0, 6), 1.0);
        assert_eq!(band_level(8.0, 6), 3.0);
        assert!((band_level(11.3, 6) - 3.5).abs() < 0.01);
        assert_eq!(band_level(1000.0, 4), 4.0);
        assert_eq!(band_level(0.1, 4), 1.0);
    }

    #[test]
    fn dither_offsets_move_every_frame_and_stay_in_the_tile() {
        let offs: Vec<[f32; 2]> = (0..64).map(dither_offset).collect();
        assert!(offs.iter().all(|o| o.iter().all(|v| (0.0..64.0).contains(v))));
        assert!(offs.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn multiply_takes_two_draws_and_the_first_keeps_alpha() {
        assert_eq!(rest_pipe(Pipe::Composite), Some(Pipe::CompositeRest));
        assert_eq!(rest_pipe(Pipe::Shape), None);
        assert_eq!(write_mask(Pipe::Composite, BlendMode::Multiply), wgpu::ColorWrites::COLOR);
        assert_eq!(write_mask(Pipe::Composite, BlendMode::Screen), wgpu::ColorWrites::ALL);
    }

    /// The two draws of `Multiply`, worked on the CPU with the blend factors as wgpu applies
    /// them, equal the premultiplied multiply `s (1 - da) + d (1 - sa) + s d`.
    #[test]
    fn two_draw_multiply_is_exact() {
        let cases = [([0.3, 0.6, 0.1, 0.6], [0.5, 0.2, 0.4, 0.8]), ([0.4, 0.4, 0.0, 0.5], [0.0, 0.0, 0.0, 0.0])];
        for (s, d) in cases {
            let first: Vec<f32> = (0..3).map(|i| s[i] * d[i] + d[i] * (1.0 - s[3])).collect();
            let second: Vec<f32> = (0..3).map(|i| s[i] * (1.0 - d[3]) + first[i]).collect();
            let alpha = s[3] + d[3] * (1.0 - s[3]);
            for i in 0..3 {
                let exact = s[i] * (1.0 - d[3]) + d[i] * (1.0 - s[3]) + s[i] * d[i];
                assert!((second[i] - exact).abs() < 1e-6, "{s:?} {d:?}");
            }
            assert!((alpha - (s[3] + d[3] - s[3] * d[3])).abs() < 1e-6);
        }
    }
}
