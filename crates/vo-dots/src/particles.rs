//! Grid-snapped dot particles (PA09): dots that peel off and fly away on dismiss, and dots
//! that fly in and assemble into a shape, such as the check on paste.
//!
//! These are [`DotField`] dots on springs, not the GPU particle engine: a few hundred to a
//! few thousand dots with closed-form motion, every target on a whole grid cell, so the
//! shape they land in is as crisp as the rest of the grid. A spring flight is an ease, not a
//! ballistic arc; curl noise and gravity belong to the particle engine.

use std::sync::Arc;

use vo_core::hash::{rand_f32, stagger_fit};
use vo_scene::dots::{Dot, DotSource};

use crate::field::{DotField, Look};

/// The cells of a check mark in a `w` by `h` box of cells: a short stroke down to the
/// bottom third, then a long stroke up to the top right. `thick` cells wide.
pub fn check_cells(w: i32, h: i32, thick: i32) -> Vec<(i32, i32)> {
    let (w, h) = (w.max(3) as f32 - 1.0, h.max(3) as f32 - 1.0);
    let a = (0.0, 0.55 * h);
    let b = (0.36 * w, h);
    let c = (w, 0.0);
    let mut out = Vec::new();
    for (p, q) in [(a, b), (b, c)] {
        let steps = (q.0 - p.0).abs().max((q.1 - p.1).abs()).ceil() as i32 * 2;
        for i in 0..=steps {
            let k = i as f32 / steps as f32;
            let (x, y) = (p.0 + (q.0 - p.0) * k, p.1 + (q.1 - p.1) * k);
            for d in 0..thick {
                out.push((x.round() as i32, y.round() as i32 - d));
            }
        }
    }
    out.sort_by_key(|&(x, y)| (x, y));
    out.dedup();
    out.retain(|&(_, y)| y >= 0);
    out
}

/// How dots peel off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peel {
    /// Direction of flight in cells (any length).
    pub dir: (f32, f32),
    /// Cells travelled, each dot 0.6 to 1.4 times this.
    pub distance: f32,
    /// Sideways scatter, in cells.
    pub spread: f32,
    /// Seconds for the peel to sweep from the leftmost dot to the rightmost.
    pub sweep: f64,
    pub seed: u32,
}

impl Default for Peel {
    fn default() -> Peel {
        Peel { dir: (0.35, -1.0), distance: 18.0, spread: 6.0, sweep: 0.28, seed: 1 }
    }
}

/// Sends dots `ids` flying from `t`: each leaves when the sweep reaches its column, travels
/// to a whole cell along `p.dir`, shrinks and fades. They are hidden at the end, so
/// `DotField::prune` drops them.
pub fn peel_off(field: &mut DotField, t: f64, ids: &[u32], p: &Peel) {
    let looks: Vec<(u32, Look)> = ids.iter().filter_map(|&id| Some((id, field.look(t, id)?))).collect();
    let (x0, x1) = looks.iter().fold((f32::MAX, f32::MIN), |(a, b), (_, l)| (a.min(l.x), b.max(l.x)));
    let span = (x1 - x0).max(1.0);
    let len = p.dir.0.hypot(p.dir.1).max(1e-6);
    let (dx, dy) = (p.dir.0 / len, p.dir.1 / len);
    for &(id, l) in &looks {
        let r = |k: u32| rand_f32(p.seed, id, k, 0);
        let d = p.distance * (0.6 + 0.8 * r(0));
        let side = p.spread * (r(1) - 0.5);
        let to = Look {
            x: (l.x + dx * d - dy * side).round(),
            y: (l.y + dy * d + dx * side).round(),
            size: l.size * 0.4,
            bright: 0.0,
            color: l.color,
        };
        let delay = p.sweep * ((l.x - x0) / span) as f64 + 0.06 * r(2) as f64;
        field.retarget(t, id, to, delay, None);
    }
}

/// Makes a dot at each look in `from` and sends the dots to `cells` (offset by `origin`) from
/// `t`, so a shape assembles out of dots that were somewhere else, such as the waveform's.
/// Ids are `base`, `base + 1`, and so on; returns them. Sources and cells are paired left to
/// right so paths do not cross much; spare sources fade into their nearest cell, and when
/// there are too few sources some cells start from a neighbour's source. Cells start left to
/// right, `gap` seconds apart, the last no later than `cap`.
#[allow(clippy::too_many_arguments)]
pub fn assemble(
    field: &mut DotField,
    t: f64,
    from: &[Look],
    cells: &[(i32, i32)],
    origin: (f32, f32),
    look: Look,
    base: u32,
    gap: f64,
    cap: f64,
) -> Vec<u32> {
    let mut src: Vec<Look> = from.to_vec();
    src.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    let mut dst: Vec<(i32, i32)> = cells.to_vec();
    dst.sort();
    let (ns, nd) = (src.len(), dst.len());
    let mut ids = Vec::new();
    if nd == 0 {
        return ids;
    }
    let target = |j: usize| look.at(origin.0 + dst[j].0 as f32, origin.1 + dst[j].1 as f32);
    let delay = |j: usize| stagger_fit(dst[j].0.max(0) as usize, (dst[nd - 1].0 + 1).max(1) as usize, gap, cap);
    let mut used = vec![false; ns];
    let mut next = base;
    for j in 0..nd {
        let i = (j * ns / nd).min(ns.max(1) - 1);
        let start = if ns == 0 { target(j).hidden() } else { src[i] };
        if ns > 0 {
            used[i] = true;
        }
        field.retarget(t, next, target(j), delay(j), Some(start));
        ids.push(next);
        next += 1;
    }
    // Sources nobody took fade into the nearest cell.
    for (i, s) in src.iter().enumerate().filter(|(i, _)| !used[*i]) {
        let j = (i * nd / ns).min(nd - 1);
        field.retarget(t, next, Look { bright: 0.0, ..target(j) }, delay(j), Some(*s));
        next += 1;
    }
    ids
}

/// Several dot sources drawn as one, each moved by a cell offset. Use it to keep a waveform
/// and a particle field on one grid, so dots can pass from one to the other.
#[derive(Clone, Debug, Default)]
pub struct DotStack {
    pub layers: Vec<(Arc<dyn DotSource>, (f32, f32))>,
}

impl DotStack {
    pub fn new() -> DotStack {
        DotStack::default()
    }

    pub fn with(mut self, source: Arc<dyn DotSource>, offset: (f32, f32)) -> DotStack {
        self.layers.push((source, offset));
        self
    }
}

impl DotSource for DotStack {
    fn dots(&self, t: f64, out: &mut Vec<Dot>) {
        for (s, (ox, oy)) in &self.layers {
            let start = out.len();
            s.dots(t, out);
            if *ox != 0.0 || *oy != 0.0 {
                for d in &mut out[start..] {
                    d.x += ox;
                    d.y += oy;
                }
            }
        }
    }

    fn moving(&self, t: f64) -> bool {
        self.layers.iter().any(|(s, _)| s.moving(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::Springs;
    use vo_scene::geom::Color;

    #[test]
    fn check_is_two_connected_strokes() {
        let c = check_cells(9, 7, 1);
        // Bottom of the check near a third of the width, top right corner reached.
        assert!(c.contains(&(8, 0)));
        let bottom = c.iter().filter(|p| p.1 == 6).map(|p| p.0).collect::<Vec<_>>();
        assert!(bottom.iter().all(|&x| (2..=4).contains(&x)), "{bottom:?}");
        // Every column has a cell: the strokes have no gaps.
        for x in 0..9 {
            assert!(c.iter().any(|p| p.0 == x), "column {x}");
        }
    }

    #[test]
    fn peeled_dots_leave_left_to_right_and_land_on_cells() {
        let mut f = DotField::new(0.0, Springs::default());
        let ids: Vec<u32> = (0..20).collect();
        for &i in &ids {
            f.place(0.0, i, Look::new(i as f32, 0.0, 0.6, Color::WHITE));
        }
        peel_off(&mut f, 1.0, &ids, &Peel::default());
        let moved = |id: u32| {
            let l = f.look(1.1, id).unwrap();
            (l.x - id as f32).abs() + l.y.abs()
        };
        assert!(moved(0) > moved(19));
        for &i in &ids {
            let to = f.target(i).unwrap();
            assert_eq!((to.x, to.y), (to.x.round(), to.y.round()));
            assert_eq!(to.bright, 0.0);
        }
        f.prune(f.rest_at());
        assert!(f.is_empty());
    }

    #[test]
    fn assembled_shape_is_exact_at_rest() {
        let mut f = DotField::new(0.0, Springs::default());
        let cells = check_cells(9, 7, 1);
        let from: Vec<Look> = (0..40).map(|i| Look::new(i as f32 * 0.5, 10.0, 0.4, Color::WHITE)).collect();
        let look = Look::new(0.0, 0.0, 0.6, Color::WHITE);
        let ids = assemble(&mut f, 0.0, &from, &cells, (3.0, 1.0), look, 100, 0.012, 0.2);
        assert_eq!(ids.len(), cells.len());
        let end = f.rest_at();
        f.prune(end);
        let mut v = Vec::new();
        f.sample(end, &mut v);
        let mut got: Vec<(i32, i32)> = v.iter().map(|d| (d.x as i32 - 3, d.y as i32 - 1)).collect();
        got.sort();
        assert_eq!(got, cells);
    }

    #[test]
    fn a_stack_offsets_its_layers() {
        let mut a = DotField::new(0.0, Springs::default());
        a.place(0.0, 1, Look::new(1.0, 1.0, 0.5, Color::WHITE));
        let a: Arc<dyn DotSource> = Arc::new(a);
        let s = DotStack::new().with(a.clone(), (0.0, 0.0)).with(a, (10.0, 2.0));
        let mut v = Vec::new();
        s.dots(0.0, &mut v);
        assert_eq!((v[1].x, v[1].y), (11.0, 3.0));
        assert!(!s.moving(0.0));
    }
}
