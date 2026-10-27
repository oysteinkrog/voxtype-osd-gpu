//! Segment meters (`Element::segments`): a row, a column or an arc of cells lit by the level,
//! with a colour per cell, a brightness ramp, a peak hold cell and a clip cell (FX-095). For
//! the LED input meter (concept 18), the vertical MAX/MIN meter (concept 23) and the arc of
//! ticks round a dial (concept 02).
//!
//! The level comes from a [`Levels`] history through its follower. The peak is the highest
//! followed level over the last `hold` seconds, then falls at `fall` per second, so every
//! frame is still a pure function of `t`. The clip cell lights while any raw level in the
//! last `hold` seconds reached `clip_at`.
//!
//! Cells are rounded rects in a row or column. On an arc they are radial ticks (round-capped
//! lines) from `inner` to the outer radius, which is half the box's shorter side.

use crate::canvas::Painter;
use crate::display::{Paint, ShapeInstance};
use crate::element::Element;
use crate::geom::{Color, Point, RRect, Rect};
use crate::levels::{Levels, Stops};

/// Where the cells go. Rows fill from the left, columns from the bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SegmentLayout {
    Row,
    Column,
    /// From `start` through `sweep` radians (clockwise on screen, 0 pointing right), ticks
    /// from `inner` times the outer radius to the outer radius, `width` thick.
    Arc { start: f32, sweep: f32, inner: f32, width: f32 },
}

/// See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Segments {
    pub levels: Levels,
    pub cells: u32,
    pub layout: SegmentLayout,
    /// The gap between cells as a fraction of the cell pitch, and the corner radius as a
    /// fraction of the cell's shorter side.
    pub gap: f32,
    pub radius: f32,
    /// Lit colour by cell position, 0 for the first cell to 1 for the last.
    pub on: Stops,
    /// Unlit cells.
    pub off: Color,
    /// The alpha of the first lit cell; it rises to 1 at the top of the lit run.
    pub ramp: f32,
    /// The cell at the level's edge lights by the fraction the level covers it.
    pub partial: bool,
    /// Peak hold: seconds held, then the fall in level per second, and its colour (`None`
    /// uses the cell's lit colour).
    pub peak: Option<(f32, f32, Option<Color>)>,
    /// The last cell lights in `clip` while a raw level in the last `peak` hold time (or
    /// 1 s) reached `clip_at`.
    pub clip: Option<Color>,
    pub clip_at: f32,
    /// A glow under lit cells: radius and strength.
    pub glow: Option<(f32, f32)>,
}

impl Default for Segments {
    fn default() -> Segments {
        Segments {
            levels: Levels::default(),
            cells: 16,
            layout: SegmentLayout::Row,
            gap: 0.3,
            radius: 0.2,
            on: Stops::new(&[
                (0.0, Color::hex(0x22c55e, 1.0)),
                (0.7, Color::hex(0x22c55e, 1.0)),
                (0.85, Color::hex(0xfacc15, 1.0)),
                (1.0, Color::hex(0xef4444, 1.0)),
            ]),
            off: Color::hex(0x22c55e, 0.15),
            ramp: 0.55,
            partial: false,
            peak: Some((0.8, 0.6, None)),
            clip: Some(Color::hex(0xef4444, 1.0)),
            clip_at: 0.99,
            glow: None,
        }
    }
}

impl Segments {
    fn hold(&self) -> f32 {
        self.peak.map_or(1.0, |p| p.0)
    }

    /// The time after which the meter holds still.
    pub fn until(&self) -> f64 {
        let fall = self.peak.map_or(0.0, |(h, f, _)| h + 1.0 / f.max(1e-3));
        self.levels.newest_at + fall.max(self.hold()) as f64
    }

    /// The level, the peak and whether the clip cell is lit at `t`.
    pub fn read(&self, t: f64) -> (f32, f32, bool) {
        let l = &self.levels;
        let followed = l.followed();
        let reader = l.reader();
        let level = reader.at(t.min(l.newest_at));
        let index = |at: f64| ((at - l.first_at()) * l.rate as f64).floor();
        let n = followed.len() as f64;
        let hold = self.hold() as f64;
        let (lo, hi) = (index(t - hold).clamp(0.0, n), (index(t) + 1.0).clamp(0.0, n));
        let clip = l.levels[lo as usize..hi as usize].iter().any(|&x| x >= self.clip_at);
        let Some((hold, fall, _)) = self.peak else { return (level, level, clip) };
        // For each level in the window, its value now: held, then falling.
        let start = index(t - hold as f64 - 1.0 / fall.max(1e-3) as f64).clamp(0.0, hi) as usize;
        let mut peak = level;
        for (i, &y) in followed.iter().enumerate().take(hi as usize).skip(start) {
            let at = l.first_at() + i as f64 / l.rate as f64;
            let age = (t - at) as f32;
            peak = peak.max(y - fall * (age - hold).max(0.0));
        }
        (level, peak, clip)
    }

    /// Draws the meter in `rect` at `t`.
    pub fn draw(&self, t: f64, rect: Rect, p: &mut Painter) {
        let n = self.cells.max(1);
        let (level, peak, clip) = self.read(t);
        let lit = level * n as f32;
        let peak_cell = ((peak * n as f32).ceil() as u32).clamp(1, n) - 1;
        let top = lit.ceil().max(1.0);
        for i in 0..n {
            let f = if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 };
            let cover = (lit - i as f32).clamp(0.0, 1.0);
            let cover = if self.partial { cover } else { cover.ceil() };
            let ramp = self.ramp + (1.0 - self.ramp) * ((i as f32 + 1.0) / top).min(1.0);
            let mut color = self.on.at(f).fade(cover * ramp);
            if self.peak.is_some() && i == peak_cell && peak > level + 0.5 / n as f32 {
                color = self.peak.and_then(|p| p.2).unwrap_or(self.on.at(f));
            }
            if i == n - 1 && clip {
                if let Some(c) = self.clip {
                    color = c;
                }
            }
            let shape = self.cell(rect, i, n);
            p.shape(paint(shape, self.off));
            if color.a > 0.0 {
                if let Some((radius, strength)) = self.glow {
                    p.shape(ShapeInstance { fill: Paint::glow(color, radius, strength * color.a), ..shape });
                }
                p.shape(paint(shape, color));
            }
        }
    }

    /// Cell `i` of `n` as a shape with no paint yet.
    fn cell(&self, rect: Rect, i: u32, n: u32) -> ShapeInstance {
        let none = Paint::NONE;
        match self.layout {
            SegmentLayout::Row | SegmentLayout::Column => {
                let row = self.layout == SegmentLayout::Row;
                let long = if row { rect.w } else { rect.h };
                let pitch = long / n as f32;
                let len = pitch * (1.0 - self.gap.clamp(0.0, 0.9));
                let off = i as f32 * pitch + 0.5 * (pitch - len);
                let r = if row {
                    Rect::new(rect.x + off, rect.y, len, rect.h)
                } else {
                    Rect::new(rect.x, rect.bottom() - off - len, rect.w, len)
                };
                ShapeInstance::rrect(RRect::new(r, self.radius * r.w.min(r.h)), none)
            }
            SegmentLayout::Arc { start, sweep, inner, width } => {
                let c = Point::new(rect.x + 0.5 * rect.w, rect.y + 0.5 * rect.h);
                let outer = 0.5 * rect.w.min(rect.h) - 0.5 * width;
                let a = start + sweep * (i as f32 + 0.5) / n as f32;
                let (s, co) = a.sin_cos();
                let at = |r: f32| Point::new(c.x + r * co, c.y + r * s);
                ShapeInstance::line(at(outer * inner.clamp(0.0, 1.0)), at(outer), width, none)
            }
        }
    }
}

/// `shape` painted `c`: the fill for a rect, the stroke for a tick.
fn paint(shape: ShapeInstance, c: Color) -> ShapeInstance {
    match shape.kind {
        crate::display::ShapeKind::Line => ShapeInstance { stroke: Paint::solid(c), ..shape },
        _ => ShapeInstance { fill: Paint::solid(c), ..shape },
    }
}

impl<M> Element<M> {
    /// A segment meter filling the box (`crate::segments`). Give it a size.
    pub fn segments(key: impl Into<crate::element::Key>, meter: Segments) -> Element<M> {
        let until = meter.until();
        Element::canvas(key, until, move |t, rect, p| meter.draw(t, rect, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    fn fast(levels: Vec<f32>, newest_at: f64) -> Levels {
        Levels { attack: 1e-4, release: 1e-4, ..Levels::new(levels, newest_at) }
    }

    fn lit(m: &Segments, t: f64) -> Vec<f32> {
        let mut scene: Scene = Scene::new();
        scene.apply(Element::column("root").child(Element::segments("m", m.clone()).size(160.0, 10.0)), 0.0);
        let list = scene.sample(t);
        // Every cell draws its unlit base, then a lit copy when lit.
        let mut out = Vec::new();
        let mut it = list.shapes.iter().peekable();
        while let Some(base) = it.next() {
            let a = match it.peek() {
                Some(s) if s.geom == base.geom => it.next().unwrap().fill.c0.a,
                _ => 0.0,
            };
            out.push(a);
        }
        out
    }

    #[test]
    fn cells_light_up_to_the_level_with_a_ramp() {
        let m = Segments { peak: None, clip: None, ..Segments::default() };
        let m = Segments { levels: fast(vec![0.5; 100], 1.0), ..m };
        let a = lit(&m, 1.0);
        assert_eq!(a.len(), 16);
        assert!(a[..8].iter().all(|&x| x > 0.5) && a[8..].iter().all(|&x| x == 0.0), "{a:?}");
        assert!(a[0] < a[7] && (a[7] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_peak_holds_then_falls() {
        let mut levels = vec![0.9f32; 50];
        levels.extend([0.2; 150]);
        let m = Segments { clip: None, levels: fast(levels, 2.0), ..Segments::default() };
        let (_, peak, _) = m.read(1.0);
        assert!((peak - 0.9).abs() < 1e-3);
        // 0.8 s hold from the last loud level at 0.5 s, then 0.6 a second.
        let (level, peak, _) = m.read(1.5);
        assert!((level - 0.2).abs() < 1e-3 && (peak - (0.9 - 0.6 * 0.2)).abs() < 0.02, "{peak}");
        let (_, peak, _) = m.read(2.6);
        assert!((peak - 0.2).abs() < 1e-3);
        let a = lit(&m, 1.0);
        assert!(a[14] == 1.0 && a[13] == 0.0, "peak cell lit alone: {a:?}");
    }

    #[test]
    fn the_clip_cell_lights_after_a_full_level() {
        let mut levels = vec![0.3f32; 100];
        levels[60] = 1.0;
        let m = Segments { peak: None, levels: fast(levels, 1.0), ..Segments::default() };
        assert!(m.read(0.9).2);
        assert!(!m.read(1.7).2);
    }

    #[test]
    fn an_arc_puts_ticks_round_the_centre() {
        let m = Segments {
            layout: SegmentLayout::Arc { start: 0.0, sweep: std::f32::consts::PI, inner: 0.8, width: 2.0 },
            cells: 3,
            ..Segments::default()
        };
        let s = m.cell(Rect::new(0.0, 0.0, 100.0, 100.0), 1, 3);
        assert_eq!(s.kind, crate::display::ShapeKind::Line);
        // The middle tick of a half turn from +x points straight down.
        assert!((s.geom[0] - 50.0).abs() < 1e-3 && (s.geom[3] - 99.0).abs() < 1e-3, "{:?}", s.geom);
    }
}
