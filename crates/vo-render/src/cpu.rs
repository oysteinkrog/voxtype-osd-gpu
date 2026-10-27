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
//! - Not exact: offscreen layers draw as direct layers (group opacity per instance), only the
//!   nearest clip applies, and emitters, glows, shadows and bloom are skipped.

use std::collections::HashMap;
use std::sync::Arc;

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};
use vo_scene::display::{BlendMode, DisplayList, GlyphInstance, ItemKind, Paint, ShapeInstance};
use vo_scene::font::{Face, FontRegistry};
use vo_scene::geom::{Affine, Point, RRect};

use crate::color::{linear_premul, linear_to_srgb, oklab_to_linear};
use crate::shapes::{pixels_per_unit, GpuPaint, GpuShape};

const KIND_CIRCLE: u32 = vo_scene::display::ShapeKind::Circle as u32;
const KIND_LINE: u32 = vo_scene::display::ShapeKind::Line as u32;
const KIND_BAND: u32 = vo_scene::display::ShapeKind::Band as u32;
const PAINT_NONE: u32 = vo_scene::display::PaintKind::None as u32;
const PAINT_SOLID: u32 = vo_scene::display::PaintKind::Solid as u32;
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
        if opacity <= 0.0 {
            return;
        }
        let st = State { to_px, from_px, ppu, opacity, blend: layer.blend, clip };
        for item in list.layer_items(li) {
            let range = item.start as usize..(item.start + item.len) as usize;
            match item.kind {
                ItemKind::Shapes => {
                    for s in &list.shapes[range] {
                        if let Some(g) = GpuShape::new(s, &to_px, ppu, opacity) {
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
        // Glows and shadows are left to the GPU frames that follow.
        let fill = (s.kinds >> 8) & 0xff;
        if fill == vo_scene::display::PaintKind::Glow as u32 || fill == vo_scene::display::PaintKind::Shadow as u32 {
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
    outline.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).ok()?;
    pen.0.finish()
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
