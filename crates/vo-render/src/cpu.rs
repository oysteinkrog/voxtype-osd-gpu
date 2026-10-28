//! Draws a display list on the CPU, for the first frames while the GPU device is still being
//! made (roadmap Q9). The result should match the GPU path closely enough that the hand-over
//! does not show.
//!
//! - Shapes run the shape shader's maths per pixel: the same signed distances, the same
//!   coverage rule, OKLab gradients through [`GpuShape`], so edges and colours agree.
//! - Glyphs are filled from their unhinted outlines with tiny-skia's anti-aliasing, not Slug.
//!   Edges can differ by a fraction of a pixel.
//! - Compositing is in linear light with premultiplied alpha, then sRGB encoded, as the GPU
//!   writes to an sRGB swapchain.
//! - Glows and drop shadows run the shape shader's halo maths per pixel. A bloom layer is
//!   drawn alone and its blurred copy added, through the same pyramid as the GPU (13-tap
//!   downsample with a Karis average, tent upsample, bilinear reads), at the GPU's levels.
//!   On such a frame the effect path's tone map applies too, without its dither. Light
//!   mode's dark bloom is not done.
//! - Not exact: offscreen layers draw as direct layers (group opacity per instance), only the
//!   nearest clip applies, and emitters are skipped. Masks are not applied: a layer under an
//!   alpha mask is hidden, one under an inverted mask unmasked.

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::instance::Size;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};
use vo_scene::display::{BlendMode, Bloom, DisplayList, GlyphInstance, ItemKind, LayerTarget, Paint, ShapeInstance};
use vo_scene::font::{Face, FontRegistry};
use vo_scene::geom::{Affine, Point, RRect};

use crate::color::{linear_premul, linear_to_srgb, oklab_to_linear};
use crate::renderer::{bloom_levels, ALIGN, BLOOM_MIX};
use crate::shapes::{pixels_per_unit, GpuPaint, GpuShape};

const KIND_CIRCLE: u32 = vo_scene::display::ShapeKind::Circle as u32;
const KIND_LINE: u32 = vo_scene::display::ShapeKind::Line as u32;
const KIND_BAND: u32 = vo_scene::display::ShapeKind::Band as u32;
const PAINT_NONE: u32 = vo_scene::display::PaintKind::None as u32;
const PAINT_SOLID: u32 = vo_scene::display::PaintKind::Solid as u32;
const PAINT_GLOW: u32 = vo_scene::display::PaintKind::Glow as u32;
const PAINT_SHADOW: u32 = vo_scene::display::PaintKind::Shadow as u32;
const PAINT_CONIC: u32 = vo_scene::display::PaintKind::Conic as u32;
const PAINT_SHEEN: u32 = vo_scene::display::PaintKind::Sheen as u32;

/// The CPU renderer. Keeps its buffers and glyph outlines between frames.
pub struct CpuRenderer {
    fonts: FontRegistry,
    faces: Vec<Arc<Face>>,
    outlines: HashMap<(u32, u32), Option<tiny_skia::Path>>,
    /// Linear premultiplied RGBA, one per physical pixel.
    acc: Vec<[f32; 4]>,
    mask: Option<tiny_skia::Mask>,
    width: u32,
    height: u32,
    /// The GPU draws this frame on its effect path, where glows may exceed 1.
    effects: bool,
}

#[derive(Clone, Copy)]
struct Clip {
    rect: RRect,
    from_px: Affine,
    ppu: f32,
    fade: [f32; 4],
}

#[derive(Clone, Copy)]
struct State {
    to_px: Affine,
    from_px: Affine,
    ppu: f32,
    opacity: f32,
    blend: BlendMode,
    clip: Option<Clip>,
}

impl CpuRenderer {
    pub fn new(fonts: FontRegistry) -> CpuRenderer {
        CpuRenderer {
            fonts,
            faces: Vec::new(),
            outlines: HashMap::new(),
            acc: Vec::new(),
            mask: None,
            width: 0,
            height: 0,
            effects: false,
        }
    }

    /// Draws `list` at `scale` physical pixels per logical unit into `out`: `width` x `height`
    /// pixels, rows `stride` bytes apart, each pixel B, G, R, A bytes (Wayland's ARGB8888),
    /// premultiplied and sRGB encoded like the GPU path's output.
    pub fn draw_bgra(
        &mut self,
        list: &DisplayList,
        scale: f32,
        width: u32,
        height: u32,
        stride: usize,
        out: &mut [u8],
    ) {
        self.draw(list, scale, width, height);
        for y in 0..height as usize {
            let row = &mut out[y * stride..y * stride + width as usize * 4];
            let src = &self.acc[y * width as usize..(y + 1) * width as usize];
            for (px, c) in row.as_chunks_mut::<4>().0.iter_mut().zip(src) {
                let a = c[3].clamp(0.0, 1.0);
                let enc = |v: f32| (linear_to_srgb(v.clamp(0.0, a)) * 255.0 + 0.5) as u8;
                px[0] = enc(c[2]);
                px[1] = enc(c[1]);
                px[2] = enc(c[0]);
                px[3] = (a * 255.0 + 0.5) as u8;
            }
        }
    }

    /// Draws into the linear premultiplied buffer, [`CpuRenderer::pixels`].
    pub fn draw(&mut self, list: &DisplayList, scale: f32, width: u32, height: u32) {
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.mask = tiny_skia::Mask::new(width.max(1), height.max(1));
        }
        self.acc.clear();
        self.acc.resize((width * height) as usize, [0.0; 4]);
        if list.layers.is_empty() {
            return;
        }
        self.effects = list.layers.iter().any(|l| bloom_of(l).is_some() || l.trail.is_some());
        self.draw_layers(list, scale);
        if self.effects {
            self.acc.iter_mut().for_each(tone_map);
        }
    }

    fn draw_layers(&mut self, list: &DisplayList, scale: f32) {
        let top = State {
            to_px: Affine::IDENTITY,
            from_px: Affine::IDENTITY,
            ppu: 1.0,
            opacity: 1.0,
            blend: BlendMode::Normal,
            clip: None,
        };
        self.visit(list, scale, 0, &top);
    }

    /// The last frame as linear premultiplied RGBA, row by row.
    pub fn pixels(&self) -> &[[f32; 4]] {
        &self.acc
    }

    fn visit(&mut self, list: &DisplayList, scale: f32, li: u32, parent: &State) {
        let layer = &list.layers[li as usize];
        let to_px = layer.transform.then(&Affine::scale(scale));
        let Some(from_px) = to_px.invert() else { return };
        let ppu = pixels_per_unit(&to_px);
        let clip = match layer.clip {
            Some(rect) => Some(Clip { rect, from_px, ppu, fade: layer.clip_fade }),
            None => parent.clip,
        };
        let opacity = parent.opacity * layer.opacity;
        if opacity <= 0.0 || hidden_by_mask(list, li) {
            return;
        }
        let st = State { to_px, from_px, ppu, opacity, blend: layer.blend, clip };
        // A bloom layer draws alone, so its blurred copy can be added.
        let bloom = bloom_of(layer).filter(|_| self.effects);
        let n = self.acc.len();
        let below = bloom.map(|_| std::mem::replace(&mut self.acc, vec![[0.0; 4]; n]));
        for item in list.layer_items(li) {
            let range = item.start as usize..(item.start + item.len) as usize;
            match item.kind {
                ItemKind::Shapes => {
                    for s in &list.shapes[range] {
                        if let Some(mut g) = GpuShape::new(s, &to_px, ppu, opacity) {
                            if !self.effects {
                                g.clamp_glow();
                            }
                            self.shape(&g, &st);
                        }
                    }
                }
                ItemKind::Glyphs => {
                    for g in &list.glyphs[range] {
                        self.glyph(g, &st);
                    }
                }
                // The CPU first frame draws no vectors or emitters.
                ItemKind::Emitters | ItemKind::Vectors | ItemKind::Gpu => {}
                ItemKind::Dots => {
                    for d in &list.dots[range] {
                        let centre = crate::dots::snap(d.centre, d.radius, d.snap, &to_px, ppu);
                        let s = ShapeInstance::circle(centre, d.radius, Paint::solid(d.color));
                        if let Some(g) = GpuShape::new(&s, &to_px, ppu, opacity) {
                            self.shape(&g, &st);
                        }
                    }
                }
                // The first frame draws glass as its interior tint, which already meets the
                // contrast bar; the GPU adds the bevel, rim and sheen.
                ItemKind::Glass => {
                    for g in &list.glass[range] {
                        let [x, y, w, h] = g.rect;
                        let rect = vo_scene::geom::RRect::new(vo_scene::geom::Rect::new(x, y, w, h), g.radius);
                        let s = ShapeInstance::rrect(rect, Paint::solid(g.glass.tint).fade(g.alpha));
                        if let Some(gs) = GpuShape::new(&s, &to_px, ppu, opacity) {
                            self.shape(&gs, &st);
                        }
                    }
                }
                ItemKind::Layer => self.visit(list, scale, item.start, &st),
            }
        }
        if let (Some(b), Some(below)) = (bloom, below) {
            let layer = std::mem::replace(&mut self.acc, below);
            self.add_bloom(&layer, b, ppu, st.blend, parent.clip.as_ref());
        }
    }

    /// Draws a bloom layer's pixels `layer` (the whole frame) over the buffer, plus
    /// `b.strength` times their blur through the GPU's pyramid, inside `clip`.
    fn add_bloom(&mut self, layer: &[[f32; 4]], b: Bloom, ppu: f32, blend: BlendMode, clip: Option<&Clip>) {
        let (w, h) = (self.width as i32, self.height as i32);
        let mut drawn = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for (i, c) in layer.iter().enumerate() {
            if c.iter().any(|v| *v != 0.0) {
                let (x, y) = (i as i32 % w, i as i32 / w);
                drawn = [drawn[0].min(x), drawn[1].min(y), drawn[2].max(x + 1), drawn[3].max(y + 1)];
            }
        }
        if drawn[2] <= drawn[0] {
            return;
        }
        // The layer texture as the GPU sizes it: what it drew, padded by the bloom's reach,
        // rounded up to `ALIGN`.
        let (mut levels, mut top) = bloom_levels(b.radius * ppu);
        let pad = 3 << levels;
        let (ox, oy) = (drawn[0] - pad, drawn[1] - pad);
        let tw = ((drawn[2] + pad - ox) as u32).next_multiple_of(ALIGN);
        let th = ((drawn[3] + pad - oy) as u32).next_multiple_of(ALIGN);
        let fit = (tw.min(th) / 4).max(1).ilog2().max(1);
        if levels > fit {
            (levels, top) = (fit, 1.0);
        }
        let mut src = Img::new(tw, th);
        for y in 0..th as i32 {
            for x in 0..tw as i32 {
                let (fx, fy) = (ox + x, oy + y);
                if (0..w).contains(&fx) && (0..h).contains(&fy) {
                    src.px[(y as u32 * tw + x as u32) as usize] = layer[(fy * w + fx) as usize];
                }
            }
        }
        let glow = pyramid(src, levels, top);
        for fy in oy.max(0)..(oy + th as i32).min(h) {
            for fx in ox.max(0)..(ox + tw as i32).min(w) {
                let base = layer[(fy * w + fx) as usize];
                let uv = [(fx - ox) as f32 + 0.5, (fy - oy) as f32 + 0.5];
                let g = glow.sample(uv[0] / tw as f32, uv[1] / th as f32);
                let mut c = [0, 1, 2, 3].map(|i| base[i] + b.strength * g[i]);
                c[3] = c[3].min(1.0);
                if c.iter().all(|v| *v <= 0.0) {
                    continue;
                }
                let k = clip_coverage(clip, Point::new(fx as f32 + 0.5, fy as f32 + 0.5));
                self.blend(fx as u32, fy as u32, scale4(c, k), blend);
            }
        }
    }

    /// The pixel box `[x0, y0, x1, y1)` covering `r` (layer space) through `to_px`, clamped.
    fn px_box(&self, r: [f32; 4], to_px: &Affine) -> Option<[u32; 4]> {
        let corners =
            [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])].map(|(x, y)| to_px.apply(Point::new(x, y)));
        let x0 = corners.iter().map(|p| p.x).fold(f32::MAX, f32::min).floor().max(0.0);
        let y0 = corners.iter().map(|p| p.y).fold(f32::MAX, f32::min).floor().max(0.0);
        let x1 = corners.iter().map(|p| p.x).fold(f32::MIN, f32::max).ceil().min(self.width as f32);
        let y1 = corners.iter().map(|p| p.y).fold(f32::MIN, f32::max).ceil().min(self.height as f32);
        (x1 > x0 && y1 > y0).then_some([x0 as u32, y0 as u32, x1 as u32, y1 as u32])
    }

    fn shape(&mut self, s: &GpuShape, st: &State) {
        let fill = (s.kinds >> 8) & 0xff;
        if fill == PAINT_GLOW || fill == PAINT_SHADOW {
            self.halo(s, st);
            return;
        }
        let kind = s.kinds & 0xff;
        let g = s.geom;
        let bounds = match kind {
            KIND_CIRCLE => [g[0] - g[2], g[1] - g[2], g[0] + g[2], g[1] + g[2]],
            KIND_LINE => [g[0].min(g[2]), g[1].min(g[3]), g[0].max(g[2]), g[1].max(g[3])],
            KIND_BAND => {
                let h = s.radii[0].max(s.radii[1]);
                [g[0], g[1] - h, g[2], g[1] + h]
            }
            _ => [g[0], g[1], g[0] + g[2], g[1] + g[3]],
        };
        // Grown like the shader's quad: half the stroke plus a pixel.
        let m = 0.5 * s.stroke_width + 1.0 / st.ppu;
        let Some([x0, y0, x1, y1]) =
            self.px_box([bounds[0] - m, bounds[1] - m, bounds[2] + m, bounds[3] + m], &st.to_px)
        else {
            return;
        };
        // A band's ends in pixels, compared with the pixel centre as the shader does.
        let ends = (st.to_px.apply(Point::new(g[0], g[1])).x, st.to_px.apply(Point::new(g[2], g[1])).x);
        let fill_kind = (s.kinds >> 8) & 0xff;
        let stroke_kind = (s.kinds >> 16) & 0xff;
        for y in y0..y1 {
            for x in x0..x1 {
                let frag = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                let p = st.from_px.apply(frag);
                let d = match kind {
                    KIND_CIRCLE => (p.x - g[0]).hypot(p.y - g[1]) - g[2],
                    KIND_LINE => sd_segment(p, Point::new(g[0], g[1]), Point::new(g[2], g[3])),
                    KIND_BAND => sd_band(p, g, s.radii, frag.x, ends),
                    _ => sd_rrect(p, g, s.radii),
                };
                let dp = d * st.ppu;
                let hw = 0.5 * s.stroke_width * st.ppu;
                let fill_cov = if kind == KIND_LINE { 0.0 } else { (0.5 - dp).clamp(0.0, 1.0) };
                let stroke_cov = ((dp + 0.5).min(hw) - (dp - 0.5).max(-hw)).clamp(0.0, 1.0);
                if fill_cov <= 0.0 && stroke_cov <= 0.0 {
                    continue;
                }
                let fill = scale4(paint(fill_kind, &s.fill, p), fill_cov);
                let stroke = scale4(paint(stroke_kind, &s.stroke, p), stroke_cov);
                let c = [0, 1, 2, 3].map(|i| stroke[i] + fill[i] * (1.0 - stroke[3]));
                let k = clip_coverage(st.clip.as_ref(), frag);
                self.blend(x, y, scale4(c, k), st.blend);
            }
        }
    }

    /// A glow or a drop shadow: the shape shader's `halo`, pixel by pixel.
    fn halo(&mut self, s: &GpuShape, st: &State) {
        let Some([x0, y0, x1, y1]) = self.px_box(s.quad(st.ppu), &st.to_px) else { return };
        let kind = s.kinds & 0xff;
        let glow = (s.kinds >> 8) & 0xff == PAINT_GLOW;
        let (g, e, hw) = (s.geom, s.fill.ends, 0.5 * s.stroke_width);
        let ends = (st.to_px.apply(Point::new(g[0], g[1])).x, st.to_px.apply(Point::new(g[2], g[1])).x);
        let c0 = s.fill.c0;
        let rgb = oklab_to_linear([c0[0], c0[1], c0[2]]).map(|v| v.clamp(0.0, 1.0));
        for y in y0..y1 {
            for x in x0..x1 {
                let frag = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                let p = st.from_px.apply(frag);
                let v = if glow {
                    // Around a stroke the glow falls off on both sides of it.
                    let d = sd_shape(kind, p, g, s.radii, frag.x, ends);
                    let dd = if hw > 0.0 { d.abs() } else { d } - hw;
                    e[1] * (-dd.max(0.0) / e[0].max(1e-4)).exp()
                } else {
                    // At least a quarter pixel, so a hard shadow still has an antialiased edge.
                    let sigma = e[2].max(0.25 / st.ppu);
                    let spread = e[3];
                    let q = Point::new(p.x - e[0], p.y - e[1]);
                    if kind == KIND_LINE || kind == KIND_BAND {
                        let ds = sd_shape(kind, q, g, s.radii, frag.x - e[0] * st.ppu, ends) - hw - spread;
                        0.5 - 0.5 * erf(ds * std::f32::consts::FRAC_1_SQRT_2 / sigma)
                    } else {
                        let mut rect = g;
                        if kind == KIND_CIRCLE {
                            rect = [g[0] - g[2], g[1] - g[2], 2.0 * g[2], 2.0 * g[2]];
                        }
                        let grow = spread + hw;
                        let rect = [rect[0] - grow, rect[1] - grow, rect[2] + 2.0 * grow, rect[3] + 2.0 * grow];
                        let (qx, qy) = (q.x - (rect[0] + 0.5 * rect[2]), q.y - (rect[1] + 0.5 * rect[3]));
                        let r = match (qy < 0.0, qx >= 0.0) {
                            (true, false) => s.radii[0],
                            (true, true) => s.radii[1],
                            (false, true) => s.radii[2],
                            (false, false) => s.radii[3],
                        };
                        let corner = (r + grow).clamp(0.0, 0.5 * rect[2].min(rect[3]));
                        box_shadow(rect, corner, q, sigma)
                    }
                };
                let a = c0[3] * v.max(0.0);
                if a <= 0.0 {
                    continue;
                }
                let c = [rgb[0] * a, rgb[1] * a, rgb[2] * a, a.min(1.0)];
                let k = clip_coverage(st.clip.as_ref(), frag);
                self.blend(x, y, scale4(c, k), st.blend);
            }
        }
    }

    fn glyph(&mut self, g: &GlyphInstance, st: &State) {
        let color = linear_premul(g.color, st.opacity);
        if color[3] <= 0.0 {
            return;
        }
        if g.font as usize >= self.faces.len() {
            let more = self.fonts.faces_from(self.faces.len());
            self.faces.extend(more);
        }
        let Some(face) = self.faces.get(g.font as usize).cloned() else { return };
        let outline = self.outlines.entry((g.font, g.glyph)).or_insert_with(|| glyph_path(&face, g.glyph));
        let Some(path) = outline.as_ref() else { return };
        let k = g.size / face.units_per_em() as f32;
        let m = &st.to_px;
        // Em space has y up; layer space has y down.
        let o = m.apply(g.pos);
        let t = tiny_skia::Transform::from_row(m.a * k, m.b * k, -m.c * k, -m.d * k, o.x, o.y);
        let Some(b) = path.bounds().transform(t) else { return };
        let x0 = (b.left().floor() - 1.0).max(0.0) as u32;
        let y0 = (b.top().floor() - 1.0).max(0.0) as u32;
        let x1 = (b.right().ceil() + 1.0).min(self.width as f32) as u32;
        let y1 = (b.bottom().ceil() + 1.0).min(self.height as f32) as u32;
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let Some(mut mask) = self.mask.take() else { return };
        mask.fill_path(path, tiny_skia::FillRule::Winding, true, t);
        let w = self.width as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = y as usize * w + x as usize;
                let cov = mask.data()[i];
                if cov == 0 {
                    continue;
                }
                mask.data_mut()[i] = 0;
                let frag = Point::new(x as f32 + 0.5, y as f32 + 0.5);
                let k = cov as f32 / 255.0 * clip_coverage(st.clip.as_ref(), frag);
                self.blend(x, y, scale4(color, k), st.blend);
            }
        }
        self.mask = Some(mask);
    }

    /// Premultiplied `src` onto the buffer at `(x, y)`, as the GPU's fixed-function blend does.
    fn blend(&mut self, x: u32, y: u32, s: [f32; 4], mode: BlendMode) {
        let d = &mut self.acc[(y * self.width + x) as usize];
        let out = match mode {
            BlendMode::Normal => [0, 1, 2, 3].map(|i| s[i] + d[i] * (1.0 - s[3])),
            BlendMode::Add => [0, 1, 2, 3].map(|i| s[i] + d[i]),
            BlendMode::Screen => [0, 1, 2, 3].map(|i| s[i] + d[i] * (1.0 - s[i])),
            BlendMode::Multiply => [0, 1, 2, 3].map(|i| s[i] * d[i] + s[i] * (1.0 - d[3]) + d[i] * (1.0 - s[3])),
        };
        *d = out;
    }
}

fn scale4(c: [f32; 4], k: f32) -> [f32; 4] {
    c.map(|v| v * k)
}

/// The shader's `sd_band`: distance to the straight top or bottom edge, or far outside when the
/// pixel centre `frag_x` is not between the ends (in pixels).
fn sd_band(p: Point, g: [f32; 4], r: [f32; 4], frag_x: f32, ends: (f32, f32)) -> f32 {
    if frag_x < ends.0.min(ends.1) || frag_x >= ends.0.max(ends.1) {
        return 1e9;
    }
    let slope = (r[1] - r[0]) / (g[2] - g[0]).max(1e-6);
    let half = r[0] + slope * (p.x - g[0]);
    ((p.y - g[1]).abs() - half) / (1.0 + slope * slope).sqrt()
}

/// The shader's `paint`: premultiplied linear colour at `p`, stops mixed in premultiplied OKLab.
fn paint(kind: u32, pt: &GpuPaint, p: Point) -> [f32; 4] {
    if kind == PAINT_NONE {
        return [0.0; 4];
    }
    let mut t = 0.0;
    let e = pt.ends;
    let smooth = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    if kind == PAINT_CONIC {
        let a = (p.y - e[1]).atan2(p.x - e[0]) * std::f32::consts::FRAC_1_PI * 0.5 - e[2] + 0.5;
        t = smooth(((a - a.floor()) - 0.5).abs() / e[3]);
    } else if kind == PAINT_SHEEN {
        let len2 = e[2] * e[2] + e[3] * e[3];
        t = if len2 > 0.0 { smooth(((p.x - e[0]) * e[2] + (p.y - e[1]) * e[3]).abs() / len2) } else { 1.0 };
    } else if kind != PAINT_SOLID {
        let (dx, dy) = (e[2] - e[0], e[3] - e[1]);
        let len2 = dx * dx + dy * dy;
        if len2 > 0.0 {
            t = (((p.x - e[0]) * dx + (p.y - e[1]) * dy) / len2).clamp(0.0, 1.0);
        }
    }
    let pre = |c: [f32; 4]| [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]];
    let (a, b) = (pre(pt.c0), pre(pt.c1));
    let lab = [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t);
    if lab[3] <= 0.0 {
        return [0.0; 4];
    }
    let rgb = oklab_to_linear([lab[0] / lab[3], lab[1] / lab[3], lab[2] / lab[3]]).map(|v| v.clamp(0.0, 1.0));
    [rgb[0] * lab[3], rgb[1] * lab[3], rgb[2] * lab[3], lab[3]]
}

/// The shader's `sd_rrect`: `rect` is x, y, w, h; radii TL, TR, BR, BL.
fn sd_rrect(p: Point, rect: [f32; 4], radii: [f32; 4]) -> f32 {
    let (hx, hy) = (0.5 * rect[2], 0.5 * rect[3]);
    let (qx0, qy0) = (p.x - (rect[0] + hx), p.y - (rect[1] + hy));
    let r = match (qy0 < 0.0, qx0 >= 0.0) {
        (true, false) => radii[0],
        (true, true) => radii[1],
        (false, true) => radii[2],
        (false, false) => radii[3],
    };
    let r = r.clamp(0.0, hx.min(hy).max(0.0));
    let (qx, qy) = (qx0.abs() - hx + r, qy0.abs() - hy + r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// The shader's `sd_shape`: signed distance to the shape at `p`; `frag_x` is the pixel centre
/// in target pixels, for bands.
fn sd_shape(kind: u32, p: Point, g: [f32; 4], radii: [f32; 4], frag_x: f32, ends: (f32, f32)) -> f32 {
    match kind {
        KIND_CIRCLE => (p.x - g[0]).hypot(p.y - g[1]) - g[2],
        KIND_LINE => sd_segment(p, Point::new(g[0], g[1]), Point::new(g[2], g[3])),
        KIND_BAND => sd_band(p, g, radii, frag_x, ends),
        _ => sd_rrect(p, g, radii),
    }
}

/// The shader's error function: the 4-term approximation (max error about 5e-4).
fn erf(x: f32) -> f32 {
    let a = x.abs();
    let y = 1.0 + (0.278_393 + (0.230_389 + 0.078_108 * (a * a)) * a) * a;
    let y = y * y;
    x.signum() * (1.0 - 1.0 / (y * y))
}

fn gaussian(x: f32, sigma: f32) -> f32 {
    (-(x * x) / (2.0 * sigma * sigma)).exp() / (2.506_628_3 * sigma)
}

/// The shader's `box_shadow_x`: one row of a blurred rounded box, centred on 0.
fn box_shadow_x(x: f32, y: f32, sigma: f32, corner: f32, half: [f32; 2]) -> f32 {
    let delta = (half[1] - corner - y.abs()).min(0.0);
    let curved = half[0] - corner + (corner * corner - delta * delta).max(0.0).sqrt();
    let k = std::f32::consts::FRAC_1_SQRT_2 / sigma;
    0.5 * (erf((x + curved) * k) - erf((x - curved) * k))
}

/// The shader's `box_shadow`: a Gaussian blur of a rounded box at `p`, exact in x, four
/// samples in y.
fn box_shadow(rect: [f32; 4], corner: f32, p: Point, sigma: f32) -> f32 {
    let half = [0.5 * rect[2], 0.5 * rect[3]];
    let (qx, qy) = (p.x - (rect[0] + half[0]), p.y - (rect[1] + half[1]));
    let (lo, hi) = (qy - half[1], qy + half[1]);
    let start = (-3.0 * sigma).clamp(lo, hi);
    let end = (3.0 * sigma).clamp(lo, hi);
    let step = (end - start) / 4.0;
    (0..4)
        .map(|i| {
            let y = start + (i as f32 + 0.5) * step;
            box_shadow_x(qx, qy - y, sigma, corner, half) * gaussian(y, sigma) * step
        })
        .sum()
}

/// The effect path's tone map (`fs_final`): a shoulder above 0.95 of the brightest straight
/// channel brings light above 1 back under 1 with its hue kept.
fn tone_map(c: &mut [f32; 4]) {
    if c[3] <= 0.0 {
        *c = [0.0; 4];
        return;
    }
    c[3] = c[3].min(1.0);
    let k = 0.95;
    let m = c[0].max(c[1]).max(c[2]) / c[3].max(1e-4);
    if m > k {
        let s = (k + (1.0 - k) * (1.0 - (-(m - k) / (1.0 - k)).exp())) / m;
        for v in &mut c[..3] {
            *v *= s;
        }
    }
}

/// The bloom of an offscreen layer, if it has one that shows.
fn bloom_of(layer: &vo_scene::display::Layer) -> Option<Bloom> {
    match layer.target {
        LayerTarget::Offscreen { bloom: Some(b) } if b.strength > 0.0 => Some(b),
        _ => None,
    }
}

/// A linear premultiplied image for the bloom pyramid.
struct Img {
    w: u32,
    h: u32,
    px: Vec<[f32; 4]>,
}

impl Img {
    fn new(w: u32, h: u32) -> Img {
        Img { w, h, px: vec![[0.0; 4]; (w * h) as usize] }
    }

    /// A bilinear read at `(u, v)` in 0..1, clamped to the edge, as the GPU's sampler reads.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let (x, y) = (u * self.w as f32 - 0.5, v * self.h as f32 - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |x: f32, y: f32| {
            let x = (x as i32).clamp(0, self.w as i32 - 1) as u32;
            let y = (y as i32).clamp(0, self.h as i32 - 1) as u32;
            self.px[(y * self.w + x) as usize]
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        [0, 1, 2, 3].map(|i| {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            top + (bottom - top) * fy
        })
    }
}

/// `bloom.wgsl`'s pyramid on the CPU: `levels` 13-tap downsamples (a Karis average on the
/// first), then tent upsamples that mix each level with the one below, the deepest by `top`.
/// The result is at half the size of `src`.
fn pyramid(src: Img, levels: u32, top: f32) -> Img {
    let mut down: Vec<Img> = Vec::new();
    for l in 0..levels {
        let s = if l == 0 { &src } else { &down[l as usize - 1] };
        down.push(downsample(s, l == 0));
    }
    let n = levels as usize;
    if n == 1 {
        return down.pop().expect("one level");
    }
    let mut up: Option<Img> = None;
    for l in (0..n - 1).rev() {
        let low = up.as_ref().unwrap_or(&down[n - 1]);
        let weight = if l == n - 2 { BLOOM_MIX * top } else { BLOOM_MIX };
        up = Some(upsample(&down[l], low, weight));
    }
    up.expect("two levels or more")
}

fn downsample(src: &Img, karis: bool) -> Img {
    let mut dst = Img::new((src.w / 2).max(1), (src.h / 2).max(1));
    let (tx, ty) = (1.0 / src.w as f32, 1.0 / src.h as f32);
    let luma = |c: [f32; 4]| 1.0 / (1.0 + 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]);
    for y in 0..dst.h {
        for x in 0..dst.w {
            let (u, v) = ((x as f32 + 0.5) / dst.w as f32, (y as f32 + 0.5) / dst.h as f32);
            let tap = |dx: f32, dy: f32| src.sample(u + dx * tx, v + dy * ty);
            let box4 = |p: [[f32; 2]; 4]| {
                let t = p.map(|[dx, dy]| tap(dx, dy));
                [0, 1, 2, 3].map(|i| 0.25 * (t[0][i] + t[1][i] + t[2][i] + t[3][i]))
            };
            let boxes = [
                box4([[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]]),
                box4([[-2.0, -2.0], [0.0, -2.0], [-2.0, 0.0], [0.0, 0.0]]),
                box4([[0.0, -2.0], [2.0, -2.0], [0.0, 0.0], [2.0, 0.0]]),
                box4([[-2.0, 0.0], [0.0, 0.0], [-2.0, 2.0], [0.0, 2.0]]),
                box4([[0.0, 0.0], [2.0, 0.0], [0.0, 2.0], [2.0, 2.0]]),
            ];
            let weights = [0.5, 0.125, 0.125, 0.125, 0.125];
            let w = weights.iter().zip(&boxes).map(|(w, b)| if karis { w * luma(*b) } else { *w });
            let w: Vec<f32> = w.collect();
            let total: f32 = w.iter().sum();
            dst.px[(y * dst.w + x) as usize] =
                [0, 1, 2, 3].map(|i| boxes.iter().zip(&w).map(|(b, w)| b[i] * w).sum::<f32>() / total);
        }
    }
    dst
}

fn upsample(here: &Img, low: &Img, weight: f32) -> Img {
    let mut dst = Img::new(here.w, here.h);
    let (tx, ty) = (1.0 / low.w as f32, 1.0 / low.h as f32);
    for y in 0..dst.h {
        for x in 0..dst.w {
            let (u, v) = ((x as f32 + 0.5) / dst.w as f32, (y as f32 + 0.5) / dst.h as f32);
            let mut s = [0.0f32; 4];
            for (dx, dy, k) in [
                (0.0, 0.0, 4.0),
                (-1.0, 0.0, 2.0),
                (1.0, 0.0, 2.0),
                (0.0, -1.0, 2.0),
                (0.0, 1.0, 2.0),
                (-1.0, -1.0, 1.0),
                (1.0, -1.0, 1.0),
                (-1.0, 1.0, 1.0),
                (1.0, 1.0, 1.0),
            ] {
                let c = low.sample(u + dx * tx, v + dy * ty);
                for i in 0..4 {
                    s[i] += c[i] * k;
                }
            }
            let h = here.px[(y * dst.w + x) as usize];
            dst.px[(y * dst.w + x) as usize] = [0, 1, 2, 3].map(|i| h[i] + (s[i] / 16.0 - h[i]) * weight);
        }
    }
    dst
}

fn sd_segment(p: Point, a: Point, b: Point) -> f32 {
    let (pax, pay) = (p.x - a.x, p.y - a.y);
    let (bax, bay) = (b.x - a.x, b.y - a.y);
    let h = ((pax * bax + pay * bay) / (bax * bax + bay * bay).max(1e-12)).clamp(0.0, 1.0);
    (pax - bax * h).hypot(pay - bay * h)
}

fn clip_coverage(clip: Option<&Clip>, frag: Point) -> f32 {
    let Some(c) = clip else { return 1.0 };
    let p = c.from_px.apply(frag);
    let r = c.rect.rect;
    let d = sd_rrect(p, [r.x, r.y, r.w, r.h], c.rect.radii) * c.ppu;
    // The shader's edge fades: a smoothstep over the fade width from each edge.
    let edges = [p.x - r.x, p.y - r.y, r.x + r.w - p.x, r.y + r.h - p.y];
    let fade: f32 = edges
        .iter()
        .zip(c.fade)
        .map(|(e, f)| {
            if f <= 0.0 {
                return 1.0;
            }
            let x = (e / f).clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        })
        .product();
    (0.5 - d).clamp(0.0, 1.0) * fade
}

struct PathPen(tiny_skia::PathBuilder);

impl OutlinePen for PathPen {
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

/// A glyph's unhinted outline in font units, y up.
fn glyph_path(face: &Face, glyph: u32) -> Option<tiny_skia::Path> {
    let font = face.skrifa();
    let outline = font.outline_glyphs().get(GlyphId::new(glyph))?;
    let mut pen = PathPen(tiny_skia::PathBuilder::new());
    outline.draw(DrawSettings::unhinted(Size::unscaled(), face.location()), &mut pen).ok()?;
    pen.0.finish()
}

/// Masks (FX04) are not drawn here. A mask layer is never drawn, and a layer under an alpha
/// mask is hidden for the frame rather than shown unmasked; one under an inverted mask draws
/// unmasked.
fn hidden_by_mask(list: &DisplayList, li: u32) -> bool {
    if matches!(list.layers[li as usize].target, LayerTarget::Mask(_)) {
        return true;
    }
    list.layer_items(li)
        .iter()
        .filter(|i| i.kind == ItemKind::Layer)
        .any(|i| matches!(list.layers[i.start as usize].target, LayerTarget::Mask(m) if !m.invert))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::display::{Layer, Paint, ShapeInstance};
    use vo_scene::geom::{Color, Rect, Size as LSize};

    fn list_with(shape: ShapeInstance, size: LSize) -> DisplayList {
        let mut list = DisplayList { size, scale: 1.0, ..Default::default() };
        list.shapes.push(shape);
        list.items.push(vo_scene::display::Item { kind: ItemKind::Shapes, start: 0, len: 1 });
        let mut root = Layer::root();
        root.items = 0..1;
        list.layers.push(root);
        list
    }

    #[test]
    fn a_solid_rect_fills_its_pixels_and_nothing_else() {
        let r = RRect::new(Rect::new(2.0, 2.0, 4.0, 4.0), 0.0);
        let list =
            list_with(ShapeInstance::rrect(r, Paint::solid(Color::rgba(1.0, 0.0, 0.0, 1.0))), LSize::new(8.0, 8.0));
        let mut cpu = CpuRenderer::new(FontRegistry::new());
        let mut out = vec![0u8; 8 * 8 * 4];
        cpu.draw_bgra(&list, 1.0, 8, 8, 32, &mut out);
        let px = |x: usize, y: usize| &out[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
        assert_eq!(px(3, 3), [0, 0, 255, 255]);
        assert_eq!(px(0, 0), [0, 0, 0, 0]);
        assert_eq!(px(6, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn half_alpha_is_encoded_like_an_srgb_target() {
        // The GPU writes srgb(linear * a) to an sRGB swapchain, not srgb(colour) * a.
        let r = RRect::new(Rect::new(0.0, 0.0, 4.0, 4.0), 0.0);
        let list =
            list_with(ShapeInstance::rrect(r, Paint::solid(Color::rgba(1.0, 1.0, 1.0, 0.5))), LSize::new(4.0, 4.0));
        let mut cpu = CpuRenderer::new(FontRegistry::new());
        let mut out = vec![0u8; 4 * 4 * 4];
        cpu.draw_bgra(&list, 1.0, 4, 4, 16, &mut out);
        let want = (linear_to_srgb(0.5) * 255.0 + 0.5) as u8;
        assert_eq!(&out[20..24], &[want, want, want, 128]);
    }
}
