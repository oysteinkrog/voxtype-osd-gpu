//! A line trace over the audio level (`Element::trace`): one smooth line rebuilt each frame,
//! with phase-shifted ghost copies behind it and a flat horizon line that fades at both ends
//! (FX-110, concepts 16 and 18).
//!
//! The line is a carrier wave whose height follows the level. Two ways to map the level:
//!
//! - [`TraceMode::Live`]: the whole line follows the level now, under a bell across the box,
//!   so it swells in the middle while the user speaks (concept 16).
//! - [`TraceMode::History`]: each x reads the level at its own time, newest on the right, so
//!   the line is a scrolling oscilloscope of the history (concept 18 with a dense carrier).
//!
//! The carrier moves with time until the newest level (or until the end of `settle`), then
//! holds still. Like `Bars`, every frame is a pure function of `t` and the fields.
//!
//! The vector shader loops over every segment of a stroke for every pixel of its box, so a
//! long line is cut into runs of `run` segments, each its own stroke with a small box. Runs
//! share their end points; with a translucent colour or a glow the shared point blends twice,
//! so keep `run` high for faint ghost lines and glows. The `wave_fx` example measures the cost.

use crate::canvas::Painter;
use crate::display::{Paint, ShapeInstance};
use crate::element::Element;
use crate::geom::{Color, Point, Rect};
use crate::levels::{smoothstep, Levels};
use crate::path::{Contour, StrokeStyle};

/// How the level maps onto x. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TraceMode {
    Live,
    /// `window` seconds of history across the box.
    History {
        window: f32,
    },
}

/// See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    pub levels: Levels,
    pub mode: TraceMode,
    /// Carrier waves across the box.
    pub cycles: f32,
    /// Carrier speed in cycles a second; positive moves the crests right.
    pub speed: f32,
    /// How much of a second, faster carrier is mixed in, 0 to 1, for a less regular line.
    pub detail: f32,
    /// Height at level 1 and at silence, as fractions of half the box height.
    pub gain: f32,
    pub floor: f32,
    /// Phase-shifted copies drawn behind the line: how many, the phase step between them in
    /// turns, the height factor and the alpha of the first one (later ones fade further).
    pub ghosts: u32,
    pub ghost_phase: f32,
    pub ghost_scale: f32,
    pub ghost_alpha: f32,
    /// Fraction of the width at each end over which the height falls to 0 and the line fades
    /// out, so it lands on the horizon.
    pub fade: f32,
    pub width: f32,
    pub glow: f32,
    pub color: Color,
    /// Ghost colour; `None` uses `color`.
    pub ghost_color: Option<Color>,
    /// A thin bright line on top of the main line, `core_width` of its width.
    pub core: Option<Color>,
    pub core_width: f32,
    /// The flat line across the whole box, fading to nothing at both ends.
    pub horizon: Option<Color>,
    /// From this time on, the height falls to the floor (to a flat line with `floor` 0) over
    /// `settle_secs`.
    pub settle: Option<f64>,
    pub settle_secs: f32,
    /// Logical units between the points of the line.
    pub step: f32,
    /// Segments per stroke run. See the module docs.
    pub run: usize,
}

impl Default for Trace {
    fn default() -> Trace {
        Trace {
            levels: Levels::default(),
            mode: TraceMode::Live,
            cycles: 5.0,
            speed: 0.6,
            detail: 0.35,
            gain: 0.9,
            floor: 0.04,
            ghosts: 2,
            ghost_phase: 0.11,
            ghost_scale: 0.8,
            ghost_alpha: 0.45,
            fade: 0.18,
            width: 2.0,
            glow: 6.0,
            color: Color::hex(0x38bdf8, 1.0),
            ghost_color: None,
            core: Some(Color::hex(0xe0f7ff, 1.0)),
            core_width: 0.4,
            horizon: Some(Color::hex(0x38bdf8, 0.7)),
            settle: None,
            settle_secs: 0.4,
            step: 2.0,
            run: 48,
        }
    }
}

impl Trace {
    /// The time after which the picture holds still.
    pub fn until(&self) -> f64 {
        let end = self.settle.map_or(f64::NEG_INFINITY, |s| s + self.settle_secs as f64);
        self.levels.newest_at.max(end)
    }

    pub fn moving(&self, t: f64) -> bool {
        t < self.until()
    }

    /// The height multiplier at `t`: 1, falling smoothly to 0 after `settle`.
    pub fn envelope(&self, t: f64) -> f32 {
        let Some(s) = self.settle else { return 1.0 };
        1.0 - smoothstep(0.0, 1.0, ((t - s) / self.settle_secs.max(1e-3) as f64) as f32)
    }

    /// The points of the line `k` (0 the main line, then the ghosts) in `rect` at `t`.
    pub fn points(&self, rect: Rect, t: f64, k: u32) -> Vec<Point> {
        let tp = t.min(self.until());
        let reader = self.levels.reader();
        let edge = tp.min(self.levels.newest_at);
        let now = reader.at(edge);
        let env = self.envelope(tp);
        let half = 0.5 * rect.h;
        let mid = rect.y + half;
        let steps = (rect.w / self.step.max(0.25)).ceil().max(2.0) as usize;
        let phase = self.speed as f64 * tp + (k as f64 * self.ghost_phase as f64);
        let scale = self.ghost_scale.powi(k as i32);
        let tau = std::f64::consts::TAU;
        (0..=steps)
            .map(|i| {
                let u = i as f32 / steps as f32;
                let level = match self.mode {
                    TraceMode::Live => now * (std::f32::consts::PI * u).sin().max(0.0).powf(1.5),
                    TraceMode::History { window } => reader.at(edge - (1.0 - u as f64) * window as f64),
                };
                let ends = smoothstep(0.0, self.fade, u) * smoothstep(0.0, self.fade, 1.0 - u);
                let amp = (self.floor + (self.gain - self.floor) * level * env) * ends * scale * half;
                let x = u as f64 * self.cycles as f64;
                let a = (tau * (x - phase)).sin();
                let b = (tau * (2.37 * x + 1.6 * phase + 0.3)).sin();
                let y = (1.0 - self.detail) as f64 * a + self.detail as f64 * b;
                Point::new(rect.x + u * rect.w, mid - amp * y as f32)
            })
            .collect()
    }

    /// Draws the trace in `rect` at `t`.
    pub fn draw(&self, t: f64, rect: Rect, p: &mut Painter) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        if let Some(c) = self.horizon {
            horizon(rect.x, rect.right(), rect.y + 0.5 * rect.h, 0.5 * self.width.min(1.5), c, p);
        }
        let fade_len = self.fade * rect.w;
        let ghost = self.ghost_color.unwrap_or(self.color);
        for k in (1..=self.ghosts).rev() {
            let a = self.ghost_alpha * (1.0 - (k - 1) as f32 / self.ghosts as f32);
            let style = StrokeStyle::solid(self.width * 0.75, ghost.fade(a));
            self.stroke(&self.points(rect, t, k), fade_len, &style, p);
        }
        let pts = self.points(rect, t, 0);
        self.stroke(&pts, fade_len, &StrokeStyle::solid(self.width, self.color).glow(self.glow), p);
        if let Some(c) = self.core {
            self.stroke(&pts, fade_len, &StrokeStyle::solid(self.width * self.core_width, c), p);
        }
    }

    /// Strokes `pts` in runs, the colour fading in over the first `fade_len` and out over the
    /// last.
    fn stroke(&self, pts: &[Point], fade_len: f32, style: &StrokeStyle, p: &mut Painter) {
        let c = Contour::polyline(pts, false);
        let len = c.length();
        let (on, off) = (style.c0, style.c0.fade(0.0));
        for run in c.segs.chunks(self.run.max(1)) {
            let (s0, s1) = (run[0].s0, run[run.len() - 1].s1);
            // A run before the fade-out takes the fade-in ramp, which is full colour past
            // `fade_len`; a run after the fade-in takes the fade-out ramp.
            let style = if s1 <= len - fade_len {
                style.ramp(0.0, off, fade_len, on)
            } else if s0 >= fade_len {
                style.ramp(len - fade_len, on, len, off)
            } else {
                // A run that holds both fades (short lines): fade in only.
                style.ramp(0.0, off, fade_len, on)
            };
            p.stroke(&Contour { segs: run.to_vec(), closed: false }, &style);
        }
    }
}

/// A flat line from `x0` to `x1` at `y`, `half` thick each side, full colour in the middle
/// fading to nothing at both ends. Two bands that tile with no seam.
pub fn horizon(x0: f32, x1: f32, y: f32, half: f32, c: Color, p: &mut Painter) {
    let xm = 0.5 * (x0 + x1);
    let left = Paint::linear(Point::new(x0, y), c.fade(0.0), Point::new(xm, y), c);
    let right = Paint::linear(Point::new(xm, y), c, Point::new(x1, y), c.fade(0.0));
    p.shape(ShapeInstance::band(x0, xm, y, half, half, left));
    p.shape(ShapeInstance::band(xm, x1, y, half, half, right));
}

impl<M> Element<M> {
    /// A line trace filling the box (`crate::trace`). Give it a size: it has no content size.
    pub fn trace(key: impl Into<crate::element::Key>, trace: Trace) -> Element<M> {
        let until = trace.until();
        Element::canvas(key, until, move |t, rect, p| trace.draw(t, rect, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::ItemKind;
    use crate::scene::Scene;

    fn speech(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| ((i as f32 * 0.21).sin().abs() * (0.5 + 0.5 * (i as f32 * 0.023).sin())).clamp(0.0, 1.0))
            .collect()
    }

    fn trace(mode: TraceMode) -> Trace {
        Trace { levels: Levels::new(speech(400), 4.0), mode, ..Trace::default() }
    }

    const R: Rect = Rect { x: 0.0, y: 0.0, w: 200.0, h: 40.0 };

    #[test]
    fn the_line_lands_on_the_horizon_at_both_ends() {
        for mode in [TraceMode::Live, TraceMode::History { window: 2.0 }] {
            let pts = trace(mode).points(R, 3.5, 0);
            assert_eq!(pts.first().unwrap().y, 20.0);
            assert_eq!(pts.last().unwrap().y, 20.0);
            assert!(pts.iter().any(|q| (q.y - 20.0).abs() > 4.0), "{mode:?} swings");
            assert!(pts.iter().all(|q| q.y >= 0.0 && q.y <= 40.0));
        }
    }

    #[test]
    fn loud_levels_swing_wider_than_quiet_ones() {
        let swing = |level: f32| {
            let t = Trace { levels: Levels::new(vec![level; 200], 2.0), ..Trace::default() };
            t.points(R, 2.0, 0).iter().map(|q| (q.y - 20.0).abs()).fold(0.0, f32::max)
        };
        assert!(swing(1.0) > 3.0 * swing(0.1), "{} {}", swing(1.0), swing(0.1));
    }

    #[test]
    fn it_holds_still_after_the_newest_level_and_settles_flat() {
        let t = trace(TraceMode::Live);
        assert_eq!(t.points(R, 4.0, 0), t.points(R, 9.0, 0));
        assert_ne!(t.points(R, 3.0, 0), t.points(R, 3.1, 0));
        let s = Trace { settle: Some(4.0), floor: 0.0, ..t };
        assert!(s.moving(4.3) && !s.moving(4.41));
        assert!(s.points(R, 4.5, 0).iter().all(|q| (q.y - 20.0).abs() < 1e-4));
    }

    #[test]
    fn ghosts_are_shifted_and_smaller() {
        let t = trace(TraceMode::Live);
        let (a, b) = (t.points(R, 3.5, 0), t.points(R, 3.5, 1));
        assert_ne!(a, b);
        let peak = |v: &[Point]| v.iter().map(|q| (q.y - 20.0).abs()).fold(0.0, f32::max);
        assert!(peak(&b) < peak(&a));
    }

    #[test]
    fn long_lines_are_cut_into_runs_that_meet() {
        let t = Trace { run: 16, ghosts: 0, core: None, horizon: None, ..trace(TraceMode::Live) };
        let mut scene: Scene = Scene::new();
        scene.apply(Element::column("root").child(Element::trace("t", t).size(200.0, 40.0)), 0.0);
        let list = scene.sample(3.5);
        let strokes: Vec<_> = list.vectors.iter().collect();
        // 100 segments in runs of 16.
        assert_eq!(strokes.len(), 7);
        for w in strokes.windows(2) {
            let end = list.vector_segs[(w[0].segs[0] + w[0].segs[1] - 1) as usize].p2;
            assert_eq!(end, list.vector_segs[w[1].segs[0] as usize].p0);
        }
        assert!(list.items.iter().any(|i| i.kind == ItemKind::Vectors));
        // The first run fades in and the last fades out.
        assert_eq!(strokes[0].c0.a, 0.0);
        assert_eq!(strokes[6].c1.a, 0.0);
    }

    #[test]
    fn the_scene_wakes_until_the_trace_settles() {
        let t = Trace { settle: Some(4.0), ..trace(TraceMode::History { window: 2.0 }) };
        let mut scene: Scene = Scene::new();
        scene.apply(Element::column("root").child(Element::trace("t", t).size(200.0, 40.0)), 0.0);
        assert_eq!(scene.next_wake(4.2), Some(4.2));
        assert_eq!(scene.next_wake(4.5), None);
    }
}
