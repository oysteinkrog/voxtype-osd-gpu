//! Pen strokes from a real glyph (EG-29): the centre lines a hand would draw to write the
//! letter, in the order it would draw them, with the stem width to draw them at.
//!
//! [`analyse`] fills the glyph outline on a grid of [`GRID`] pixels per em (a small nonzero
//! scanline fill), thins the ink to a one pixel skeleton (Zhang-Suen), and traces the
//! skeleton into polylines:
//!
//! - short spurs (the forks thinning leaves at square corners) are pruned, and junctions a
//!   few pixels apart are merged into one;
//! - at a junction the branches that run on most nearly straight are joined, so a `T`'s bar
//!   is one stroke and an `H`'s stems run through their crossbar;
//! - each polyline is simplified and smoothed (two rounds of Chaikin corner cutting).
//!
//! The strokes come out in pen order: top before bottom, left before right, a horizontal
//! before a vertical that starts on the same line; each open stroke runs left to right when
//! it is wider than tall and top to bottom otherwise, and a closed stroke starts at its top
//! and runs anticlockwise. Delayed strokes come last: marks (a part of the glyph's ink apart
//! from its body and above the x-height or below the baseline, like the dot of `i` or the
//! ring of `å`) and crossbars (a stroke that crosses a longer one, like the bar of `t` or `f`,
//! or crosses a closed stroke, like the slash of `ø`).
//!
//! The stem width is the median width along the skeleton. [`Coverage`] says how well the
//! strokes, drawn round-capped at that width, cover the glyph's ink, so a caller can fall
//! back to another treatment for a glyph that does not reduce to strokes (an emoji, a
//! complex CJK glyph).
//!
//! The work is a fraction of a millisecond per glyph, still too much for a frame that first
//! shows a word. [`lookup`] answers from a cache keyed by the glyph's outline, and on a miss
//! queues the glyph on one worker thread and says [`Lookup::Pending`]; [`prefetch`] queues a
//! whole word ahead of time; [`get_now`] computes on the calling thread (offline renders,
//! tests). Results are the same on any thread: the analysis uses no clock and no randomness,
//! so the same outline gives bit-identical strokes.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use kurbo::{BezPath, PathEl};

use crate::font::{Face, FontRegistry};
use crate::geom::Point;
use crate::glyph_morph::WordOutline;

/// Grid pixels per em the analysis runs at.
pub const GRID: f32 = 96.0;

/// The fraction of the glyph's ink the strokes must cover, and the most of the strokes that
/// may fall outside the ink, for [`Coverage::passes`].
pub const MIN_COVERED: f32 = 0.95;
pub const MAX_OUTSIDE: f32 = 0.05;
/// The share of the ink a pen's strokes must cover to write the glyph ([`Coverage::writes`]).
pub const WRITE_MIN_COVERED: f32 = 0.9;
/// The share of a pen's strokes that may fall outside the ink ([`Coverage::writes`]).
pub const WRITE_MAX_OUTSIDE: f32 = 0.1;

/// One pen stroke, in em units with the pen origin at 0 and y down (multiply by the em size
/// and add the glyph's pen position to place it).
#[derive(Clone, Debug, PartialEq)]
pub struct CentreStroke {
    /// The polyline in drawing order. A closed stroke does not repeat its first point.
    pub points: Vec<Point>,
    pub closed: bool,
    /// A dot, mark or crossbar: drawn after the glyph's base strokes, and a writer may hold
    /// it to the end of the word.
    pub delayed: bool,
    /// The ink's width at each point, in em.
    pub widths: Vec<f32>,
}

/// How the strokes, stroked round-capped at the stem width, match the glyph's ink.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Coverage {
    /// Fraction of the glyph's ink pixels the strokes cover.
    pub covered: f32,
    /// Fraction of the stroke pixels outside the glyph's ink.
    pub outside: f32,
}

impl Coverage {
    /// Whether the strokes stand for the glyph: at least [`MIN_COVERED`] of its ink covered,
    /// at most [`MAX_OUTSIDE`] of the strokes outside it.
    pub fn passes(&self) -> bool {
        self.covered >= MIN_COVERED && self.outside <= MAX_OUTSIDE
    }

    /// Whether a pen may write the glyph by these strokes (SW-07): at least
    /// [`WRITE_MIN_COVERED`] of its ink covered, at most [`WRITE_MAX_OUTSIDE`] outside it.
    /// Looser than [`Coverage::passes`], since wet ink dries into the type within a fifth of
    /// a second: the near misses (grotesk `ø` at 6 % outside, Saira ExtraBold `w` at 93 %)
    /// are still written, and only analyses that miss the letter are swept instead.
    pub fn writes(&self) -> bool {
        self.covered >= WRITE_MIN_COVERED && self.outside <= WRITE_MAX_OUTSIDE
    }
}

/// A glyph's pen strokes. See the module docs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlyphCentre {
    /// In pen order: base strokes, then delayed strokes.
    pub strokes: Vec<CentreStroke>,
    /// The stem width in em.
    pub stem: f32,
    pub coverage: Coverage,
}

impl GlyphCentre {
    /// The number of base strokes; the delayed ones follow them.
    pub fn base_count(&self) -> usize {
        self.strokes.iter().take_while(|s| !s.delayed).count()
    }
}

/// The analysis of one outline, given in grid pixels (`GRID` per em, y down, baseline at 0),
/// with the x-height in em. The result is in em.
pub fn analyse(path: &BezPath, x_height: f32) -> GlyphCentre {
    let Some(grid) = Grid::fill(path, 1.0) else { return GlyphCentre::default() };
    let (base, stem_px) = trace(&grid);
    // The first fit that passes, else the one closest to passing.
    let mut best: Option<(f32, Vec<Raw>, Coverage)> = None;
    for (cap_out, widen) in FITS {
        let strokes = fit(&grid, base.clone(), stem_px, x_height, cap_out, widen);
        let cov = coverage(path, &strokes, None);
        let score = (cov.covered - MIN_COVERED).min(MAX_OUTSIDE - cov.outside);
        if best.as_ref().is_none_or(|b| score > b.0) {
            best = Some((score, strokes, cov));
        }
        if cov.passes() {
            break;
        }
    }
    let Some((_, strokes, coverage)) = best else { return GlyphCentre::default() };
    let to_em = |p: Point| Point::new(p.x / GRID, p.y / GRID);
    let strokes = strokes
        .into_iter()
        .map(|s| CentreStroke {
            points: s.pts.into_iter().map(to_em).collect(),
            closed: s.closed,
            delayed: s.delayed,
            widths: s.w.into_iter().map(|w| if w > 0.0 { w } else { stem_px } / GRID).collect(),
        })
        .collect();
    GlyphCentre { strokes, stem: stem_px / GRID, coverage }
}

/// Thin, trace and centre on the outline: the strokes in grid pixels, with their widths,
/// and the stem width in pixels.
fn trace(grid: &Grid) -> (Vec<Raw>, f32) {
    let dist = grid.distance();
    let skel = grid.thin();
    let rough = stem_width(&skel, &dist, grid.w);
    let mut graph = Graph::trace(grid, &skel, &dist);
    graph.prune(rough);
    let mut strokes = graph.strokes(rough);
    let mut chords: Vec<f32> = Vec::new();
    for s in &mut strokes {
        s.w = centre_on_outline(&grid.edges, &mut s.pts, s.closed, rough);
        chords.extend(s.w.iter().copied().filter(|w| *w > 0.0));
    }
    let stem = if chords.is_empty() {
        rough
    } else {
        chords.sort_by(|a, b| a.total_cmp(b));
        chords[(STEM_RANK * chords.len() as f32) as usize]
    };
    for s in &mut strokes {
        fill_widths(&mut s.w, stem);
    }
    (strokes, stem)
}

/// Fits traced strokes to the ink with ends and corners whose pen may stand `cap_out` of
/// its radius out of the ink and a pen `widen` times the ink's width, then simplifies, tags
/// and orders them.
fn fit(grid: &Grid, mut strokes: Vec<Raw>, stem: f32, x_height: f32, cap_out: f32, widen: f32) -> Vec<Raw> {
    let r = 0.5 * stem;
    for s in &mut strokes {
        shape_ends(&grid.edges, s, r, cap_out);
        let keep = simplify(&s.pts, &s.w, s.closed);
        s.pts = keep.iter().map(|&i| s.pts[i]).collect();
        s.w = keep.iter().map(|&i| s.w[i]).collect();
        into_vertices(&grid.edges, s, r, cap_out);
        // The pen never gets wider than the ink's inscribed circle there, with the cap's
        // allowance, so no interpolated or end width stands far out of the letter.
        for (p, w) in s.pts.iter().zip(s.w.iter_mut()) {
            *w = (*w * widen).min(2.0 * (edge_dist(&grid.edges, *p) + cap_out * r)).max(1.0);
        }
        s.orient();
        s.measure();
    }
    tag_delayed(&mut strokes, grid, x_height * GRID, stem);
    order(&mut strokes, stem);
    strokes
}

/// The stem width is this rank of the ink's widths along the strokes: the median.
const STEM_RANK: f32 = 0.5;

/// The fits tried, in order: how far a free end's cap, or the pen in a sharp corner, may
/// stand out of the ink (a fraction of the pen radius), and how much wider than the ink the
/// pen is. Each glyph takes the first fit that passes [`Coverage::passes`], else the one
/// closest to passing, so a square terminal gets its corners filled only as far as its ink
/// allows.
const FITS: [(f32, f32); 6] = [(0.25, 1.0), (0.5, 1.0), (0.25, 1.08), (0.75, 1.0), (0.5, 1.08), (0.75, 1.08)];

/// Sharp turns run on into the ink's tip, as a pen writing a `v` goes down into its point.
/// The turn is measured between the arms a pen radius either side, so a corner the
/// smoothing rounded over several points still counts; of each run of sharp points the
/// sharpest moves out along its bisector while the pen still fits.
fn into_vertices(edges: &[(Point, Point)], s: &mut Raw, r: f32, cap_out: f32) {
    let n = s.pts.len();
    if n < 3 {
        return;
    }
    let need = r * (1.0 - cap_out);
    let pts = s.pts.clone();
    let unit = |a: Point, b: Point| {
        let d = Point::new(b.x - a.x, b.y - a.y);
        let l = (d.x * d.x + d.y * d.y).sqrt();
        if l < 1e-6 { Point::ZERO } else { Point::new(d.x / l, d.y / l) }
    };
    // The point an arc length `r` away from `i`, backwards or forwards; None past an open end.
    let arm = |i: usize, fwd: bool| -> Option<Point> {
        let (mut k, mut acc) = (i, 0.0);
        while acc < r {
            let next = match (fwd, s.closed) {
                (true, true) => (k + 1) % n,
                (false, true) => (k + n - 1) % n,
                (true, false) if k + 1 < n => k + 1,
                (false, false) if k > 0 => k - 1,
                _ => return None,
            };
            acc += dist2(pts[k], pts[next]).sqrt();
            k = next;
            if k == i {
                return None;
            }
        }
        Some(pts[k])
    };
    let turn: Vec<Option<(f32, Point)>> = (0..n)
        .map(|i| {
            let (a, b) = (arm(i, false)?, arm(i, true)?);
            let (u, v) = (unit(a, pts[i]), unit(pts[i], b));
            let dot = u.x * v.x + u.y * v.y;
            let out = unit(Point::ZERO, Point::new(u.x - v.x, u.y - v.y));
            (dot < -0.2 && out != Point::ZERO).then_some((dot, out))
        })
        .collect();
    let mut i = 0;
    while i < n {
        if turn[i].is_none() {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < n && turn[j + 1].is_some() {
            j += 1;
        }
        let k = (i..=j).min_by(|&a, &b| turn[a].unwrap().0.total_cmp(&turn[b].unwrap().0).then(a.cmp(&b))).unwrap_or(i);
        let out = turn[k].unwrap().1;
        let mut q = pts[k];
        for _ in 0..(4.0 * r / 0.25) as usize {
            let next = Point::new(q.x + out.x * 0.25, q.y + out.y * 0.25);
            if edge_dist(edges, next) < need {
                break;
            }
            q = next;
        }
        s.pts[k] = q;
        i = j + 1;
    }
}

/// Fills the widths the outline did not give (0) from the nearest known ones on each side,
/// or the stem when none is known.
fn fill_widths(w: &mut [f32], stem: f32) {
    let known: Vec<usize> = (0..w.len()).filter(|&i| w[i] > 0.0).collect();
    if known.is_empty() {
        w.fill(stem);
        return;
    }
    let mut k = 0;
    for i in 0..w.len() {
        if w[i] > 0.0 {
            continue;
        }
        while k + 1 < known.len() && known[k + 1] < i {
            k += 1;
        }
        let (a, b) = (known[k], known.get(k + 1).copied());
        w[i] = match b {
            _ if i < a => w[a],
            Some(b) => w[a] + (w[b] - w[a]) * (i - a) as f32 / (b - a) as f32,
            None => w[a],
        };
    }
}

/// Distance from `p` along the unit direction `d` to the nearest outline edge.
fn ray(edges: &[(Point, Point)], p: Point, d: Point) -> Option<f32> {
    let cross = |a: Point, b: Point| a.x * b.y - a.y * b.x;
    let mut best: Option<f32> = None;
    for (e0, e1) in edges {
        let e = Point::new(e1.x - e0.x, e1.y - e0.y);
        let den = cross(d, e);
        if den.abs() < 1e-9 {
            continue;
        }
        let ap = Point::new(e0.x - p.x, e0.y - p.y);
        let u = cross(ap, d) / den;
        let t = cross(ap, e) / den;
        if (0.0..1.0).contains(&u) && t > 0.0 && best.is_none_or(|b| t < b) {
            best = Some(t);
        }
    }
    best
}

/// Distance from `p` to the outline.
fn edge_dist(edges: &[(Point, Point)], p: Point) -> f32 {
    let mut best = f32::MAX;
    for (a, b) in edges {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
        best = best.min(dist2(p, Point::new(a.x + dx * t, a.y + dy * t)));
    }
    best.sqrt()
}

/// Drops the start of a polyline (and its widths) up to where `keep` first holds, cutting
/// the segment there by bisection. Leaves at least one point.
fn trim_start(pts: &mut Vec<Point>, w: &mut Vec<f32>, keep: impl Fn(Point) -> bool) {
    let Some(k) = pts.iter().position(|p| keep(*p)) else {
        let last = pts.len() - 1;
        pts.drain(..last);
        w.drain(..last);
        return;
    };
    if k == 0 {
        return;
    }
    let (a, b) = (pts[k - 1], pts[k]);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..12 {
        let m = 0.5 * (lo + hi);
        if keep(Point::new(a.x + (b.x - a.x) * m, a.y + (b.y - a.y) * m)) {
            hi = m;
        } else {
            lo = m;
        }
    }
    pts[k - 1] = Point::new(a.x + (b.x - a.x) * hi, a.y + (b.y - a.y) * hi);
    w[k - 1] = w[k];
    pts.drain(..k - 1);
    w.drain(..k - 1);
}

/// Pushes the last point on along the end's heading while the pen still fits in the ink.
fn extend_end(edges: &[(Point, Point)], pts: &mut Vec<Point>, w: &mut Vec<f32>, r: f32, cap_out: f32) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let rev: Vec<Point> = pts.iter().rev().copied().collect();
    let h = heading(&rev, r.max(1.5));
    let t = Point::new(-h.x, -h.y);
    if t == Point::ZERO {
        return;
    }
    let need = r * (1.0 - cap_out);
    let mut p = pts[n - 1];
    let mut moved = false;
    for _ in 0..(4.0 * r / 0.25) as usize {
        let q = Point::new(p.x + t.x * 0.25, p.y + t.y * 0.25);
        if edge_dist(edges, q) < need {
            break;
        }
        p = q;
        moved = true;
    }
    if moved {
        pts.push(p);
        let last = *w.last().unwrap_or(&0.0);
        w.push(last);
    }
}

/// Fits a stroke's ends to the ink: an end on a junction starts where the branch leaves the
/// junction's disk (the stroke through it covers the rest); a free end is cut back where
/// the pen would stand out of the ink and pushed on where it still fits. A stroke shorter
/// than the pen becomes a dot: a small circle the pen turns to fill it.
fn shape_ends(edges: &[(Point, Point)], s: &mut Raw, r: f32, cap_out: f32) {
    if s.closed || s.pts.is_empty() {
        return;
    }
    let need = r * (1.0 - cap_out);
    if polylen(&s.pts) < 2.0 * r && s.ends == [None, None] {
        let k = s.pts.len() as f32;
        let c = s.pts.iter().fold(Point::ZERO, |a, p| Point::new(a.x + p.x / k, a.y + p.y / k));
        let rho = edge_dist(edges, c) - need;
        if rho >= 0.5 {
            // The dot's own shape, inset by the pen: a ray to the outline in each of 24
            // directions, so a square dot gets its corners.
            s.pts = (0..24)
                .map(|i| {
                    let a = std::f32::consts::TAU * i as f32 / 24.0;
                    let d = Point::new(a.cos(), a.sin());
                    // Out from the centre while the pen still fits: the outline inset by
                    // the pen, so the pen touches each side and its corners.
                    let reach = ray(edges, c, d).unwrap_or(rho + need);
                    let mut k = 0.0;
                    while k + 0.25 < reach {
                        let q = Point::new(c.x + (k + 0.25) * d.x, c.y + (k + 0.25) * d.y);
                        if edge_dist(edges, q) < need {
                            break;
                        }
                        k += 0.25;
                    }
                    Point::new(c.x + k * d.x, c.y + k * d.y)
                })
                .collect();
            s.w = vec![2.0 * need; 24];
            s.closed = true;
        } else {
            s.pts = vec![Point::new(c.x - 0.25, c.y), Point::new(c.x + 0.25, c.y)];
            s.w = vec![2.0 * r; 2];
        }
        return;
    }
    for end in 0..2 {
        if end == 1 {
            s.pts.reverse();
            s.w.reverse();
        }
        match s.ends[end] {
            Some((c, radius)) => {
                if polylen(&s.pts) > radius + r {
                    trim_start(&mut s.pts, &mut s.w, |p| dist2(p, c) >= radius * radius);
                }
            }
            None => {
                trim_start(&mut s.pts, &mut s.w, |p| edge_dist(edges, p) >= need);
                s.pts.reverse();
                s.w.reverse();
                extend_end(edges, &mut s.pts, &mut s.w, r, cap_out);
                s.pts.reverse();
                s.w.reverse();
            }
        }
        if end == 1 {
            s.pts.reverse();
            s.w.reverse();
        }
    }
    if s.pts.len() == 1 {
        let c = s.pts[0];
        s.pts = vec![Point::new(c.x - 0.25, c.y), Point::new(c.x + 0.25, c.y)];
        s.w = vec![2.0 * r; 2];
    }
}

/// Moves each point of a stroke to the middle of the ink across it, measured on the outline
/// along the stroke's normal, and returns the ink's width there (0 where the normal does not
/// cross a plain stem: at a junction, past an end).
fn centre_on_outline(edges: &[(Point, Point)], pts: &mut [Point], closed: bool, stem: f32) -> Vec<f32> {
    let n = pts.len();
    let mut out = pts.to_vec();
    let mut widths = vec![0.0; n];
    if n < 2 {
        return widths;
    }
    let cross = |a: Point, b: Point| a.x * b.y - a.y * b.x;
    // The tangent from the points half a stem back and ahead, so pixel steps do not tilt it.
    let span = (0.5 * stem).max(1.5);
    let step = |i: usize, fwd: bool| -> Point {
        let (mut k, mut acc) = (i, 0.0);
        loop {
            let next = match (fwd, closed) {
                (true, true) => (k + 1) % n,
                (false, true) => (k + n - 1) % n,
                (true, false) if k + 1 < n => k + 1,
                (false, false) if k > 0 => k - 1,
                _ => return pts[k],
            };
            acc += dist2(pts[k], pts[next]).sqrt();
            k = next;
            if acc >= span || k == i {
                return pts[k];
            }
        }
    };
    for i in 0..n {
        let (a, b) = (step(i, false), step(i, true));
        let t = Point::new(b.x - a.x, b.y - a.y);
        let l = (t.x * t.x + t.y * t.y).sqrt();
        if l < 1e-6 {
            continue;
        }
        let nrm = Point::new(-t.y / l, t.x / l);
        let p = pts[i];
        let (mut fwd, mut back) = (f32::MAX, f32::MAX);
        for (e0, e1) in edges {
            let d = Point::new(e1.x - e0.x, e1.y - e0.y);
            let den = cross(nrm, d);
            if den.abs() < 1e-9 {
                continue;
            }
            let ap = Point::new(e0.x - p.x, e0.y - p.y);
            let u = cross(ap, nrm) / den;
            if !(0.0..1.0).contains(&u) {
                continue;
            }
            let s = cross(ap, d) / den;
            if s >= 0.0 {
                fwd = fwd.min(s);
            } else {
                back = back.min(-s);
            }
        }
        let w = fwd + back;
        let shift = 0.5 * (fwd - back);
        if w < 1.6 * stem + 2.0 && shift.abs() < 0.6 * stem + 0.5 {
            let q = Point::new(p.x + nrm.x * shift, p.y + nrm.y * shift);
            out[i] = q;
            // No wider than the circle that fits there: a chord into a junction's notch
            // is longer than the stem it starts on.
            widths[i] = w.min(2.0 * edge_dist(edges, q));
        }
    }
    pts.copy_from_slice(&out);
    widths
}

/// The outline of `glyph` at grid size and the face's x-height in em.
fn outline(face: &Face, glyph: u32) -> (BezPath, f32) {
    use skrifa::instance::Size;
    use skrifa::MetadataProvider;
    let path = crate::path::glyph_path(face, glyph, GRID);
    let xh = face.skrifa().metrics(Size::unscaled(), face.location()).x_height;
    let xh = xh.filter(|x| *x > 0.0).map_or(0.5, |x| x / face.units_per_em().max(1) as f32);
    (path, xh)
}

/// The cache key: the outline's coordinates and the x-height, so two registries that load
/// the same font share entries and an instance at another variation location does not.
fn key(path: &BezPath, xh: f32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    xh.to_bits().hash(&mut h);
    for el in path.elements() {
        let pts: &[kurbo::Point] = match el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => std::slice::from_ref(p),
            PathEl::QuadTo(a, b) => &[*a, *b],
            PathEl::CurveTo(a, b, c) => &[*a, *b, *c],
            PathEl::ClosePath => &[],
        };
        std::mem::discriminant(el).hash(&mut h);
        for p in pts {
            p.x.to_bits().hash(&mut h);
            p.y.to_bits().hash(&mut h);
        }
    }
    h.finish()
}

/// A cache answer.
#[derive(Clone, Debug)]
pub enum Lookup {
    Ready(Arc<GlyphCentre>),
    /// Queued on the worker; ask again on a later frame.
    Pending,
}

/// Entries kept before the cache starts over; a few faces' worth of glyphs.
const CACHE_CAP: usize = 4096;

enum Slot {
    Ready(Arc<GlyphCentre>),
    Pending,
}

struct Cache {
    slots: Mutex<HashMap<u64, Slot>>,
    worker: Mutex<Option<Sender<(u64, BezPath, f32)>>>,
}

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Cache { slots: Mutex::new(HashMap::new()), worker: Mutex::new(None) })
}

fn store(k: u64, g: Arc<GlyphCentre>) {
    let mut slots = cache().slots.lock().unwrap_or_else(|e| e.into_inner());
    if slots.len() >= CACHE_CAP && !slots.contains_key(&k) {
        slots.retain(|_, s| matches!(s, Slot::Pending));
    }
    slots.insert(k, Slot::Ready(g));
}

fn queue(k: u64, path: BezPath, xh: f32) {
    let mut w = cache().worker.lock().unwrap_or_else(|e| e.into_inner());
    let tx = w.get_or_insert_with(|| {
        let (tx, rx) = channel::<(u64, BezPath, f32)>();
        let spawned = std::thread::Builder::new().name("vo-centreline".into()).spawn(move || {
            for (k, path, xh) in rx {
                store(k, Arc::new(analyse(&path, xh)));
            }
        });
        if spawned.is_err() {
            // No thread: the glyph stays pending and `get_now` still works.
        }
        tx
    });
    let _ = tx.send((k, path, xh));
}

/// Whether `glyph` in `face` is drawn in colour (a COLR layer set or a bitmap, as emoji
/// are): no single pen line stands for it (SW-07).
pub fn is_colour(face: &Face, glyph: u32) -> bool {
    use skrifa::{GlyphId, MetadataProvider};
    let font = face.skrifa();
    let id = GlyphId::new(glyph);
    if font.color_glyphs().get(id).is_some() {
        return true;
    }
    let strikes = font.bitmap_strikes();
    (0..strikes.len()).filter_map(|i| strikes.get(i)).any(|s| s.get(id).is_some())
}

/// The strokes of `glyph` in `face` if they are cached; on a miss the glyph is queued on the
/// worker thread and this says [`Lookup::Pending`]. Never analyses on the calling thread.
pub fn lookup(face: &Face, glyph: u32) -> Lookup {
    let (path, xh) = outline(face, glyph);
    let k = key(&path, xh);
    {
        let mut slots = cache().slots.lock().unwrap_or_else(|e| e.into_inner());
        match slots.get(&k) {
            Some(Slot::Ready(g)) => return Lookup::Ready(g.clone()),
            Some(Slot::Pending) => return Lookup::Pending,
            None => {
                slots.insert(k, Slot::Pending);
            }
        }
    }
    queue(k, path, xh);
    Lookup::Pending
}

/// The strokes of `glyph` in `face`, analysed on this thread if they are not cached.
pub fn get_now(face: &Face, glyph: u32) -> Arc<GlyphCentre> {
    let (path, xh) = outline(face, glyph);
    let k = key(&path, xh);
    if let Some(Slot::Ready(g)) = cache().slots.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
        return g.clone();
    }
    let g = Arc::new(analyse(&path, xh));
    store(k, g.clone());
    g
}

/// Queues every inked glyph of `word` that is not cached yet, so a writer built a frame or
/// two later finds them ready.
pub fn prefetch(fonts: &FontRegistry, word: &WordOutline) {
    for g in word.glyphs.iter().filter(|g| g.has_ink()) {
        if let Some(face) = fonts.get(g.font) {
            let _ = lookup(&face, g.glyph);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Raster

/// Filled pixels of an outline, with a border of empty pixels.
struct Grid {
    /// The flattened outline, in the grid's path coordinates.
    edges: Vec<(Point, Point)>,
    w: usize,
    h: usize,
    /// Path coordinates of pixel (0, 0)'s top left corner.
    ox: f32,
    oy: f32,
    ink: Vec<bool>,
}

const PAD: usize = 2;

impl Grid {
    /// `path` filled at `scale` grid pixels per path unit.
    fn fill(path: &BezPath, scale: f32) -> Option<Grid> {
        let mut edges: Vec<(Point, Point)> = Vec::new();
        let (mut start, mut cur) = (None::<Point>, Point::ZERO);
        let close = |edges: &mut Vec<(Point, Point)>, start: &mut Option<Point>, cur: Point| {
            if let Some(s) = start.take() {
                if s != cur {
                    edges.push((cur, s));
                }
            }
        };
        kurbo::flatten(path.iter(), 0.1, |el| match el {
            PathEl::MoveTo(p) => {
                close(&mut edges, &mut start, cur);
                cur = Point::new(p.x as f32 * scale, p.y as f32 * scale);
                start = Some(cur);
            }
            PathEl::LineTo(p) => {
                let q = Point::new(p.x as f32 * scale, p.y as f32 * scale);
                edges.push((cur, q));
                cur = q;
            }
            PathEl::ClosePath => {
                close(&mut edges, &mut start, cur);
                if let Some(&(_, q)) = edges.last() {
                    cur = q;
                }
            }
            _ => {}
        });
        close(&mut edges, &mut start, cur);
        if edges.is_empty() {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (a, b) in &edges {
            for p in [a, b] {
                x0 = x0.min(p.x);
                y0 = y0.min(p.y);
                x1 = x1.max(p.x);
                y1 = y1.max(p.y);
            }
        }
        let ox = x0.floor() - PAD as f32;
        let oy = y0.floor() - PAD as f32;
        let w = (x1.ceil() - ox) as usize + PAD;
        let h = (y1.ceil() - oy) as usize + PAD;
        let mut ink = vec![false; w * h];
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for j in 0..h {
            let y = oy + j as f32 + 0.5;
            xs.clear();
            for (a, b) in &edges {
                if a.y == b.y {
                    continue;
                }
                let (lo, hi, dir) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
                if y < lo.y || y >= hi.y {
                    continue;
                }
                xs.push((lo.x + (y - lo.y) / (hi.y - lo.y) * (hi.x - lo.x), dir));
            }
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            for k in 0..xs.len() {
                wind += xs[k].1;
                if wind == 0 || k + 1 == xs.len() {
                    continue;
                }
                let (from, to) = (xs[k].0, xs[k + 1].0);
                // Pixels whose centre lies in [from, to).
                let i0 = (from - ox - 0.5).ceil().max(0.0) as usize;
                let i1 = ((to - ox - 0.5).ceil().max(0.0) as usize).min(w);
                for i in i0..i1 {
                    ink[j * w + i] = true;
                }
            }
        }
        Some(Grid { edges, w, h, ox, oy, ink })
    }

    /// Path coordinates of pixel `i`'s centre.
    fn centre(&self, i: usize) -> Point {
        Point::new(self.ox + (i % self.w) as f32 + 0.5, self.oy + (i / self.w) as f32 + 0.5)
    }

    /// The pixel nearest a point in path coordinates, clamped to the grid.
    fn pixel(&self, p: Point) -> usize {
        let x = ((p.x - self.ox).floor().max(0.0) as usize).min(self.w - 1);
        let y = ((p.y - self.oy).floor().max(0.0) as usize).min(self.h - 1);
        y * self.w + x
    }

    /// Euclidean distance from each ink pixel's centre to the nearest empty pixel's centre
    /// (Felzenszwalb and Huttenlocher), 0 outside the ink.
    fn distance(&self) -> Vec<f32> {
        const INF: f64 = 1e10;
        let (w, h) = (self.w, self.h);
        let mut f: Vec<f64> = self.ink.iter().map(|&b| if b { INF } else { 0.0 }).collect();
        let n = w.max(h);
        let (mut v, mut z, mut d, mut line) = (vec![0usize; n], vec![0.0f64; n + 1], vec![0.0f64; n], vec![0.0f64; n]);
        for j in 0..h {
            line[..w].copy_from_slice(&f[j * w..(j + 1) * w]);
            edt_1d(&line[..w], &mut d[..w], &mut v, &mut z);
            f[j * w..(j + 1) * w].copy_from_slice(&d[..w]);
        }
        for i in 0..w {
            for j in 0..h {
                line[j] = f[j * w + i];
            }
            edt_1d(&line[..h], &mut d[..h], &mut v, &mut z);
            for j in 0..h {
                f[j * w + i] = d[j];
            }
        }
        f.iter().map(|x| x.sqrt() as f32).collect()
    }

    /// The Zhang-Suen skeleton of the ink.
    fn thin(&self) -> Vec<bool> {
        let (w, h) = (self.w, self.h);
        let mut img = self.ink.clone();
        let mut del = Vec::new();
        loop {
            let mut changed = false;
            for pass in 0..2 {
                del.clear();
                for y in 1..h - 1 {
                    for x in 1..w - 1 {
                        let i = y * w + x;
                        if !img[i] {
                            continue;
                        }
                        let p = ring(&img, w, i);
                        let b = p.iter().filter(|&&q| q).count();
                        if !(2..=6).contains(&b) {
                            continue;
                        }
                        let a = (0..8).filter(|&k| !p[k] && p[(k + 1) % 8]).count();
                        if a != 1 {
                            continue;
                        }
                        // p[0] N, p[2] E, p[4] S, p[6] W.
                        let ok = if pass == 0 {
                            !(p[0] && p[2] && p[4]) && !(p[2] && p[4] && p[6])
                        } else {
                            !(p[0] && p[2] && p[6]) && !(p[0] && p[4] && p[6])
                        };
                        if ok {
                            del.push(i);
                        }
                    }
                }
                for &i in &del {
                    img[i] = false;
                }
                changed |= !del.is_empty();
            }
            if !changed {
                return img;
            }
        }
    }
}

/// The eight neighbours of pixel `i`, clockwise from north.
fn ring(img: &[bool], w: usize, i: usize) -> [bool; 8] {
    [img[i - w], img[i - w + 1], img[i + 1], img[i + w + 1], img[i + w], img[i + w - 1], img[i - 1], img[i - w - 1]]
}

/// Squared distance transform of a sampled function in one dimension (Felzenszwalb and
/// Huttenlocher's lower envelope of parabolas).
fn edt_1d(f: &[f64], d: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let cross = |q: usize, p: usize| ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * (q as f64 - p as f64));
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        let mut s = cross(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = cross(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let dq = q as f64 - v[k] as f64;
        *out = dq * dq + f[v[k]];
    }
}

/// The skeleton's neighbours of pixel `i`: its 4-neighbours, and a diagonal one only when
/// neither 4-neighbour between them is on the skeleton, so a staircase is a plain line.
fn neighbours(sk: &[bool], w: usize, i: usize) -> ([usize; 8], usize) {
    let mut out = [0usize; 8];
    let mut n = 0;
    for d in [i - w, i + 1, i + w, i - 1] {
        if sk[d] {
            out[n] = d;
            n += 1;
        }
    }
    for (d, a, b) in [(i - w + 1, i - w, i + 1), (i + w + 1, i + w, i + 1), (i + w - 1, i + w, i - 1), (i - w - 1, i - w, i - 1)] {
        if sk[d] && !sk[a] && !sk[b] {
            out[n] = d;
            n += 1;
        }
    }
    (out, n)
}

/// Stem width in pixels: twice the median distance to the edge along the skeleton, less
/// the half pixel the pixel centres add.
fn stem_width(skel: &[bool], dist: &[f32], w: usize) -> f32 {
    let mut ds: Vec<f32> = (0..skel.len())
        .filter(|&i| skel[i] && neighbours(skel, w, i).1 <= 2)
        .map(|i| dist[i])
        .collect();
    if ds.is_empty() {
        ds = (0..skel.len()).filter(|&i| skel[i]).map(|i| dist[i]).collect();
    }
    if ds.is_empty() {
        return 1.0;
    }
    ds.sort_by(|a, b| a.total_cmp(b));
    (2.0 * ds[ds.len() / 2] - 0.5).max(1.0)
}

// ---------------------------------------------------------------------------------------
// Skeleton graph

#[derive(Clone, Debug)]
struct Node {
    pos: Point,
    /// Junction radius: the distance to the edge at the node.
    radius: f32,
    alive: bool,
    /// The end of a pruned branch out of the point of a V: a stroke turning here runs out
    /// to it and back.
    tip: Option<Point>,
}

#[derive(Clone, Debug)]
struct Edge {
    a: usize,
    b: usize,
    /// Points strictly between the end nodes, from `a` to `b`.
    mid: Vec<Point>,
    alive: bool,
}

struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// Pure cycles with no node (an `o`), as closed point lists.
    loops: Vec<Vec<Point>>,
}

fn dist2(a: Point, b: Point) -> f32 {
    (a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y)
}

fn polylen(pts: &[Point]) -> f32 {
    pts.windows(2).map(|w| dist2(w[0], w[1]).sqrt()).sum()
}

impl Graph {
    fn trace(grid: &Grid, sk: &[bool], dist: &[f32]) -> Graph {
        let w = grid.w;
        let n = sk.len();
        let deg = |i: usize| neighbours(sk, w, i).1;
        let mut node_of: Vec<usize> = vec![usize::MAX; n];
        let mut nodes = Vec::new();
        // Junction pixels in clusters, then end and lone pixels on their own.
        for i in 0..n {
            if !sk[i] || node_of[i] != usize::MAX || deg(i) == 2 {
                continue;
            }
            let id = nodes.len();
            let mut members = vec![i];
            node_of[i] = id;
            if deg(i) >= 3 {
                let mut k = 0;
                while k < members.len() {
                    let (nb, c) = neighbours(sk, w, members[k]);
                    for &q in &nb[..c] {
                        if node_of[q] == usize::MAX && deg(q) >= 3 {
                            node_of[q] = id;
                            members.push(q);
                        }
                    }
                    k += 1;
                }
            }
            let (mut sx, mut sy, mut r) = (0.0, 0.0, 0.0f32);
            for &m in &members {
                let c = grid.centre(m);
                sx += c.x;
                sy += c.y;
                r = r.max(dist[m]);
            }
            let k = members.len() as f32;
            nodes.push(Node { pos: Point::new(sx / k, sy / k), radius: r, alive: true, tip: None });
        }
        let mut edges = Vec::new();
        let mut seen = vec![false; n];
        let mut direct: Vec<(usize, usize)> = Vec::new();
        for p in 0..n {
            if node_of[p] == usize::MAX {
                continue;
            }
            let a = node_of[p];
            let (nb, c) = neighbours(sk, w, p);
            for &q in &nb[..c] {
                if node_of[q] == a {
                    continue;
                }
                if node_of[q] != usize::MAX {
                    let pair = (p.min(q), p.max(q));
                    if !direct.contains(&pair) {
                        direct.push(pair);
                        edges.push(Edge { a, b: node_of[q], mid: Vec::new(), alive: true });
                    }
                    continue;
                }
                if seen[q] {
                    continue;
                }
                let mut mid = vec![grid.centre(q)];
                seen[q] = true;
                let (mut prev, mut cur) = (p, q);
                let b = loop {
                    let (nb2, c2) = neighbours(sk, w, cur);
                    let Some(&next) = nb2[..c2].iter().find(|&&x| x != prev) else { break a };
                    if node_of[next] != usize::MAX {
                        break node_of[next];
                    }
                    if seen[next] {
                        break a;
                    }
                    seen[next] = true;
                    mid.push(grid.centre(next));
                    prev = cur;
                    cur = next;
                };
                edges.push(Edge { a, b, mid, alive: true });
            }
        }
        // What is left unseen is pure cycles.
        let mut loops = Vec::new();
        for s in 0..n {
            if !sk[s] || seen[s] || node_of[s] != usize::MAX {
                continue;
            }
            let mut pts = vec![grid.centre(s)];
            seen[s] = true;
            let (mut prev, mut cur) = (usize::MAX, s);
            loop {
                let (nb, c) = neighbours(sk, w, cur);
                let Some(&next) = nb[..c].iter().find(|&&x| x != prev && !seen[x]) else { break };
                seen[next] = true;
                pts.push(grid.centre(next));
                prev = cur;
                cur = next;
            }
            if pts.len() >= 3 {
                loops.push(pts);
            }
        }
        Graph { nodes, edges, loops }
    }

    /// The edge's points starting at node `n`.
    fn points_from(&self, e: &Edge, n: usize) -> Vec<Point> {
        let mut pts = self.points(e);
        if e.a != n {
            pts.reverse();
        }
        pts
    }

    /// Heading and length of every live edge at node `n` but edge `skip`.
    fn branches(&self, n: usize, skip: usize, stem: f32) -> Vec<(Point, f32)> {
        let mut out = Vec::new();
        for (k, e) in self.edges.iter().enumerate() {
            if !e.alive || k == skip || (e.a != n && e.b != n) {
                continue;
            }
            let pts = self.points(e);
            let len = polylen(&pts);
            if e.a == n {
                out.push((heading(&pts, stem.max(3.0)), len));
            }
            if e.b == n {
                let rev: Vec<Point> = pts.iter().rev().copied().collect();
                out.push((heading(&rev, stem.max(3.0)), len));
            }
        }
        out
    }

    fn points(&self, e: &Edge) -> Vec<Point> {
        let mut pts = Vec::with_capacity(e.mid.len() + 2);
        pts.push(self.nodes[e.a].pos);
        pts.extend_from_slice(&e.mid);
        pts.push(self.nodes[e.b].pos);
        pts
    }

    fn degrees(&self) -> Vec<usize> {
        let mut d = vec![0; self.nodes.len()];
        for e in self.edges.iter().filter(|e| e.alive) {
            d[e.a] += 1;
            d[e.b] += 1;
        }
        d
    }

    /// Prunes spurs, merges close junctions and joins edges through nodes left with two.
    fn prune(&mut self, stem: f32) {
        for _ in 0..64 {
            let mut changed = false;
            // Spurs: an end hanging off a junction, shorter than the junction is wide.
            let deg = self.degrees();
            let mut spurs: Vec<(usize, f32, bool)> = Vec::new();
            for (k, e) in self.edges.iter().enumerate().filter(|(_, e)| e.alive && e.a != e.b) {
                let (end, junction) = match (deg[e.a], deg[e.b]) {
                    (1, d) if d >= 3 => (e.a, e.b),
                    (d, 1) if d >= 3 => (e.b, e.a),
                    _ => continue,
                };
                // How far the branch reaches past the junction's disk: a fork thinning leaves
                // in a square corner runs out towards the edge and barely gets past it.
                let len = polylen(&self.points(e));
                let reach = len + self.nodes[end].radius - self.nodes[junction].radius;
                let corner = self.nodes[end].radius < CORNER * stem;
                let others = self.branches(junction, k, stem);
                let h = heading(&self.points_from(e, junction), stem.max(3.0));
                // A stub straight across from a longer branch is the other half of a bar
                // (the left arm of a `t`): keep it.
                let across = others.iter().any(|(oh, olen)| -(h.x * oh.x + h.y * oh.y) > 0.9 && *olen > len);
                // A stub out of the point of a V (a flat-cut `w` or `W` bottom) is filled by
                // the pen going into the corner instead.
                let v_tip = others.len() == 2 && len < 1.5 * stem && {
                    let (a, b) = (others[0].0, others[1].0);
                    let out = Point::new(-(a.x + b.x), -(a.y + b.y));
                    let l = (out.x * out.x + out.y * out.y).sqrt();
                    a.x * b.x + a.y * b.y > 0.0 && l > 1e-6 && (h.x * out.x + h.y * out.y) / l > 0.8
                };
                if v_tip || (!across && (reach < SPUR * stem || (corner && reach < CORNER_SPUR * stem))) {
                    spurs.push((k, len, v_tip));
                }
            }
            // Shortest spur per junction first, one per junction per round, so two short
            // arms of one junction do not both go.
            spurs.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            let mut touched: Vec<usize> = Vec::new();
            for (k, _, v_tip) in spurs {
                let e = &self.edges[k];
                let j = if deg[e.a] == 1 { e.b } else { e.a };
                let end = if deg[e.a] == 1 { e.a } else { e.b };
                if touched.contains(&j) {
                    continue;
                }
                touched.push(j);
                if v_tip {
                    self.nodes[j].tip = Some(self.nodes[end].pos);
                }
                self.edges[k].alive = false;
                self.nodes[end].alive = false;
                changed = true;
            }
            if changed {
                continue;
            }
            // Junctions a short edge apart become one.
            let deg = self.degrees();
            let near = self.edges.iter().enumerate().find(|(_, e)| {
                e.alive && e.a != e.b && deg[e.a] >= 3 && deg[e.b] >= 3 && polylen(&self.points(e)) < 0.6 * stem
            });
            if let Some((k, _)) = near {
                let (a, b) = (self.edges[k].a, self.edges[k].b);
                self.edges[k].alive = false;
                let (pa, pb) = (self.nodes[a].pos, self.nodes[b].pos);
                self.nodes[a].pos = Point::new(0.5 * (pa.x + pb.x), 0.5 * (pa.y + pb.y));
                self.nodes[a].radius = self.nodes[a].radius.max(self.nodes[b].radius);
                self.nodes[b].alive = false;
                for e in self.edges.iter_mut().filter(|e| e.alive) {
                    if e.a == b {
                        e.a = a;
                    }
                    if e.b == b {
                        e.b = a;
                    }
                }
                continue;
            }
            // Nodes with two edges: join them.
            let deg = self.degrees();
            let two = (0..self.nodes.len()).find(|&n| {
                deg[n] == 2
                    && self.nodes[n].alive
                    && !self.edges.iter().any(|e| e.alive && e.a == n && e.b == n)
            });
            if let Some(n) = two {
                let ks: Vec<usize> =
                    (0..self.edges.len()).filter(|&k| self.edges[k].alive && (self.edges[k].a == n || self.edges[k].b == n)).collect();
                let (e1, e2) = (self.edges[ks[0]].clone(), self.edges[ks[1]].clone());
                // e1 oriented to end at n, e2 to start at n.
                let e1 = if e1.b == n { e1 } else { reversed(e1) };
                let e2 = if e2.a == n { e2 } else { reversed(e2) };
                let mut mid = e1.mid;
                mid.push(self.nodes[n].pos);
                if let Some(tip) = self.nodes[n].tip {
                    // Out to the tip and back, a point per pixel so smoothing keeps it.
                    let c = self.nodes[n].pos;
                    let steps = dist2(c, tip).sqrt().ceil().max(1.0) as usize;
                    let at = |k: usize| {
                        let t = k as f32 / steps as f32;
                        Point::new(c.x + (tip.x - c.x) * t, c.y + (tip.y - c.y) * t)
                    };
                    mid.extend((1..=steps).map(at));
                    mid.extend((0..steps).rev().map(at));
                }
                mid.extend(e2.mid);
                self.edges[ks[0]].alive = false;
                self.edges[ks[1]].alive = false;
                self.nodes[n].alive = false;
                if e1.a == e2.b && self.degrees()[e1.a] == 0 {
                    // A loop through one other node with nothing else on it: a cycle.
                    let mut pts = vec![self.nodes[e1.a].pos];
                    pts.extend(mid);
                    self.nodes[e1.a].alive = false;
                    self.loops.push(pts);
                } else {
                    self.edges.push(Edge { a: e1.a, b: e2.b, mid, alive: true });
                }
                continue;
            }
            // A node whose only edge is a loop back to itself: a cycle.
            let lone = self.edges.iter().position(|e| e.alive && e.a == e.b && deg[e.a] == 2);
            if let Some(k) = lone {
                let e = self.edges[k].clone();
                self.edges[k].alive = false;
                self.nodes[e.a].alive = false;
                let mut pts = vec![self.nodes[e.a].pos];
                pts.extend(e.mid);
                self.loops.push(pts);
                continue;
            }
            break;
        }
    }

    /// Joins edges into strokes through the junctions where they run on straightest.
    /// At a three-way node where a ring meets a short free spur that runs on from neither side
    /// of it (the slash of a heavy `ø`, cut short by the ink at the ring's corner), the
    /// ring's two ends, to be joined so the ring stays one closed stroke, and the spur, a
    /// stroke of its own: `(spur, ring end, ring end)` as indices into `ends`. A spur that
    /// runs on straight from the ring (the stem of `b` above its bowl, the legs of `A`) keeps
    /// the usual join.
    fn slash_at(&self, n: usize, ends: &[(usize, Point)], deg: &[usize], stem: f32) -> Option<(usize, usize, usize)> {
        if ends.len() != 3 {
            return None;
        }
        let other = |end: usize| {
            let e = &self.edges[end / 2];
            if end.is_multiple_of(2) {
                e.b
            } else {
                e.a
            }
        };
        let spur = (0..3).find(|&k| {
            let m = other(ends[k].0);
            m != n && deg[m] == 1 && polylen(&self.points(&self.edges[ends[k].0 / 2])) < SLASH_LEN * stem
        })?;
        let (i, j) = match spur {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        // The two ring ends lead to the same node, round a ring.
        if other(ends[i].0) != other(ends[j].0) || deg[other(ends[i].0)] == 1 {
            return None;
        }
        let run_on = |a: usize, b: usize| -(ends[a].1.x * ends[b].1.x + ends[a].1.y * ends[b].1.y);
        (run_on(spur, i) < SLASH && run_on(spur, j) < SLASH).then_some((spur, i, j))
    }

    fn strokes(&self, stem: f32) -> Vec<Raw> {
        let ne = self.edges.len();
        // End reference: 2 * edge + (0 at a, 1 at b).
        let mut partner: Vec<Option<usize>> = vec![None; 2 * ne];
        let reach = stem.max(3.0);
        let deg = self.degrees();
        // Spurs a ring kept closed past (`slash_at`): written apart, after the ring.
        let mut slashes: Vec<usize> = Vec::new();
        for n in 0..self.nodes.len() {
            if !self.nodes[n].alive {
                continue;
            }
            let mut ends: Vec<(usize, Point)> = Vec::new();
            for (k, e) in self.edges.iter().enumerate().filter(|(_, e)| e.alive) {
                let pts = self.points(e);
                if e.a == n {
                    ends.push((2 * k, heading(&pts, reach)));
                }
                if e.b == n {
                    let rev: Vec<Point> = pts.iter().rev().copied().collect();
                    ends.push((2 * k + 1, heading(&rev, reach)));
                }
            }
            if ends.len() < 2 {
                continue;
            }
            let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
            for i in 0..ends.len() {
                for j in i + 1..ends.len() {
                    let s = -(ends[i].1.x * ends[j].1.x + ends[i].1.y * ends[j].1.y);
                    pairs.push((s, i, j));
                }
            }
            pairs.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
            let mut used = vec![false; ends.len()];
            if let Some((spur, i, j)) = self.slash_at(n, &ends, &deg, stem) {
                used[spur] = true;
                used[i] = true;
                used[j] = true;
                partner[ends[i].0] = Some(ends[j].0);
                partner[ends[j].0] = Some(ends[i].0);
                slashes.push(ends[spur].0 / 2);
            }
            for (s, i, j) in pairs {
                if s < STRAIGHT || used[i] || used[j] {
                    continue;
                }
                used[i] = true;
                used[j] = true;
                partner[ends[i].0] = Some(ends[j].0);
                partner[ends[j].0] = Some(ends[i].0);
            }
        }
        let mut done = vec![false; ne];
        let mut out = Vec::new();
        let walk = |start: usize, done: &mut Vec<bool>| -> (Vec<Point>, Vec<usize>, bool, usize) {
            let mut pts: Vec<Point> = Vec::new();
            let mut through = Vec::new();
            let mut end = start;
            loop {
                let k = end / 2;
                done[k] = true;
                let mut ep = self.points(&self.edges[k]);
                if !end.is_multiple_of(2) {
                    ep.reverse();
                }
                if !pts.is_empty() {
                    ep.remove(0);
                }
                pts.extend(ep);
                let exit = end ^ 1;
                let exit_node = if exit.is_multiple_of(2) { self.edges[k].a } else { self.edges[k].b };
                match partner[exit] {
                    Some(p) if p == start => {
                        through.push(exit_node);
                        return (pts, through, true, exit_node);
                    }
                    Some(p) if !done[p / 2] => {
                        through.push(exit_node);
                        end = p;
                    }
                    _ => return (pts, through, false, exit_node),
                }
            }
        };
        // Open chains from free ends first, in edge order, then the cycles that remain.
        for start in 0..2 * ne {
            if !self.edges[start / 2].alive || done[start / 2] || partner[start].is_some() {
                continue;
            }
            let first = if start.is_multiple_of(2) { self.edges[start / 2].a } else { self.edges[start / 2].b };
            let (pts, through, closed, last) = walk(start, &mut done);
            let mut raw = Raw::new(pts, closed, through);
            let junction = |n: usize| (deg[n] >= 3).then(|| (self.nodes[n].pos, self.nodes[n].radius));
            raw.ends = [junction(first), junction(last)];
            raw.delayed = slashes.contains(&(start / 2));
            out.push(raw);
        }
        for k in 0..ne {
            if self.edges[k].alive && !done[k] {
                let (mut pts, through, closed, _) = walk(2 * k, &mut done);
                if closed && pts.len() > 1 && dist2(pts[0], *pts.last().unwrap()) < 1e-6 {
                    pts.pop();
                }
                out.push(Raw::new(pts, closed, through));
            }
        }
        for l in &self.loops {
            out.push(Raw::new(l.clone(), true, Vec::new()));
        }
        // Lone pixels and nodes with no edges: dots.
        for (n, node) in self.nodes.iter().enumerate() {
            if node.alive && deg[n] == 0 {
                let p = node.pos;
                out.push(Raw::new(vec![Point::new(p.x - 0.25, p.y), Point::new(p.x + 0.25, p.y)], false, Vec::new()));
            }
        }
        for s in &mut out {
            s.pts = smooth(&s.pts, s.closed);
        }
        out
    }
}

/// A spur that reaches less than this many stem widths past its junction's disk is pruned;
/// one that ends in a corner (its end less than `CORNER` stems from the edge), when it
/// reaches less than `CORNER_SPUR` stems. A short crossbar stub ends well inside its ink
/// and stays.
const SPUR: f32 = 0.25;
const CORNER: f32 = 0.3;
const CORNER_SPUR: f32 = 0.6;
/// Two branches at a junction join when they run on within this much of straight
/// (-cos of the angle between their headings; 0.6 is about 127 degrees).
const STRAIGHT: f32 = 0.6;
/// A spur off a ring is a slash ([`Graph::slash_at`]) when it runs on from both sides of the
/// ring at less than this (about 154 degrees) and is shorter than `SLASH_LEN` stems: a
/// longer one is a leg or a tail of its own (`R`, `A`).
const SLASH: f32 = 0.9;
const SLASH_LEN: f32 = 1.0;

fn reversed(e: Edge) -> Edge {
    let mut mid = e.mid;
    mid.reverse();
    Edge { a: e.b, b: e.a, mid, alive: e.alive }
}

/// Unit heading from `pts[0]` to the point `reach` along the polyline.
fn heading(pts: &[Point], reach: f32) -> Point {
    let o = pts[0];
    let mut acc = 0.0;
    let mut target = *pts.last().unwrap_or(&o);
    for w in pts.windows(2) {
        acc += dist2(w[0], w[1]).sqrt();
        if acc >= reach {
            target = w[1];
            break;
        }
    }
    let d = Point::new(target.x - o.x, target.y - o.y);
    let l = (d.x * d.x + d.y * d.y).sqrt();
    if l < 1e-6 {
        Point::ZERO
    } else {
        Point::new(d.x / l, d.y / l)
    }
}

/// A stroke being built, in grid path coordinates.
#[derive(Clone, Debug)]
struct Raw {
    pts: Vec<Point>,
    /// Ink width at each point, 0 where unknown.
    w: Vec<f32>,
    /// A junction (its centre and radius) at the start and at the end of an open stroke.
    ends: [Option<(Point, f32)>; 2],
    closed: bool,
    /// Nodes the stroke passes through (not its ends).
    through: Vec<usize>,
    delayed: bool,
    len: f32,
    top: f32,
    left: f32,
    horizontal: bool,
}

impl Raw {
    fn new(pts: Vec<Point>, closed: bool, through: Vec<usize>) -> Raw {
        Raw { pts, w: Vec::new(), ends: [None, None], closed, through, delayed: false, len: 0.0, top: 0.0, left: 0.0, horizontal: false }
    }

    /// Open: left to right when wider than tall, else top to bottom. Closed: anticlockwise
    /// on screen from the top.
    fn orient(&mut self) {
        let pts = &mut self.pts;
        if pts.len() < 2 {
            return;
        }
        if self.closed {
            // Shoelace in y-down space: positive is clockwise on screen.
            let n = pts.len();
            let area: f32 = (0..n).map(|i| {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                a.x * b.y - b.x * a.y
            }).sum();
            if area > 0.0 {
                pts.reverse();
                self.w.reverse();
            }
            let top = (0..n)
                .min_by(|&i, &j| pts[i].y.total_cmp(&pts[j].y).then(pts[i].x.total_cmp(&pts[j].x)))
                .unwrap_or(0);
            pts.rotate_left(top);
            self.w.rotate_left(top);
        } else {
            let (a, b) = (pts[0], *pts.last().unwrap());
            let (x0, x1, y0, y1) = bounds(pts);
            let wide = x1 - x0 > y1 - y0;
            let flip = if wide { a.x > b.x } else { a.y > b.y };
            if flip {
                pts.reverse();
                self.w.reverse();
            }
        }
    }

    fn measure(&mut self) {
        let (x0, x1, y0, y1) = bounds(&self.pts);
        self.top = y0;
        self.left = x0;
        self.horizontal = x1 - x0 > y1 - y0;
        let mut len = polylen(&self.pts);
        if self.closed && self.pts.len() > 1 {
            len += dist2(self.pts[0], *self.pts.last().unwrap()).sqrt();
        }
        self.len = len;
    }
}

fn bounds(pts: &[Point]) -> (f32, f32, f32, f32) {
    pts.iter().fold((f32::MAX, f32::MIN, f32::MAX, f32::MIN), |(x0, x1, y0, y1), p| {
        (x0.min(p.x), x1.max(p.x), y0.min(p.y), y1.max(p.y))
    })
}

/// Ramer-Douglas-Peucker at 0.35 px: the indices of the points kept.
fn simplify(pts: &[Point], w: &[f32], closed: bool) -> Vec<usize> {
    if pts.len() < 3 {
        return (0..pts.len()).collect();
    }
    if closed {
        // Split at the point farthest from the first and simplify both halves.
        let far = (1..pts.len()).max_by(|&i, &j| dist2(pts[0], pts[i]).total_cmp(&dist2(pts[0], pts[j]))).unwrap_or(1);
        let mut idx: Vec<usize> = (0..=far).collect();
        idx.extend(far + 1..pts.len());
        idx.push(0);
        let keep = rdp(pts, w, &idx);
        let mut out: Vec<usize> = keep.into_iter().map(|k| idx[k]).collect();
        out.pop();
        return out;
    }
    let idx: Vec<usize> = (0..pts.len()).collect();
    rdp(pts, w, &idx)
}

/// RDP over `pts[idx[..]]`, both ends kept, with a split at the farthest point from the
/// first so a closed run works too. Returns positions in `idx`.
fn rdp(pts: &[Point], wid: &[f32], idx: &[usize]) -> Vec<usize> {
    const EPS: f32 = 0.35;
    let m = idx.len();
    let mut keep = vec![false; m];
    keep[0] = true;
    keep[m - 1] = true;
    let far = (1..m).max_by(|&i, &j| dist2(pts[idx[0]], pts[idx[i]]).total_cmp(&dist2(pts[idx[0]], pts[idx[j]]))).unwrap_or(0);
    keep[far] = true;
    let mut stack = vec![(0usize, far), (far, m - 1)];
    while let Some((i, j)) = stack.pop() {
        if j <= i + 1 {
            continue;
        }
        let (a, b) = (pts[idx[i]], pts[idx[j]]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let l = (dx * dx + dy * dy).sqrt();
        let mut best = (0.0f32, i);
        let (wa, wb) = (wid[idx[i]], wid[idx[j]]);
        for k in i + 1..j {
            let p = pts[idx[k]];
            let d = if l < 1e-6 { dist2(p, a).sqrt() } else { ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / l };
            // A width that leaves the straight line between the ends counts at half.
            let f = (k - i) as f32 / (j - i) as f32;
            let d = d.max(0.5 * (wid[idx[k]] - (wa + (wb - wa) * f)).abs());
            if d > best.0 {
                best = (d, k);
            }
        }
        if best.0 > EPS {
            keep[best.1] = true;
            stack.push((i, best.1));
            stack.push((best.1, j));
        }
    }
    (0..m).filter(|&k| keep[k]).collect()
}

/// Two rounds of Chaikin corner cutting; an open polyline keeps its ends.
fn smooth(pts: &[Point], closed: bool) -> Vec<Point> {
    let mut cur = pts.to_vec();
    let mix = |a: Point, b: Point, t: f32| Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
    for _ in 0..2 {
        if cur.len() < 3 {
            return cur;
        }
        let n = cur.len();
        let mut next = Vec::with_capacity(2 * n + 2);
        if closed {
            for i in 0..n {
                let (a, b) = (cur[i], cur[(i + 1) % n]);
                next.push(mix(a, b, 0.25));
                next.push(mix(a, b, 0.75));
            }
        } else {
            next.push(cur[0]);
            for w in cur.windows(2) {
                next.push(mix(w[0], w[1], 0.25));
                next.push(mix(w[0], w[1], 0.75));
            }
            next.push(cur[n - 1]);
        }
        cur = next;
    }
    cur
}

/// Marks and crossbars become delayed strokes.
fn tag_delayed(strokes: &mut [Raw], grid: &Grid, x_height: f32, stem: f32) {
    // Ink parts, 8-connected.
    let (w, n) = (grid.w, grid.ink.len());
    let mut label = vec![usize::MAX; n];
    let mut parts: Vec<(usize, f32, f32)> = Vec::new(); // area, top, bottom
    let mut stack = Vec::new();
    for s in 0..n {
        if !grid.ink[s] || label[s] != usize::MAX {
            continue;
        }
        let id = parts.len();
        let (mut area, mut top, mut bottom) = (0usize, f32::MAX, f32::MIN);
        label[s] = id;
        stack.push(s);
        while let Some(i) = stack.pop() {
            area += 1;
            let c = grid.centre(i);
            top = top.min(c.y - 0.5);
            bottom = bottom.max(c.y + 0.5);
            for d in [i - w - 1, i - w, i - w + 1, i - 1, i + 1, i + w - 1, i + w, i + w + 1] {
                if grid.ink[d] && label[d] == usize::MAX {
                    label[d] = id;
                    stack.push(d);
                }
            }
        }
        parts.push((area, top, bottom));
    }
    let body = (0..parts.len()).max_by(|&a, &b| parts[a].0.cmp(&parts[b].0).then(b.cmp(&a))).unwrap_or(0);
    let part_of = |p: Point| -> Option<usize> {
        let i = grid.pixel(p);
        if label[i] != usize::MAX {
            return Some(label[i]);
        }
        // The nearest ink pixel within a few pixels.
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        let mut best: Option<(isize, usize)> = None;
        for dy in -3isize..=3 {
            for dx in -3isize..=3 {
                let (xx, yy) = (x + dx, y + dy);
                if xx < 0 || yy < 0 || xx >= w as isize || yy >= grid.h as isize {
                    continue;
                }
                let j = yy as usize * w + xx as usize;
                if label[j] != usize::MAX && best.is_none_or(|b| dx * dx + dy * dy < b.0) {
                    best = Some((dx * dx + dy * dy, label[j]));
                }
            }
        }
        best.map(|b| b.1)
    };
    for s in strokes.iter_mut() {
        let Some(part) = s.pts.first().and_then(|p| part_of(*p)) else { continue };
        if part == body {
            continue;
        }
        let (_, top, bottom) = parts[part];
        let above = bottom < -0.85 * x_height;
        let below = top > 0.5 * stem;
        s.delayed = above || below;
    }
    // Crossbars: of two strokes that pass through the same node, the shorter, when it is
    // wider than tall or the other is closed.
    let k = strokes.len();
    for i in 0..k {
        for j in 0..k {
            if i == j {
                continue;
            }
            let crosses = strokes[i].through.iter().any(|n| strokes[j].through.contains(n));
            if !crosses {
                continue;
            }
            let (a, b) = (&strokes[i], &strokes[j]);
            let shorter = a.len < b.len || (a.len == b.len && i > j);
            if shorter && !a.closed && (a.horizontal || b.closed) {
                strokes[i].delayed = true;
            }
        }
    }
}

/// Pen order: base strokes then delayed ones; within each, repeatedly the leftmost of the
/// strokes whose top (a horizontal's lifted by a stem) is within half a stem of the highest.
fn order(strokes: &mut Vec<Raw>, stem: f32) {
    let mut rest: Vec<Raw> = std::mem::take(strokes);
    let key = |s: &Raw| s.top - if s.horizontal { stem } else { 0.0 };
    for delayed in [false, true] {
        loop {
            let group: Vec<usize> = (0..rest.len()).filter(|&i| rest[i].delayed == delayed).collect();
            let Some(min) = group.iter().map(|&i| key(&rest[i])).min_by(|a, b| a.total_cmp(b)) else { break };
            let pick = group
                .iter()
                .copied()
                .filter(|&i| key(&rest[i]) <= min + 0.5 * stem)
                .min_by(|&a, &b| rest[a].left.total_cmp(&rest[b].left).then(a.cmp(&b)))
                .unwrap_or(group[0]);
            strokes.push(rest.remove(pick));
        }
    }
}

/// Grid pixels per analysis pixel for the coverage test.
const FINE: f32 = 3.0;

/// Coverage of the outline's ink by the strokes, each at its own widths, or all at
/// `width` when given, on a grid [`FINE`] times finer than the analysis.
fn coverage(path: &BezPath, strokes: &[Raw], width: Option<f32>) -> Coverage {
    let Some(grid) = Grid::fill(path, FINE) else { return Coverage::default() };
    let mut pen = vec![false; grid.ink.len()];
    let fine = |p: Point| Point::new(p.x * FINE, p.y * FINE);
    for s in strokes {
        let mut pts: Vec<(Point, f32)> =
            s.pts.iter().zip(&s.w).map(|(p, w)| (fine(*p), 0.5 * FINE * width.unwrap_or(*w))).collect();
        if s.closed && !pts.is_empty() {
            pts.push(pts[0]);
        }
        if pts.len() == 1 {
            pts.push(pts[0]);
        }
        for seg in pts.windows(2) {
            stamp(&grid, &mut pen, seg[0].0, seg[1].0, seg[0].1, seg[1].1);
        }
    }
    let ink = grid.ink.iter().filter(|&&b| b).count();
    let drawn = pen.iter().filter(|&&b| b).count();
    let both = grid.ink.iter().zip(&pen).filter(|(a, b)| **a && **b).count();
    Coverage {
        covered: if ink == 0 { 1.0 } else { both as f32 / ink as f32 },
        outside: if drawn == 0 { 0.0 } else { (drawn - both) as f32 / drawn as f32 },
    }
}

/// [`GlyphCentre::coverage`] measured again: the strokes of `g` over the outline `path` (in
/// grid pixels, as [`analyse`] takes it), each at its own widths, or all at `width` (in em)
/// when given.
pub fn measure_coverage(path: &BezPath, g: &GlyphCentre, width: Option<f32>) -> Coverage {
    let raws: Vec<Raw> = g
        .strokes
        .iter()
        .map(|c| {
            let mut r = Raw::new(c.points.iter().map(|p| Point::new(p.x * GRID, p.y * GRID)).collect(), c.closed, Vec::new());
            r.w = c.widths.iter().map(|w| w * GRID).collect();
            r
        })
        .collect();
    coverage(path, &raws, width.map(|w| w * GRID))
}

/// Marks the pixels whose centre is within the segment `a`-`b` drawn with a radius going
/// from `ra` to `rb`.
fn stamp(grid: &Grid, pen: &mut [bool], a: Point, b: Point, ra: f32, rb: f32) {
    let r = ra.max(rb);
    let x0 = ((a.x.min(b.x) - r - grid.ox).floor().max(0.0)) as usize;
    let y0 = ((a.y.min(b.y) - r - grid.oy).floor().max(0.0)) as usize;
    let x1 = (((a.x.max(b.x) + r - grid.ox).ceil().max(0.0)) as usize).min(grid.w);
    let y1 = (((a.y.max(b.y) + r - grid.oy).ceil().max(0.0)) as usize).min(grid.h);
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l2 = dx * dx + dy * dy;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = Point::new(grid.ox + x as f32 + 0.5, grid.oy + y as f32 + 0.5);
            let t = if l2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            let q = Point::new(a.x + dx * t, a.y + dy * t);
            let rr = ra + (rb - ra) * t;
            if dist2(p, q) <= rr * rr {
                pen[y * grid.w + x] = true;
            }
        }
    }
}

/// An ASCII picture of the analysis at `cap_out` (`CL_CAP`, default the first fit) and
/// width scale `CL_WIDEN`, for tuning: digits are the strokes in order, `#` ink the pen
/// covers, `o` ink it misses, `!` pen outside the ink.
#[doc(hidden)]
pub fn debug_art(path: &BezPath, x_height: f32) -> String {
    let Some(grid) = Grid::fill(path, 1.0) else { return String::new() };
    let (base, stem_px) = trace(&grid);
    let cap_out: f32 = std::env::var("CL_CAP").ok().and_then(|v| v.parse().ok()).unwrap_or(FITS[0].0);
    let widen: f32 = std::env::var("CL_WIDEN").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let strokes = fit(&grid, base, stem_px, x_height, cap_out, widen);
    let cov = coverage(path, &strokes, None);
    let mut line = vec![0u8; grid.ink.len()];
    let mut pen = vec![false; grid.ink.len()];
    for (k, s) in strokes.iter().enumerate() {
        let mut m = vec![false; grid.ink.len()];
        let mut pts: Vec<(Point, f32)> = s.pts.iter().copied().zip(s.w.iter().copied()).collect();
        if s.closed {
            pts.push(pts[0]);
        }
        if pts.len() == 1 {
            pts.push(pts[0]);
        }
        for seg in pts.windows(2) {
            stamp(&grid, &mut m, seg[0].0, seg[1].0, 0.5, 0.5);
            stamp(&grid, &mut pen, seg[0].0, seg[1].0, 0.5 * seg[0].1, 0.5 * seg[1].1);
        }
        for i in 0..m.len() {
            if m[i] {
                line[i] = b'0' + k as u8;
            }
        }
    }
    let mut out = format!(
        "stem {stem_px:.1} covered {:.3} outside {:.3} delayed {:?}\n",
        cov.covered,
        cov.outside,
        strokes.iter().map(|s| s.delayed).collect::<Vec<_>>()
    );
    for y in 0..grid.h {
        for x in 0..grid.w {
            let i = y * grid.w + x;
            out.push(match (line[i], grid.ink[i], pen[i]) {
                (c, _, _) if c != 0 => c as char,
                (_, true, true) => '#',
                (_, true, false) => 'o',
                (_, false, true) => '!',
                _ => '.',
            });
        }
        out.push('\n');
    }
    out
}
