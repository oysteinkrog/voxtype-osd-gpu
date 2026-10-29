//! Wet-ink stroke writing (SW-03): the pen follows the real letter's centre lines and stays
//! on its ink, and dots, crossbars and marks wait for the end of the word.

use vo_scene::element::TextStyle;
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Point};
use vo_scene::glyph_morph::WordOutline;
use vo_scene::stroke_write::{PenParams, PenSchedule, StrokeSource, WetInk, Writing};
use vo_scene::text::{ShapedLayout, TextLayout};

const FACES: [&str; 2] = ["grotesk", "extra-condensed-bold"];

fn writing(fonts: &FontRegistry, face: &str, text: &str, size: f32) -> Writing {
    let id = fonts.asset(face).expect("font asset");
    let shaped = ShapedLayout::new(fonts.clone()).layout(text, &TextStyle::new(id, size, 1.2 * size), None);
    let word = WordOutline::new(fonts, &shaped, size, Point::new(8.0, 1.2 * size));
    Writing::with_source(fonts, text, word, 0.15 * size, StrokeSource::FontNow)
}

/// Pixels per layer unit in the raster test.
const SS: f32 = 4.0;

/// A boolean raster of a box in layer units.
struct Raster {
    x0: f32,
    y0: f32,
    w: usize,
    h: usize,
    px: Vec<bool>,
}

impl Raster {
    fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Raster {
        let (w, h) = (((x1 - x0) * SS).ceil() as usize, ((y1 - y0) * SS).ceil() as usize);
        Raster { x0, y0, w, h, px: vec![false; w * h] }
    }

    fn centre(&self, i: usize, j: usize) -> Point {
        Point::new(self.x0 + (i as f32 + 0.5) / SS, self.y0 + (j as f32 + 0.5) / SS)
    }

    /// Nonzero scanline fill of closed polygons.
    fn fill(&mut self, polys: &[Vec<Point>]) {
        for j in 0..self.h {
            let y = self.centre(0, j).y;
            let mut xs: Vec<(f32, i32)> = Vec::new();
            for poly in polys {
                for k in 0..poly.len() {
                    let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
                    if (a.y <= y) != (b.y <= y) {
                        let x = a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x);
                        xs.push((x, if b.y > a.y { 1 } else { -1 }));
                    }
                }
            }
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            for k in 0..xs.len() {
                wind += xs[k].1;
                if wind != 0 && k + 1 < xs.len() {
                    for i in 0..self.w {
                        let x = self.centre(i, j).x;
                        if x >= xs[k].0 && x < xs[k + 1].0 {
                            self.px[j * self.w + i] = true;
                        }
                    }
                }
            }
        }
    }

    /// A round-capped segment whose radius runs from `ra` to `rb`.
    fn capsule(&mut self, a: Point, b: Point, ra: f32, rb: f32) {
        let r = ra.max(rb);
        let (lo_x, hi_x) = (a.x.min(b.x) - r, a.x.max(b.x) + r);
        let (lo_y, hi_y) = (a.y.min(b.y) - r, a.y.max(b.y) + r);
        let i0 = (((lo_x - self.x0) * SS).floor().max(0.0)) as usize;
        let i1 = (((hi_x - self.x0) * SS).ceil() as usize).min(self.w);
        let j0 = (((lo_y - self.y0) * SS).floor().max(0.0)) as usize;
        let j1 = (((hi_y - self.y0) * SS).ceil() as usize).min(self.h);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let l2 = (dx * dx + dy * dy).max(1e-9);
        for j in j0..j1 {
            for i in i0..i1 {
                let p = self.centre(i, j);
                let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0);
                let (qx, qy) = (a.x + t * dx - p.x, a.y + t * dy - p.y);
                let rr = ra + (rb - ra) * t;
                if qx * qx + qy * qy <= rr * rr {
                    self.px[j * self.w + i] = true;
                }
            }
        }
    }
}

/// Points along a quadratic segment.
fn quad(s: &vo_scene::path::VectorSeg, t: f32) -> Point {
    let u = 1.0 - t;
    Point::new(
        u * u * s.p0.x + 2.0 * u * t * s.p1.x + t * t * s.p2.x,
        u * u * s.p0.y + 2.0 * u * t * s.p1.y + t * t * s.p2.y,
    )
}

/// The share of the pen's pixels that fall outside the word's ink, the pen drawn at each
/// glyph's stem width with its width table, as the wet ink draws it.
fn outside_share(w: &Writing) -> (f32, usize) {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for g in &w.word.glyphs {
        x0 = x0.min(g.ink.x);
        y0 = y0.min(g.ink.y);
        x1 = x1.max(g.ink.x + g.ink.w);
        y1 = y1.max(g.ink.y + g.ink.h);
    }
    let pad = 10.0;
    let mut ink = Raster::new(x0 - pad, y0 - pad, x1 + pad, y1 + pad);
    let mut pen = Raster::new(x0 - pad, y0 - pad, x1 + pad, y1 + pad);
    let polys: Vec<Vec<Point>> = w
        .word
        .glyphs
        .iter()
        .flat_map(|g| g.outline.contours.iter())
        .map(|c| c.segs.iter().flat_map(|s| (0..8).map(move |k| quad(s, k as f32 / 8.0))).collect())
        .collect();
    ink.fill(&polys);
    for t in w.strokes() {
        let stem = w.stem(t.glyph).expect("centre lines");
        for s in &t.contour.segs {
            let n = 6;
            for k in 0..n {
                let (ta, tb) = (k as f32 / n as f32, (k + 1) as f32 / n as f32);
                let wa = s.w[0] + (s.w[1] - s.w[0]) * ta;
                let wb = s.w[0] + (s.w[1] - s.w[0]) * tb;
                pen.capsule(quad(s, ta), quad(s, tb), 0.5 * stem * wa, 0.5 * stem * wb);
            }
        }
    }
    let drawn = pen.px.iter().filter(|&&b| b).count();
    let out = pen.px.iter().zip(&ink.px).filter(|(p, i)| **p && !**i).count();
    (out as f32 / drawn.max(1) as f32, drawn)
}

#[test]
fn wet_ink_stays_on_the_letters_of_configuration() {
    let fonts = FontRegistry::new();
    for face in FACES {
        for size in [28.0, 44.0] {
            let w = writing(&fonts, face, "configuration", size);
            let (out, drawn) = outside_share(&w);
            eprintln!("{face} {size}: {:.2} % of {drawn} pen pixels outside the ink", 100.0 * out);
            assert!(drawn > 1000, "{face}: {drawn}");
            assert!(out <= 0.05, "{face} {size}: {:.3} of the pen outside the ink", out);
        }
    }
}

#[test]
fn dots_and_crossbars_of_tiff_come_after_every_base_stroke() {
    let fonts = FontRegistry::new();
    for face in FACES {
        let w = writing(&fonts, face, "tiff", 40.0);
        let strokes: Vec<_> = w.strokes().collect();
        let last_base = strokes.iter().filter(|t| !t.delayed).map(|t| t.start).fold(f32::MIN, f32::max);
        let delayed: Vec<_> = strokes.iter().filter(|t| t.delayed).collect();
        // The dot of the i, the bar of the t and the bar (or bars) of the f's.
        assert!(delayed.len() >= 3, "{face}: {} delayed of {}", delayed.len(), strokes.len());
        for t in &delayed {
            assert!(
                t.start > last_base,
                "{face}: delayed stroke of glyph {} at {} before {last_base}",
                t.glyph,
                t.start
            );
        }
        // Base strokes keep the word's order, glyph by glyph; so do the delayed ones.
        let base_glyphs: Vec<usize> = strokes.iter().filter(|t| !t.delayed).map(|t| t.glyph).collect();
        assert!(base_glyphs.windows(2).all(|p| p[0] <= p[1]), "{face}: {base_glyphs:?}");
        let late_glyphs: Vec<usize> = delayed.iter().map(|t| t.glyph).collect();
        assert!(late_glyphs.windows(2).all(|p| p[0] <= p[1]), "{face}: {late_glyphs:?}");
        // The i's dot is among them: a delayed stroke on the i's glyph, above the x-height.
        let i = w.word.glyphs.iter().position(|g| g.cluster == 1).expect("i glyph");
        assert!(delayed.iter().any(|t| t.glyph == i), "{face}: no delayed stroke on the i");
    }
}

#[test]
fn ring_of_a_and_slash_of_o_are_delayed() {
    let fonts = FontRegistry::new();
    for face in FACES {
        let text = "på økt";
        let w = writing(&fonts, face, text, 40.0);
        let glyph_of = |c: char| {
            let at = text.find(c).unwrap() as u32;
            w.word.glyphs.iter().position(|g| g.cluster == at).unwrap()
        };
        let (a, o) = (glyph_of('å'), glyph_of('ø'));
        let a_late: Vec<_> = w.strokes().filter(|t| t.glyph == a && t.delayed).collect();
        let o_late: Vec<_> = w.strokes().filter(|t| t.glyph == o && t.delayed).collect();
        assert!(!a_late.is_empty(), "{face}: the ring of å is not delayed");
        assert!(!o_late.is_empty(), "{face}: the slash of ø is not delayed");
        // The ring sits above the a's bowl.
        let g = &w.word.glyphs[a];
        let ring_bottom = a_late.iter().flat_map(|t| t.contour.resample(12)).map(|p| p.y).fold(f32::MIN, f32::max);
        assert!(ring_bottom < g.pos.y - 0.4 * g.size, "{face}: ring at {ring_bottom}, baseline {}", g.pos.y);
        // The slash reaches into a corner the o's ring leaves empty: lower left or upper
        // right of the ink box (a heavy face shows only its ends there).
        let g = &w.word.glyphs[o];
        let (cx, cy) = (g.ink.x + 0.5 * g.ink.w, g.ink.y + 0.5 * g.ink.h);
        let cornered = o_late.iter().flat_map(|t| t.contour.resample(12)).any(|p| {
            let (dx, dy) = ((p.x - cx) / g.ink.w, (p.y - cy) / g.ink.h);
            dx * dy < 0.0 && dx.abs() > 0.15 && dy.abs() > 0.3
        });
        let pts: Vec<_> = o_late
            .iter()
            .flat_map(|t| t.contour.resample(4))
            .map(|p| ((p.x - cx) / g.ink.w, (p.y - cy) / g.ink.h))
            .collect();
        assert!(cornered, "{face}: the delayed strokes of ø are not its slash: {pts:?}");
        let last_base = w.strokes().filter(|t| !t.delayed).map(|t| t.start).fold(f32::MIN, f32::max);
        assert!(w.strokes().filter(|t| t.delayed).all(|t| t.start > last_base));
    }
}

#[test]
fn wet_ink_dries_after_its_glyph_and_the_pen_is_the_accent() {
    let fonts = FontRegistry::new();
    let w = writing(&fonts, "grotesk", "ink", 40.0);
    let s = PenSchedule::plan(PenParams::default(), &[(1.0, w.length())]);
    let slot = s.slots()[0];
    let text = Color::hex(0x808080, 1.0);
    let wet = WetInk::new(text, Color::hex(0xf59e0b, 1.0));
    // 15 % toward white.
    assert!((wet.ink.r - (0.5019608 + 0.15 * (1.0 - 0.5019608))).abs() < 1e-4);
    assert!(wet.nib >= 4.0 && wet.nib <= 6.0);
    // Nothing is wet before the pen; each stroke is wet when written and dry 180 ms after its
    // glyph is done; all is dry 180 ms after the word.
    let n = w.strokes().count();
    assert!(w.wetness(&slot, slot.start - 0.01, &wet).iter().all(|a| *a == 0.0));
    let mid = w.wetness(&slot, 0.5 * (slot.start + slot.end), &wet);
    assert_eq!(mid.len(), n);
    assert!(mid[0] > 0.0);
    let end = w.wetness(&slot, slot.end, &wet);
    assert!(end.last().copied().unwrap_or(0.0) > 0.9, "{end:?}");
    assert!(w.wetness(&slot, slot.end + wet.dry_secs + 1e-3, &wet).iter().all(|a| *a == 0.0));
    assert!(wet.until(&slot) >= slot.end + wet.dry_secs);
    // Wetness only falls once written.
    let mut last = vec![0.0f32; n];
    let mut peak = vec![false; n];
    for k in 0..=200 {
        let t = slot.start + (slot.end + 0.25 - slot.start) * k as f64 / 200.0;
        let a = w.wetness(&slot, t, &wet);
        for i in 0..n {
            if peak[i] {
                assert!(a[i] <= last[i] + 1e-6, "stroke {i} wetter again at {t}");
            }
            if a[i] >= 1.0 {
                peak[i] = true;
            }
        }
        last = a;
    }
    assert!(peak.iter().all(|p| *p));
}
