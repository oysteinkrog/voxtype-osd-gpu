//! Effect bounds for the `effects` overlay: where each effect draws and how far it reaches,
//! so a glow cut off at the surface edge or an offscreen texture far larger than its content
//! shows at a glance.
//!
//! Two sources. The renderer's report of the last frame gives every offscreen layer: its
//! content, its texture and its bloom's reach, with the GPU time of its passes. The display
//! list gives the effects drawn in the shape and glyph shaders, which have no surface of
//! their own: glow and drop-shadow paints and glyph halos and blurs.

use vo_render::profile::{FrameReport, FrameTimings, Section, SurfaceKind};
use vo_scene::display::{DisplayList, ItemKind, PaintKind, ShapeInstance, ShapeKind};
use vo_scene::geom::{Affine, Point, Rect, Size};

/// One effect's extent, in the scene's logical units.
#[derive(Clone, Debug, PartialEq)]
pub struct FxBox {
    /// What the effect is drawn from: the content of the layer, the shape, the glyphs.
    pub content: Rect,
    /// How far it reaches.
    pub reach: Rect,
    pub label: String,
    /// The reach leaves the surface: the effect is cut off at the edge.
    pub clipped: bool,
    /// An offscreen layer (drawn in its own texture) rather than a shader effect.
    pub surface: bool,
}

/// How far a glow paint reaches: where `exp(-d / radius) * strength` drops under one 8-bit
/// step.
pub fn glow_reach(radius: f32, strength: f32) -> f32 {
    (radius * (255.0 * strength).max(1.0).ln()).max(0.0)
}

/// A shape's own box in layer space, stroke included.
fn shape_box(s: &ShapeInstance) -> Rect {
    let [a, b, c, d] = s.geom;
    let half = 0.5 * s.stroke_width;
    let r = match s.kind {
        ShapeKind::Circle => Rect::new(a - c, b - c, 2.0 * c, 2.0 * c),
        ShapeKind::Line => {
            let (x0, y0, x1, y1) = (a.min(c), b.min(d), a.max(c), b.max(d));
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        }
        ShapeKind::Band => {
            let h = s.radii[0].max(s.radii[1]);
            Rect::new(a.min(c), b - h, (c - a).abs(), 2.0 * h)
        }
        _ => Rect::new(a, b, c, d),
    };
    inflate(r, half)
}

fn inflate(r: Rect, by: f32) -> Rect {
    Rect::new(r.x - by, r.y - by, r.w + 2.0 * by, r.h + 2.0 * by)
}

fn union(a: Option<Rect>, b: Rect) -> Rect {
    match a {
        None => b,
        Some(a) => {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            Rect::new(x0, y0, a.right().max(b.right()) - x0, a.bottom().max(b.bottom()) - y0)
        }
    }
}

fn map(m: &Affine, r: Rect) -> Rect {
    let pts = [(r.x, r.y), (r.right(), r.y), (r.x, r.bottom()), (r.right(), r.bottom())]
        .map(|(x, y)| m.apply(Point::new(x, y)));
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// How much a uniform scale `m` applies, for lengths.
fn length_scale(m: &Affine) -> f32 {
    ((m.a * m.a + m.b * m.b).sqrt() + (m.c * m.c + m.d * m.d).sqrt()) * 0.5
}

fn outside(r: Rect, size: Size) -> bool {
    r.x < -0.5 || r.y < -0.5 || r.right() > size.w + 0.5 || r.bottom() > size.h + 0.5
}

/// Glows, drop shadows and glyph halos and blurs in `list`, in surface units. Glyph effects
/// are merged per layer, since a word has many glyphs.
pub fn shader_effects(list: &DisplayList) -> Vec<FxBox> {
    let mut out = Vec::new();
    let fx_of = |i: u32| list.glyph_fx.binary_search_by_key(&i, |(g, _)| *g).ok().map(|k| list.glyph_fx[k].1);
    for layer in &list.layers {
        let m = layer.transform;
        let k = length_scale(&m);
        let mut text: Option<(Rect, Rect, f32)> = None;
        for item in &list.items[layer.items.start as usize..layer.items.end as usize] {
            let range = item.start as usize..(item.start + item.len) as usize;
            match item.kind {
                ItemKind::Shapes => {
                    for s in &list.shapes[range] {
                        let (reach, what) = match s.fill.kind {
                            PaintKind::Glow => (glow_reach(s.fill.p0.x, s.fill.p0.y), "glow"),
                            PaintKind::Shadow => (
                                3.0 * s.fill.p1.x + s.fill.p1.y.max(0.0) + s.fill.p0.x.abs().max(s.fill.p0.y.abs()),
                                "shadow",
                            ),
                            _ => continue,
                        };
                        if !s.fill.is_visible() || reach < 0.5 {
                            continue;
                        }
                        let own = shape_box(s);
                        let content = map(&m, own);
                        let mut far = inflate(own, reach);
                        if s.fill.kind == PaintKind::Shadow {
                            far = inflate(
                                Rect::new(own.x + s.fill.p0.x, own.y + s.fill.p0.y, own.w, own.h),
                                3.0 * s.fill.p1.x + s.fill.p1.y.max(0.0),
                            );
                            far = union(Some(far), own);
                        }
                        let reach_rect = map(&m, far);
                        out.push(FxBox {
                            content,
                            reach: reach_rect,
                            label: format!("{what} {:.0}", reach * k),
                            clipped: outside(reach_rect, list.size),
                            surface: false,
                        });
                    }
                }
                ItemKind::Glyphs => {
                    for gi in range {
                        let Some(fx) = fx_of(gi as u32) else { continue };
                        let halo = if fx.halo.a > 0.0 { fx.halo_radius } else { 0.0 };
                        let reach = halo.max(fx.blur).max(fx.chroma.abs());
                        if reach <= 0.0 {
                            continue;
                        }
                        let g = &list.glyphs[gi];
                        let own = Rect::new(g.pos.x, g.pos.y - 0.8 * g.size, 0.62 * g.size, g.size);
                        let (c, r, most) = text.unwrap_or((own, own, 0.0));
                        text = Some((union(Some(c), own), union(Some(r), inflate(own, reach)), most.max(reach)));
                    }
                }
                _ => {}
            }
        }
        if let Some((c, r, most)) = text {
            let reach_rect = map(&m, r);
            out.push(FxBox {
                content: map(&m, c),
                reach: reach_rect,
                label: format!("text fx {:.0}", most * k),
                clipped: outside(reach_rect, list.size),
                surface: false,
            });
        }
    }
    out
}

/// Offscreen layers from the renderer's report, in surface units (`scale` pixels each).
pub fn surface_effects(report: &FrameReport, scale: f32, size: Size, timings: Option<&FrameTimings>) -> Vec<FxBox> {
    let to_units = |b: [f32; 4]| Rect::new(b[0] / scale, b[1] / scale, (b[2] - b[0]) / scale, (b[3] - b[1]) / scale);
    let ms = |want: Section| timings.and_then(|t| t.passes.iter().find(|p| p.section == want)).map(|p| p.ms);
    report
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s.kind, SurfaceKind::Layer) && s.texture[2] > s.texture[0])
        .map(|(i, s)| {
            let content = to_units(s.content);
            let reach = match s.bloom {
                Some(b) => inflate(content, b.reach / scale),
                None => content,
            };
            let mut what: Vec<String> = Vec::new();
            if let Some(b) = s.bloom {
                what.push(format!("bloom {}x {:.0}", b.levels, b.reach / scale));
            }
            if s.trail {
                what.push("trail".into());
            }
            if s.style {
                what.push("style".into());
            }
            if s.masked {
                what.push("mask".into());
            }
            if what.is_empty() {
                what.push("offscreen".into());
            }
            let tex = [(s.texture[2] - s.texture[0]) as u32, (s.texture[3] - s.texture[1]) as u32];
            let mut label = format!("{} {} {}x{}", s.label(), what.join(" "), tex[0], tex[1]);
            let gpu =
                ms(Section::Draw { surface: i }).unwrap_or(0.0) + ms(Section::Bloom { surface: i }).unwrap_or(0.0);
            if timings.is_some() {
                label.push_str(&format!(" {gpu:.2} ms"));
            }
            FxBox { content, reach, label, clipped: s.bloom.is_some() && outside(reach, size), surface: true }
        })
        .collect()
}

/// Lines for the stats overlay: the GPU time of each section, slowest first.
pub fn timing_lines(t: &FrameTimings, max: usize) -> Vec<String> {
    let mut passes: Vec<_> = t.passes.iter().collect();
    passes.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    let mut out = vec![format!("GPU {:.3} ms in {} sections", t.total_ms, t.passes.len())];
    for p in passes.iter().take(max) {
        out.push(format!("  {:<22} {:.3} ms", p.label, p.ms));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_render::profile::{BloomReport, SurfaceReport};
    use vo_scene::display::{Item, Layer, Paint};
    use vo_scene::geom::{Color, RRect};

    #[test]
    fn glow_reach_follows_the_falloff() {
        // exp(-d / 10) drops to 1/255 at d = 10 ln 255.
        assert!((glow_reach(10.0, 1.0) - 55.4).abs() < 0.1);
        assert!(glow_reach(10.0, 4.0) > glow_reach(10.0, 1.0));
        assert_eq!(glow_reach(10.0, 0.0), 0.0);
    }

    fn list(shapes: Vec<ShapeInstance>, transform: Affine) -> DisplayList {
        let n = shapes.len() as u32;
        DisplayList {
            size: Size::new(200.0, 100.0),
            scale: 1.0,
            layers: vec![Layer { transform, items: 0..1, ..Layer::root() }],
            items: vec![Item { kind: ItemKind::Shapes, start: 0, len: n }],
            shapes,
            ..DisplayList::default()
        }
    }

    #[test]
    fn a_glow_near_the_edge_is_clipped_and_one_in_the_middle_is_not() {
        let glow = |x: f32| {
            ShapeInstance::rrect(RRect::new(Rect::new(x, 40.0, 20.0, 20.0), 4.0), Paint::glow(Color::WHITE, 4.0, 1.0))
        };
        let plain =
            ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, 200.0, 100.0), 0.0), Paint::solid(Color::WHITE));
        let fx = shader_effects(&list(vec![glow(90.0), glow(2.0), plain], Affine::IDENTITY));
        assert_eq!(fx.len(), 2, "the solid fill is no effect");
        assert!(!fx[0].clipped && fx[1].clipped);
        let r = glow_reach(4.0, 1.0);
        assert!((fx[0].reach.x - (90.0 - r)).abs() < 1e-3 && (fx[0].reach.w - (20.0 + 2.0 * r)).abs() < 1e-3);
        assert!(fx[0].label.starts_with("glow 22"), "{}", fx[0].label);
        // A layer transform moves and scales the boxes and the reach.
        let fx = shader_effects(&list(vec![glow(10.0)], Affine::scale(2.0)));
        assert!((fx[0].content.x - 20.0).abs() < 1e-3 && fx[0].label.starts_with("glow 44"));
    }

    #[test]
    fn a_shadow_reaches_toward_its_offset() {
        let s = ShapeInstance::rrect(
            RRect::new(Rect::new(50.0, 40.0, 20.0, 20.0), 0.0),
            Paint::shadow(Color::BLACK, Point::new(0.0, 10.0), 2.0, 0.0),
        );
        let fx = shader_effects(&list(vec![s], Affine::IDENTITY));
        assert_eq!(fx.len(), 1);
        assert!((fx[0].reach.bottom() - (60.0 + 10.0 + 6.0)).abs() < 1e-3, "{:?}", fx[0].reach);
        assert!((fx[0].reach.y - 40.0).abs() < 1e-3, "the shape itself stays inside the reach");
    }

    #[test]
    fn surfaces_report_bloom_reach_and_time() {
        let report = FrameReport {
            effects: true,
            size: [400, 200],
            surfaces: vec![
                SurfaceReport {
                    kind: SurfaceKind::Scene,
                    layer: None,
                    content: [0.0, 0.0, 400.0, 200.0],
                    texture: [0.0, 0.0, 400.0, 200.0],
                    bloom: None,
                    trail: false,
                    style: false,
                    masked: false,
                    draws: 2,
                },
                SurfaceReport {
                    kind: SurfaceKind::Layer,
                    layer: Some(3),
                    content: [10.0, 20.0, 110.0, 60.0],
                    texture: [-38.0, -28.0, 154.0, 100.0],
                    bloom: Some(BloomReport { levels: 4, strength: 1.0, reach: 48.0 }),
                    trail: false,
                    style: false,
                    masked: false,
                    draws: 1,
                },
            ],
            ..FrameReport::default()
        };
        let timings = FrameTimings {
            passes: vec![
                vo_render::profile::PassTime {
                    section: Section::Draw { surface: 1 },
                    layer: Some(3),
                    label: "layer 3".into(),
                    ms: 0.05,
                },
                vo_render::profile::PassTime {
                    section: Section::Bloom { surface: 1 },
                    layer: Some(3),
                    label: "layer 3 bloom".into(),
                    ms: 0.1,
                },
            ],
            total_ms: 0.2,
        };
        // At scale 2 the 400x200 pixel target is a 200x100 surface.
        let fx = surface_effects(&report, 2.0, Size::new(200.0, 100.0), Some(&timings));
        assert_eq!(fx.len(), 1, "surface 0 is the frame, not an effect");
        assert_eq!(fx[0].content, Rect::new(5.0, 10.0, 50.0, 20.0));
        assert!(fx[0].clipped, "48 px of bloom at scale 2 is 24 units, past the top edge at y 10");
        assert_eq!(fx[0].label, "layer 3 bloom 4x 24 192x128 0.15 ms");
        let lines = timing_lines(&timings, 5);
        assert!(lines[1].contains("layer 3 bloom") && lines[0].contains("0.200"));
    }
}
