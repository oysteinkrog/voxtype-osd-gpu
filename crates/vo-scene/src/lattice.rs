//! Kinetic lattice (MO-85): the words of a sentence joined by a faint lattice of thin lines
//! that encode timing, in a technical drawing style.
//!
//! Each word has a node a little under its baseline, at its left edge. Between two words
//! runs a bundle of strands that bow apart into a lens and meet at the nodes:
//!
//! - **pause is length**: the longer the pause before a word, the further its strands bow,
//!   so a slow gap draws a long slack curve and a quick one a short taut line;
//! - **loudness is thickness**: a strand is as heavy as the word it leads into was loud;
//! - **arrival tightens the lattice**: when a word arrives its strands draw on from the
//!   previous node, and every strand pulls taut and relaxes back on a damped swing.
//!
//! Faint arcs that skip one word tie the bundles into a lattice. Under each node a dashed
//! drop line ends in a small label with the pause in milliseconds.
//!
//! The words themselves are drawn by the caller with the same layout, so the reading line
//! never moves (rule 1). [`anchors`] places the nodes from that layout.
//!
//! The drawing is a pure function of `t` and the words' arrival times. Under reduced
//! motion strands are drawn whole and still, at their resting bow.

use std::sync::Arc;

use crate::canvas::Painter;
use crate::display::{Paint, ShapeInstance};
use crate::element::{Element, Key, TextStyle};
use crate::geom::{Color, Point, Rect, Size};
use crate::path::{Contour, StrokeStyle};
use crate::taffy;
use crate::taffy::prelude::{auto, length};
use crate::text::TextLayout;

/// A word in the lattice.
#[derive(Clone, Debug, PartialEq)]
pub struct LatticeWord {
    /// The pause before the word, in seconds (0 for the first word).
    pub pause: f32,
    /// How loud the word was, 0 to 1.
    pub loud: f32,
    /// Scene time the word arrived on screen.
    pub at: f64,
}

/// Where a word's node sits: its left edge, its baseline and its line, in the text box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub x: f32,
    pub width: f32,
    pub baseline: f32,
    pub line: usize,
}

/// The strip under one text line that strands may use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineGutter {
    /// Top of the strip, in the text box (just under the line's descenders).
    pub top: f32,
    /// Height of the strip. Under 6 px bundles thin to one strand.
    pub height: f32,
    /// The newest line: nodes sit at baseline + `descent` + `drop` and every strand bows down only.
    pub newest: bool,
    /// How far the line's glyphs reach under the baseline.
    pub descent: f32,
}

/// How the lattice looks.
#[derive(Clone, Debug, PartialEq)]
pub struct LatticeLook {
    /// Strands per bundle.
    pub strands: usize,
    /// Logical pixels of bow per second of pause, and the least and most bow.
    pub bow_per_sec: f32,
    pub bow: (f32, f32),
    /// Strand width for a silent word and for the loudest one.
    pub width: (f32, f32),
    pub color: Color,
    /// The newest node and its strands.
    pub accent: Color,
    /// How far under the baseline the nodes sit.
    pub drop: f32,
    pub node_radius: f32,
    /// How much an arrival pulls the strands taut (0 none, 1 straight).
    pub tighten: f32,
    /// Seconds a strand takes to draw on from the previous node.
    pub draw_on: f64,
    /// Labels under the nodes with the pause in ms, or `None` for no labels.
    pub label: Option<TextStyle>,
    pub label_color: Color,
    /// Length of the dashed drop line from a node to its label.
    pub stem: f32,
    /// Per text line (indexed by `Anchor::line`) the gutter its strands stay in. Empty, or a
    /// line past the end, draws as before.
    pub gutters: Vec<LineGutter>,
}

impl Default for LatticeLook {
    fn default() -> LatticeLook {
        LatticeLook {
            strands: 4,
            bow_per_sec: 30.0,
            bow: (2.0, 14.0),
            width: (0.6, 2.2),
            color: Color::rgba(0.82, 0.86, 0.9, 0.55),
            accent: Color::hex(0x2de2c8, 1.0),
            drop: 7.0,
            node_radius: 2.0,
            tighten: 0.6,
            draw_on: 0.18,
            label: None,
            label_color: Color::rgba(0.7, 0.74, 0.8, 0.7),
            stem: 10.0,
            gutters: Vec::new(),
        }
    }
}

/// The nodes of `words` set in `style` at `width` by `layout`, as the caller's text is laid
/// out when it shows the words joined by single spaces.
pub fn anchors(layout: &dyn TextLayout, words: &[&str], style: &TextStyle, width: Option<f32>) -> Vec<Anchor> {
    let text = words.join(" ");
    let shaped = layout.layout(&text, style, width);
    let mut starts = Vec::with_capacity(words.len());
    let mut at = 0usize;
    for w in words {
        starts.push(at..at + w.len());
        at += w.len() + 1;
    }
    let line_of = |gi: usize| shaped.lines.iter().position(|l| l.glyphs.contains(&gi)).unwrap_or(0);
    starts
        .iter()
        .map(|range| {
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            let mut line = None;
            for (gi, g) in shaped.glyphs.iter().enumerate() {
                if range.contains(&(g.cluster as usize)) {
                    lo = lo.min(g.pos.x);
                    hi = hi.max(g.pos.x + g.advance);
                    line.get_or_insert(line_of(gi));
                }
            }
            let line = line.unwrap_or(shaped.lines.len().saturating_sub(1));
            let baseline = shaped.lines.get(line).map_or(style.size, |l| l.baseline);
            if lo.is_finite() {
                Anchor { x: lo, width: hi - lo, baseline, line }
            } else {
                Anchor { x: 0.0, width: 0.0, baseline, line }
            }
        })
        .collect()
}

impl LatticeLook {
    /// The tension multiplier on every bow at `t`: 1 at rest, pulled down by the newest
    /// arrival at `last` and swinging back a little past rest before it settles.
    pub fn tension(&self, last: Option<f64>, t: f64) -> f32 {
        let Some(at) = last else { return 1.0 };
        let u = t - at;
        if u < 0.0 {
            return 1.0;
        }
        (1.0 - self.tighten as f64 * (-u / 0.3).exp() * (u * 7.0).cos()) as f32
    }

    /// The resting bow of a bundle after a pause of `pause` seconds.
    pub fn rest_bow(&self, pause: f32) -> f32 {
        (pause.max(0.0) * self.bow_per_sec).clamp(self.bow.0, self.bow.1)
    }

    fn gutter(&self, a: &Anchor) -> Option<LineGutter> {
        self.gutters.get(a.line).copied()
    }

    /// Where the node of `a` sits vertically.
    pub fn node_y(&self, a: &Anchor) -> f32 {
        match self.gutter(a) {
            None => a.baseline + self.drop,
            Some(g) if g.newest => a.baseline + g.descent + self.drop,
            Some(g) => g.top + g.height * 0.5,
        }
    }

    /// The strands between nodes `a` and `b` (same line): `(curve, edge)` with edge 1 for the
    /// middle strand and less for outer ones. `bow` is the unsigned bow before the gutter
    /// clamp; `skip` draws a single arc.
    pub fn strands(&self, a: &Anchor, b: &Anchor, bow: f32, skip: bool) -> Vec<(Contour, f32)> {
        let (pa, pb) = (Point::new(a.x, self.node_y(a)), Point::new(b.x, self.node_y(b)));
        let g = self.gutter(a);
        let (lo, hi) = match g {
            None => (-bow.abs(), bow.abs()),
            Some(g) if g.newest => (0.0, bow.abs().min((g.top + g.height - pa.y.max(pb.y)).max(0.0))),
            Some(g) => {
                let room = (pa.y.min(pb.y) - g.top).min(g.top + g.height - pa.y.max(pb.y)).max(0.0);
                (-bow.abs().min(room), bow.abs().min(room))
            }
        };
        let s = if skip { 1 } else if g.is_some_and(|g| g.height < 6.0) { 1 } else { self.strands.max(1) };
        let mut out = Vec::with_capacity(s);
        for k in 0..s {
            if skip {
                // Arcs rise (negative) as before; in a gutter the newest line sends them down.
                let off = if g.is_some_and(|g| g.newest) { hi } else if g.is_some() { -hi.abs() } else { -bow.abs() };
                out.push((lens(pa, pb, off, 20), 1.0));
                continue;
            }
            let f = if s == 1 { 0.0 } else { 2.0 * k as f32 / (s - 1) as f32 - 1.0 };
            let (off, edge) = if g.is_some_and(|g| g.newest) {
                let u = if s == 1 { 1.0 } else { k as f32 / (s - 1) as f32 };
                (hi * u, 1.0 - 0.45 * (1.0 - u))
            } else if g.is_some() {
                (if f >= 0.0 { hi * f } else { lo * -f }, 1.0 - 0.45 * f.abs())
            } else {
                (bow * f, 1.0 - 0.45 * f.abs())
            };
            out.push((lens(pa, pb, off, 16), edge));
        }
        out
    }

    /// When the drawing stops changing.
    pub fn until(&self, words: &[LatticeWord]) -> f64 {
        words.iter().map(|w| w.at).fold(f64::NEG_INFINITY, f64::max) + 1.2
    }

    /// Draws the lattice of `words` at `nodes` (same order) at `t`.
    pub fn draw(&self, words: &[LatticeWord], nodes: &[Anchor], t: f64, reduced: bool, p: &mut Painter) {
        let n = words.len().min(nodes.len());
        let shown: Vec<usize> = (0..n).filter(|&i| reduced || words[i].at <= t).collect();
        let Some(&newest) = shown.last() else { return };
        let last = (!reduced).then(|| words[newest].at);
        let tension = self.tension(last, t);
        let node = |i: usize| Point::new(nodes[i].x, self.node_y(&nodes[i]));

        for w in shown.windows(2) {
            let (a, b) = (w[0], w[1]);
            if nodes[a].line != nodes[b].line {
                continue;
            }
            let grow = if reduced { 1.0 } else { ease(((t - words[b].at) / self.draw_on).clamp(0.0, 1.0)) };
            if grow <= 0.0 {
                continue;
            }
            let bow = self.rest_bow(words[b].pause) * tension;
            let width = self.width.0 + (self.width.1 - self.width.0) * words[b].loud.clamp(0.0, 1.0);
            let color = if b == newest { mix(self.color, self.accent, 0.8) } else { self.color };
            for (c, edge) in self.strands(&nodes[a], &nodes[b], bow, false) {
                let c = c.trim(0.0, grow * c.length());
                p.stroke(&c, &StrokeStyle::solid(width * edge, color.fade(edge)));
            }
        }
        // Arcs that skip a word tie the bundles into a lattice.
        for w in shown.windows(3) {
            let (a, c) = (w[0], w[2]);
            if nodes[a].line != nodes[c].line {
                continue;
            }
            let grow = if reduced { 1.0 } else { ease(((t - words[c].at) / (2.0 * self.draw_on)).clamp(0.0, 1.0)) };
            let bow = (self.rest_bow(words[c].pause) + self.rest_bow(words[w[1]].pause)) * tension;
            let arc = self.strands(&nodes[a], &nodes[c], bow, true).remove(0).0;
            let arc = arc.trim(0.0, grow * arc.length());
            p.stroke(&arc, &StrokeStyle::solid(0.6, self.color.fade(0.5)));
        }
        // Nodes and their dashed drop lines.
        for &i in &shown {
            let at = node(i);
            let fresh = if reduced { 1.0 } else { ease(((t - words[i].at) / self.draw_on).clamp(0.0, 1.0)) };
            let c = if i == newest { self.accent } else { mix(self.color, Color::WHITE, 0.4) };
            p.shape(ShapeInstance::circle(at, self.node_radius * (0.6 + 0.4 * fresh), Paint::solid(c)));
            let stem = Contour::polyline(&[at, Point::new(at.x, at.y + self.stem * fresh)], false);
            for dash in stem.dash(&[1.5, 2.0], 0.0) {
                p.stroke(&dash, &StrokeStyle::solid(0.6, self.color.fade(0.8)));
            }
        }
    }

    /// The lattice as an element `size` big over text laid out from its top left: a canvas
    /// for the strands and, with [`LatticeLook::label`], a label under each node.
    pub fn view<M>(
        &self,
        key: impl Into<Key>,
        size: Size,
        words: Arc<[LatticeWord]>,
        nodes: Arc<[Anchor]>,
        reduced: bool,
    ) -> Element<M> {
        let key: Key = key.into();
        let look = Arc::new(self.clone());
        let until = if reduced { 0.0 } else { self.until(&words) };
        let (w2, n2, l2) = (words.clone(), nodes.clone(), look.clone());
        let canvas = Element::canvas(format!("{}-strands", key.0), until, move |t, _: Rect, p: &mut Painter| {
            l2.draw(&w2, &n2, t, reduced, p)
        })
        .size(size.w, size.h)
        .layout(absolute(0.0, 0.0));
        let mut root = Element::layer(key.clone()).size(size.w, size.h).child(canvas);
        if let Some(style) = &self.label {
            for (i, (w, a)) in words.iter().zip(nodes.iter()).enumerate() {
                let ms = (w.pause * 1000.0).round() as i32;
                let text = if i == 0 { "+0".to_string() } else { format!("+{ms}") };
                let y = self.node_y(a) + self.stem + 1.0;
                root = root.child(
                    Element::label(format!("{}-t{i}", key.0), text)
                        .text_style(*style)
                        .color(self.label_color)
                        .layout(absolute(a.x - 2.0, y)),
                );
            }
        }
        root
    }
}

fn absolute(x: f32, y: f32) -> impl FnOnce(&mut taffy::Style) {
    move |s: &mut taffy::Style| {
        s.position = taffy::Position::Absolute;
        s.inset = taffy::Rect { left: length(x), top: length(y), right: auto(), bottom: auto() };
    }
}

/// A curve from `a` to `b` that bows `bow` pixels off the straight line at its middle
/// (positive: down), as a cubic with both handles a third of the way along.
fn lens(a: Point, b: Point, bow: f32, n: usize) -> Contour {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    // Unit normal pointing down for a left-to-right segment.
    let (nx, ny) = (-dy / len, dx / len);
    // A cubic with handles offset by h bows 0.75 h at its middle.
    let h = bow / 0.75;
    let c1 = Point::new(a.x + dx / 3.0 + nx * h, a.y + dy / 3.0 + ny * h);
    let c2 = Point::new(a.x + 2.0 * dx / 3.0 + nx * h, a.y + 2.0 * dy / 3.0 + ny * h);
    let pts: Vec<Point> = (0..=n)
        .map(|i| {
            let u = i as f32 / n as f32;
            let v = 1.0 - u;
            let (k0, k1, k2, k3) = (v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u);
            Point::new(k0 * a.x + k1 * c1.x + k2 * c2.x + k3 * b.x, k0 * a.y + k1 * c1.y + k2 * c2.y + k3 * b.y)
        })
        .collect();
    Contour::polyline(&pts, false)
}

fn ease(k: f64) -> f32 {
    (1.0 - (1.0 - k).powi(3)) as f32
}

fn mix(a: Color, b: Color, k: f32) -> Color {
    Color::rgba(a.r + (b.r - a.r) * k, a.g + (b.g - a.g) * k, a.b + (b.b - a.b) * k, a.a + (b.a - a.a) * k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::EstimateText;

    #[test]
    fn a_longer_pause_bows_further_up_to_the_most() {
        let l = LatticeLook::default();
        assert!(l.rest_bow(0.4) > l.rest_bow(0.2));
        assert_eq!(l.rest_bow(0.0), l.bow.0);
        assert_eq!(l.rest_bow(30.0), l.bow.1);
        let c = lens(Point::new(0.0, 0.0), Point::new(90.0, 0.0), 6.0, 16);
        let mid = c.resample(3)[1];
        assert!((mid.y - 6.0).abs() < 0.2, "{mid:?}");
    }

    #[test]
    fn an_arrival_pulls_taut_then_settles() {
        let l = LatticeLook::default();
        assert_eq!(l.tension(None, 3.0), 1.0);
        assert!((l.tension(Some(1.0), 1.0) - (1.0 - l.tighten)).abs() < 1e-5);
        assert!((l.tension(Some(1.0), 3.0) - 1.0).abs() < 1e-3);
        assert!(l.tension(Some(1.0), 0.5) == 1.0, "nothing before the arrival");
    }

    #[test]
    fn anchors_follow_the_words_and_their_lines() {
        let style = TextStyle::default();
        let layout = EstimateText::default();
        let words = ["the", "future", "feels"];
        let a = anchors(&layout, &words, &style, None);
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].x, 0.0);
        assert!(a[1].x > a[0].x + a[0].width && a[2].x > a[1].x + a[1].width, "{a:?}");
        assert!(a.iter().all(|n| n.line == 0));
    }

    #[test]
    fn drawing_is_pure_in_time_and_hides_words_not_yet_arrived() {
        let l = LatticeLook::default();
        let words: Vec<LatticeWord> = (0..3).map(|i| LatticeWord { pause: 0.3, loud: 0.5, at: i as f64 }).collect();
        let nodes: Vec<Anchor> =
            (0..3).map(|i| Anchor { x: 60.0 * i as f32, width: 40.0, baseline: 20.0, line: 0 }).collect();
        let count = |t: f64| {
            let mut scene: crate::Scene<()> = crate::Scene::new();
            scene.set_viewport(0.0, Size::new(300.0, 80.0), 1.0);
            let view = l.view("lat", Size::new(300.0, 80.0), words.clone().into(), nodes.clone().into(), false);
            scene.apply(Element::column("root").child(view), 0.0);
            let list = scene.sample(t);
            list.shapes.len()
        };
        assert_eq!(count(1.5), count(1.5));
        assert!(count(0.5) < count(2.5), "a node per arrived word");
    }

    #[test]
    fn newest_line_strands_stay_below_the_glyphs_and_in_the_gutter() {
        let mut l = LatticeLook::default();
        let (baseline, descent) = (20.0, 6.0);
        l.gutters = vec![LineGutter { top: baseline + descent, height: 14.0, newest: true, descent }];
        let a = Anchor { x: 0.0, width: 40.0, baseline, line: 0 };
        let b = Anchor { x: 60.0, width: 40.0, baseline, line: 0 };
        for ms in (0..=30).map(|i| i as f32 * 0.1) {
            for tension in [0.4, 1.0, 1.6] {
                let bow = l.rest_bow(ms) * tension;
                for skip in [false, true] {
                    for (c, _) in l.strands(&a, &b, bow * if skip { 2.0 } else { 1.0 }, skip) {
                        for pt in c.resample(24) {
                            assert!(pt.y >= baseline + descent - 1e-3, "{ms} {pt:?}");
                            assert!(pt.y <= 20.0 + 6.0 + 14.0 + 1e-3, "{ms} {pt:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn older_line_bow_is_clamped_and_thin_gutters_use_one_strand() {
        let mut l = LatticeLook::default();
        l.gutters = vec![LineGutter { top: 26.0, height: 8.0, newest: false, descent: 6.0 }];
        let a = Anchor { x: 0.0, width: 40.0, baseline: 20.0, line: 0 };
        let b = Anchor { x: 60.0, width: 40.0, baseline: 20.0, line: 0 };
        for bow in [0.0, 5.0, 40.0] {
            for skip in [false, true] {
                for (c, _) in l.strands(&a, &b, bow, skip) {
                    for pt in c.resample(24) {
                        assert!(pt.y >= 26.0 - 1e-3 && pt.y <= 34.0 + 1e-3, "{pt:?}");
                    }
                }
            }
        }
        assert_eq!(l.strands(&a, &b, 5.0, false).len(), 4);
        l.gutters[0].height = 5.0;
        assert_eq!(l.strands(&a, &b, 5.0, false).len(), 1);
    }
}
