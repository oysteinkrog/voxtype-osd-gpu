//! A bar-strip waveform over the audio level history (`Element::bars`): one round-capped bar
//! per time slot, newest on the right, scrolling left.
//!
//! The level is smoothed by an attack and release follower (60 ms and 200 ms time constants
//! by default), so bars rise fast and fall slowly. When recording stops, `settle` brings every
//! bar down to a dot over 400 ms, which leaves a flat dotted line. A `playhead` splits the
//! strip into an accent part on the left and a grey part on the right, inside a bar if it falls
//! there.
//!
//! Like `Wave`, it is a pure function of `t` and its fields. Bars sit on a grid of slot times
//! counted from 0, so a bar keeps its height while it scrolls. The follower runs over the
//! whole level history each frame and starts from the oldest level, so keep about a second of
//! history beyond what the strip shows.
//!
//! Bars v2 (FX-105 to FX-107) adds looks on the same strip, all off by default:
//!
//! - `colour`: one accent, colour stops across the strip, stops by the bar's level (so the
//!   loudest bars turn white), or an accent centre that fades to `rest` outside it.
//! - `core`: a soft white-hot core inside each bar, longer on loud bars, with the bar's own
//!   colour left at its edges (concept 23). `glow` puts a glow under each bar.
//! - `taper`: near both ends of the strip the heights fall to dots that shrink and fade, a
//!   dotted tail.
//! - `hairline`: one-pixel lines instead of round bars, and `baseline` a hairline along the
//!   strip.
//! - `layout`: `Mirror` puts the newest bar in the middle with the history spreading out to
//!   both sides. `anchor` stands bars on the bottom edge instead of the centre line, and
//!   `reflection` draws a faded mirror image under them.

use std::sync::Arc;

use crate::display::{Paint, ShapeInstance};
use crate::geom::{Color, Point, RRect, Rect};
use crate::levels::{mix, smoothstep, Stops};

/// How bars take their colour.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum BarColour {
    /// `accent` for every bar.
    #[default]
    Accent,
    /// Stops across the strip, 0 at the left edge to 1 at the right.
    Across(Stops),
    /// Stops by the bar's level, 0 silent to 1 full.
    ByLevel(Stops),
    /// `accent` within `span` of the width round the middle, mixing to `rest` over `soft`
    /// more on each side.
    Centre { span: f32, soft: f32 },
}

/// A soft bright core inside each bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Core {
    pub color: Color,
    /// Width as a fraction of the bar width, and length as a fraction of the bar length at
    /// level 1 (quiet bars get a shorter core).
    pub width: f32,
    pub length: f32,
    /// Blur as a fraction of the bar width.
    pub blur: f32,
}

impl Default for Core {
    fn default() -> Core {
        Core { color: Color::hex(0xfff7d6, 1.0), width: 0.45, length: 0.8, blur: 0.35 }
    }
}

/// Dotted tails: over `span` of the width at each end the heights fall to dots, which shrink
/// to `end_size` and fade to `end_alpha` at the very ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Taper {
    pub span: f32,
    pub end_size: f32,
    pub end_alpha: f32,
}

impl Default for Taper {
    fn default() -> Taper {
        Taper { span: 0.2, end_size: 0.4, end_alpha: 0.35 }
    }
}

/// Where the newest bar sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BarLayout {
    /// On the right, the strip scrolling left.
    #[default]
    Scroll,
    /// In the middle, the history spreading out to both sides.
    Mirror,
}

/// What the bars stand on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    /// Centred on the middle line, growing up and down.
    #[default]
    Centre,
    /// Standing on the bottom edge (or on the reflection line with `reflection`).
    Bottom,
}

/// See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Bars {
    /// Levels from 0 to 1, oldest first, evenly spaced in time.
    pub levels: Arc<[f32]>,
    /// Scene time of the newest level. The strip scrolls until then and stays still after.
    pub newest_at: f64,
    /// Levels per second.
    pub rate: f32,
    /// Bars across the box.
    pub count: u32,
    /// Seconds of history per bar.
    pub slot: f32,
    /// Bar width as a fraction of the bar pitch.
    pub width: f32,
    /// Follower time constants in seconds: rising and falling.
    pub attack: f32,
    pub release: f32,
    /// From this time on, heights fall to dots over `settle_secs`.
    pub settle: Option<f64>,
    pub settle_secs: f32,
    /// Where the colour changes, 0 to 1 across the box: `accent` on the left, `rest` on the
    /// right. `None` draws every bar in `accent`.
    pub playhead: Option<f32>,
    pub accent: Color,
    pub rest: Color,
    pub colour: BarColour,
    pub core: Option<Core>,
    /// A glow under each bar: radius and strength.
    pub glow: Option<(f32, f32)>,
    pub taper: Option<Taper>,
    /// One-pixel lines instead of round bars.
    pub hairline: bool,
    /// A hairline along the strip: on the centre line, or under the bars with `Anchor::Bottom`.
    pub baseline: Option<Color>,
    pub layout: BarLayout,
    pub anchor: Anchor,
    /// With `Anchor::Bottom`, the alpha of a mirror image under the bars (0 for none). The
    /// bars then stand on a line at `1 / (1 + reflection_size)` of the height.
    pub reflection: f32,
    pub reflection_size: f32,
}

impl Default for Bars {
    fn default() -> Bars {
        Bars {
            levels: Arc::from([]),
            newest_at: 0.0,
            rate: 100.0,
            count: 32,
            slot: 0.06,
            width: 0.55,
            attack: 0.060,
            release: 0.200,
            settle: None,
            settle_secs: 0.4,
            playhead: None,
            accent: Color::WHITE,
            rest: Color::hex(0x6b7280, 1.0),
            colour: BarColour::Accent,
            core: None,
            glow: None,
            taper: None,
            hairline: false,
            baseline: None,
            layout: BarLayout::Scroll,
            anchor: Anchor::Centre,
            reflection: 0.0,
            reflection_size: 0.35,
        }
    }
}

impl Bars {
    /// Whether the strip still changes at `t`: while it scrolls or settles.
    pub fn moving(&self, t: f64) -> bool {
        (self.levels.len() > 1 && t < self.newest_at) || self.settle.is_some_and(|s| t < s + self.settle_secs as f64)
    }

    /// The height multiplier at `t`: 1, falling smoothly to 0 after `settle`.
    pub fn envelope(&self, t: f64) -> f32 {
        let Some(s) = self.settle else { return 1.0 };
        let u = ((t - s) / self.settle_secs.max(1e-3) as f64).clamp(0.0, 1.0) as f32;
        1.0 - u * u * (3.0 - 2.0 * u)
    }

    /// The followed level at every input level.
    pub fn followed(&self) -> Vec<f32> {
        crate::levels::follow(&self.levels, self.rate, self.attack, self.release)
    }

    /// The bars for `rect` at `t`, with `alpha` multiplied in.
    pub fn shapes(&self, rect: Rect, t: f64, alpha: f32, out: &mut Vec<ShapeInstance>) {
        let n = self.levels.len();
        if self.count == 0 || self.slot <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let pitch = rect.w / self.count as f32;
        let bar_w = (pitch * self.width).clamp(0.5, rect.h);
        let r = 0.5 * bar_w;
        let env = self.envelope(t);
        let followed = self.followed();
        let first_at = self.newest_at - n.saturating_sub(1) as f64 / self.rate as f64;
        let level_at = |at: f64| -> f32 {
            if n == 0 || at < first_at - 1e-9 {
                return 0.0;
            }
            let f = ((at - first_at) * self.rate as f64).min((n - 1) as f64);
            let i = f.floor() as usize;
            let k = (f - i as f64) as f32;
            let a = followed[i];
            let b = followed[(i + 1).min(n - 1)];
            a + (b - a) * k
        };
        // The line the bars grow from, and how far they may grow.
        let (base, max_len) = match self.anchor {
            Anchor::Centre => (rect.y + 0.5 * rect.h, 0.5 * rect.h),
            Anchor::Bottom => {
                let h = rect.h / (1.0 + if self.reflection > 0.0 { self.reflection_size } else { 0.0 });
                (rect.y + h, h)
            }
        };
        if let Some(c) = self.baseline {
            let (a, b) = (Point::new(rect.x, base), Point::new(rect.right(), base));
            out.push(ShapeInstance::line(a, b, 1.0, Paint::NONE).with_hairline(Paint::solid(c.fade(alpha))));
        }
        let edge = t.min(self.newest_at);
        let slot = self.slot as f64;
        let px_per_s = pitch as f64 / slot;
        let newest_slot = (edge / slot).floor();
        let split = self.playhead.map(|p| rect.x + p.clamp(0.0, 1.0) * rect.w);
        // Bar centres and levels. One extra slot on the outside, fading as it scrolls past
        // the edge.
        let mut bars: Vec<(f32, f32, f32)> = Vec::with_capacity(self.count as usize + 2);
        let mirror = self.layout == BarLayout::Mirror;
        let slots = if mirror { self.count / 2 + 1 } else { self.count } as i64;
        for j in 0..=slots {
            let at = (newest_slot - j as f64) * slot;
            let back = ((edge - at) * px_per_s) as f32;
            let level = level_at(at);
            if mirror {
                let mid = rect.x + 0.5 * rect.w;
                for side in [-1.0f32, 1.0] {
                    if side < 0.0 && back <= 1e-3 {
                        // The newest bar sits exactly in the middle: draw it once.
                        continue;
                    }
                    let cx = mid + side * back;
                    let fade = ((0.5 * rect.w - back) / (0.5 * pitch) + 0.5).clamp(0.0, 1.0);
                    bars.push((cx, level, fade));
                }
            } else {
                let cx = rect.right() - 0.5 * pitch - back;
                bars.push((cx, level, ((cx - rect.x) / (0.5 * pitch)).clamp(0.0, 1.0)));
            }
        }
        for (cx, level, fade) in bars {
            if fade <= 0.0 {
                continue;
            }
            let u = (cx - rect.x) / rect.w;
            // Dotted tails: the height falls to a dot near the ends, then the dot shrinks.
            let (mut height_k, mut size_k, mut tail_a) = (1.0, 1.0, 1.0);
            if let Some(tp) = self.taper {
                let d = u.min(1.0 - u).max(0.0);
                height_k = smoothstep(0.0, tp.span.max(1e-3), d);
                let dot = smoothstep(0.0, 0.5 * tp.span.max(1e-3), d);
                size_k = tp.end_size + (1.0 - tp.end_size) * dot;
                tail_a = tp.end_alpha + (1.0 - tp.end_alpha) * dot;
            }
            let level = env * level * height_k;
            let (w, rr) = (bar_w * size_k, r * size_k);
            let len = match self.anchor {
                Anchor::Centre => (level * max_len).max(rr),
                Anchor::Bottom => (level * max_len).max(2.0 * rr),
            };
            let a = alpha * fade * tail_a;
            let base_colour = self.colour_at(u, level);
            let (x0, x1) = (cx - 0.5 * w, cx + 0.5 * w);
            let paint = |c: Color| -> Paint {
                let (on, off) = (c.fade(a), self.rest.fade(a));
                match split {
                    Some(s) if x1 <= s => Paint::solid(on),
                    Some(s) if x0 >= s => Paint::solid(off),
                    Some(s) => Paint::linear(Point::new(s - 0.5, base), on, Point::new(s + 0.5, base), off),
                    None => Paint::solid(on),
                }
            };
            let body = match self.anchor {
                Anchor::Centre => Rect::new(x0, base - len, w, 2.0 * len),
                Anchor::Bottom => Rect::new(x0, base - len, w, len),
            };
            if self.reflection > 0.0 && self.anchor == Anchor::Bottom {
                let rl = len * self.reflection_size;
                let mirror = Rect::new(x0, base + 0.5 * rr, w, rl);
                let c = base_colour.fade(a * self.reflection);
                let fill = Paint::linear(Point::new(cx, base), c, Point::new(cx, base + rl), c.fade(0.0));
                out.push(self.bar(mirror, rr, fill));
            }
            if let Some((radius, strength)) = self.glow {
                out.push(ShapeInstance { fill: Paint::glow(base_colour.fade(a), radius, strength), ..self.bar(body, rr, Paint::NONE) });
            }
            out.push(self.bar(body, rr, paint(base_colour)));
            if let Some(core) = self.core {
                let cl = body.h * core.length * level.clamp(0.0, 1.0);
                let cw = w * core.width;
                if cl > 0.5 && cw > 0.0 {
                    let cy = match self.anchor {
                        Anchor::Centre => base,
                        Anchor::Bottom => base - 0.5 * body.h,
                    };
                    let rect = Rect::new(cx - 0.5 * cw, cy - 0.5 * cl, cw, cl);
                    let fill = Paint::shadow(core.color.fade(a), Point::ZERO, core.blur * w, 0.0);
                    out.push(ShapeInstance::rrect(RRect::new(rect, 0.5 * cw), fill));
                }
            }
        }
    }

    /// One bar's shape: a round-capped rect, or a hairline down its middle.
    fn bar(&self, r: Rect, radius: f32, fill: Paint) -> ShapeInstance {
        if self.hairline {
            let x = r.x + 0.5 * r.w;
            ShapeInstance::line(Point::new(x, r.y), Point::new(x, r.bottom()), 1.0, Paint::NONE).with_hairline(fill)
        } else {
            ShapeInstance::rrect(RRect::new(r, radius), fill)
        }
    }

    /// The colour of a bar at `u` across the strip with `level`.
    fn colour_at(&self, u: f32, level: f32) -> Color {
        match &self.colour {
            BarColour::Accent => self.accent,
            BarColour::Across(s) => s.at(u),
            BarColour::ByLevel(s) => s.at(level),
            BarColour::Centre { span, soft } => {
                let d = (u - 0.5).abs() - 0.5 * span;
                mix(self.accent, self.rest, if *soft > 0.0 { d / soft } else if d > 0.0 { 1.0 } else { 0.0 })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(levels: &[f32], newest_at: f64) -> Bars {
        Bars { levels: Arc::from(levels), newest_at, count: 10, slot: 0.1, width: 0.5, ..Bars::default() }
    }

    fn shapes(b: &Bars, t: f64) -> Vec<ShapeInstance> {
        let mut out = Vec::new();
        b.shapes(Rect::new(0.0, 0.0, 100.0, 40.0), t, 1.0, &mut out);
        out
    }

    #[test]
    fn the_follower_rises_fast_and_falls_slowly() {
        // A one-second step from 0 to 1 and back, at 100 levels a second.
        let mut levels = vec![0.0f32; 100];
        levels.extend([1.0; 100]);
        levels.extend([0.0; 100]);
        let b = Bars { levels: Arc::from(levels.as_slice()), ..Bars::default() };
        let y = b.followed();
        // One time constant after each edge: 63% of the way.
        assert!((y[100 + 5] - (1.0 - (-1.0f32).exp())).abs() < 0.03, "attack {}", y[105]);
        assert!((y[200 + 19] - (-1.0f32).exp()).abs() < 0.03, "release {}", y[219]);
        assert!(y[180] > 0.99);
    }

    #[test]
    fn bars_keep_their_height_while_they_scroll() {
        let levels: Vec<f32> = (0..400).map(|i| (i as f32 * 0.37).sin() * 0.5 + 0.5).collect();
        let b = bars(&levels, 4.0);
        // A whole slot later every bar has moved one pitch left with the same height.
        let a = shapes(&b, 3.0);
        let c = shapes(&b, 3.1);
        for (x, y) in a.iter().zip(c.iter().skip(1)) {
            assert!((x.geom[0] - 10.0 - y.geom[0]).abs() < 1e-3, "{:?} {:?}", x.geom, y.geom);
            assert!((x.geom[3] - y.geom[3]).abs() < 1e-4);
        }
        assert_eq!(shapes(&b, 4.0), shapes(&b, 9.0), "still after the newest level");
        assert!(b.moving(3.9) && !b.moving(4.0));
    }

    #[test]
    fn settle_brings_every_bar_down_to_a_dot() {
        let b = Bars { settle: Some(1.0), ..bars(&[0.8; 300], 1.0) };
        assert!(b.moving(1.2) && !b.moving(1.41));
        assert_eq!(b.envelope(0.9), 1.0);
        for s in shapes(&b, 1.41) {
            // Five units wide, so a dot is five units tall.
            assert!((s.geom[3] - 5.0).abs() < 1e-4, "{:?}", s.geom);
        }
        assert!(shapes(&b, 1.1).iter().any(|s| s.geom[3] > 10.0));
    }

    #[test]
    fn the_playhead_splits_the_colour() {
        let accent = Color::hex(0x22c55e, 1.0);
        let b = Bars { playhead: Some(0.4), accent, ..bars(&[0.5; 300], 2.95) };
        let out = shapes(&b, 2.95);
        let split = 40.0;
        let mut seen_split = false;
        for s in out.iter().filter(|s| s.fill.c0.a > 0.99) {
            let (x0, x1) = (s.geom[0], s.geom[0] + s.geom[2]);
            if x1 <= split {
                assert_eq!(s.fill, Paint::solid(accent));
            } else if x0 >= split {
                assert_eq!(s.fill, Paint::solid(b.rest));
            } else {
                seen_split = true;
                assert_ne!(s.fill, Paint::solid(accent));
            }
        }
        assert!(seen_split, "40% falls inside a bar");
    }
}

#[cfg(test)]
mod v2_tests {
    use super::*;
    use crate::display::{PaintKind, ShapeKind};

    fn loud() -> Bars {
        Bars { levels: Arc::from([0.9f32; 300].as_slice()), newest_at: 3.0, count: 20, ..Bars::default() }
    }

    fn shapes(b: &Bars) -> Vec<ShapeInstance> {
        let mut out = Vec::new();
        b.shapes(Rect::new(0.0, 0.0, 200.0, 40.0), 3.0, 1.0, &mut out);
        out
    }

    #[test]
    fn taper_brings_the_ends_down_to_small_faint_dots() {
        let b = Bars { taper: Some(Taper::default()), ..loud() };
        let out = shapes(&b);
        let first = out.iter().filter(|s| s.fill.c0.a > 0.0).min_by(|a, b| a.geom[0].total_cmp(&b.geom[0])).unwrap();
        let middle = out.iter().find(|s| (s.geom[0] - 100.0).abs() < 6.0).unwrap();
        assert!(first.geom[3] < 0.5 * b.width * 10.0 + 1e-3, "a small dot: {:?}", first.geom);
        assert!(first.fill.c0.a < 0.5);
        assert!(middle.geom[3] > 30.0);
    }

    #[test]
    fn mirror_puts_the_newest_bar_in_the_middle_once() {
        let b = Bars { layout: BarLayout::Mirror, ..loud() };
        let out = shapes(&b);
        let xs: Vec<f32> = out.iter().map(|s| s.geom[0] + 0.5 * s.geom[2]).collect();
        assert_eq!(xs.iter().filter(|&&x| (x - 100.0).abs() < 1e-3).count(), 1);
        for x in &xs {
            assert!(xs.iter().any(|y| (y + x - 200.0).abs() < 1e-3), "{x} has a mirror twin");
        }
    }

    #[test]
    fn colour_by_level_turns_loud_bars_white() {
        let stops = Stops::new(&[(0.0, Color::hex(0xff6a00, 1.0)), (0.8, Color::WHITE)]);
        let b = Bars { colour: BarColour::ByLevel(stops), ..loud() };
        let last = *shapes(&b).last().unwrap();
        assert!(last.fill.c0.g > 0.95, "{:?}", last.fill.c0);
    }

    #[test]
    fn a_core_is_a_soft_shape_inside_each_bar() {
        let b = Bars { core: Some(Core::default()), glow: Some((4.0, 1.0)), ..loud() };
        let out = shapes(&b);
        let cores: Vec<_> = out.iter().filter(|s| s.fill.kind == PaintKind::Shadow).collect();
        let glows = out.iter().filter(|s| s.fill.kind == PaintKind::Glow).count();
        assert_eq!(cores.len(), glows);
        assert!(cores.len() >= 20);
        // The core sits inside its bar: narrower and centred on the middle line.
        let c = cores.last().unwrap();
        assert!(c.geom[2] < 5.0 && (c.geom[1] + 0.5 * c.geom[3] - 20.0).abs() < 1e-3);
    }

    #[test]
    fn hairline_mode_draws_one_pixel_lines_on_a_baseline() {
        let grey = Color::hex(0x444444, 1.0);
        let b = Bars { hairline: true, baseline: Some(grey), colour: BarColour::Centre { span: 0.3, soft: 0.1 }, ..loud() };
        let out = shapes(&b);
        assert!(out.iter().all(|s| s.kind == ShapeKind::Line && s.flags & crate::display::SHAPE_HAIRLINE != 0));
        assert_eq!(out[0].geom, [0.0, 20.0, 200.0, 20.0]);
        // Centre lines in the accent, outer ones in the rest colour.
        let mid = out.iter().find(|s| (s.geom[0] - 100.0).abs() < 6.0 && s.geom[1] != s.geom[3]).unwrap();
        assert_eq!(mid.stroke.c0, b.accent);
        assert!((out[1].stroke.c0.r - b.rest.r).abs() < 1e-5);
    }

    #[test]
    fn bottom_anchor_with_a_reflection_stands_on_a_line() {
        let b = Bars { anchor: Anchor::Bottom, reflection: 0.4, ..loud() };
        let out = shapes(&b);
        let base = 40.0 / 1.35;
        let bars: Vec<_> = out.iter().filter(|s| s.fill.kind == PaintKind::Solid).collect();
        let mirrors: Vec<_> = out.iter().filter(|s| s.fill.kind == PaintKind::Linear).collect();
        assert_eq!(bars.len(), mirrors.len());
        for s in bars {
            assert!((s.geom[1] + s.geom[3] - base).abs() < 1e-3);
        }
        assert!(mirrors.iter().all(|s| s.geom[1] > base));
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;
    use crate::element::Element;
    use crate::scene::Scene;

    #[test]
    fn bars_keep_the_scene_awake_until_they_settle() {
        let mut scene: Scene = Scene::new();
        let b =
            Bars { levels: Arc::from([0.5f32; 300].as_slice()), newest_at: 3.0, settle: Some(3.0), ..Bars::default() };
        scene.apply(Element::column("root").child(Element::bars("b", b).size(160.0, 30.0)), 1.0);
        assert_eq!(scene.next_wake(2.0), Some(2.0));
        assert_eq!(scene.next_wake(3.2), Some(3.2));
        assert_eq!(scene.next_wake(3.5), None);
        assert!(!scene.sample(2.0).shapes.is_empty());
        assert_ne!(scene.sample(3.1), scene.sample(3.3));
    }
}
