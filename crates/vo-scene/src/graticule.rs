//! A graticule (FX-097): a plot area with minor and major grid lines, ticks on the axes and
//! number labels, built from frames with grid fills and label elements
//! ([`Graticule::view`]).
//!
//! The axes map data ranges onto the plot area: lines and ticks sit on whole multiples of
//! the steps, so a range of -3.2 to 7 with a major step of 2 has lines at -2, 0, 2, 4 and 6.
//! The grid lines are one fill each (`Grid`), not one node per line, so a fine minor grid
//! costs the same as a coarse one.

use crate::element::{Element, Key, TextStyle};
use crate::frame::{Frame, Grid};
use crate::geom::{Color, Point, Size};

/// One axis: the data range across the plot, the major and minor steps (0 for none), and
/// how labels print.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub from: f32,
    pub to: f32,
    pub major: f32,
    pub minor: f32,
    pub decimals: usize,
    pub suffix: String,
}

impl Axis {
    pub fn new(from: f32, to: f32, major: f32, minor: f32) -> Axis {
        Axis { from, to, major, minor, decimals: 0, suffix: String::new() }
    }

    /// Labels with `decimals` places and `suffix` after the number ("dB", " s").
    pub fn format(self, decimals: usize, suffix: impl Into<String>) -> Axis {
        Axis { decimals, suffix: suffix.into(), ..self }
    }

    /// Pixels per data unit across `len` pixels.
    fn scale(&self, len: f32) -> f32 {
        let span = self.to - self.from;
        if span.abs() < 1e-12 {
            0.0
        } else {
            len / span
        }
    }

    /// The whole multiples of `step` in the range, as data values.
    pub fn marks(&self, step: f32) -> Vec<f32> {
        if step <= 0.0 || !step.is_finite() {
            return Vec::new();
        }
        let (lo, hi) = (self.from.min(self.to), self.from.max(self.to));
        let first = (lo / step - 1e-4).ceil() as i64;
        let last = (hi / step + 1e-4).floor() as i64;
        (first..=last).take(10_000).map(|k| k as f32 * step).collect()
    }

    /// Where `v` sits across `len` pixels.
    pub fn place(&self, v: f32, len: f32) -> f32 {
        (v - self.from) * self.scale(len)
    }

    fn label(&self, v: f32) -> String {
        // No "-0".
        let v = if v.abs() < 0.5 * 10f32.powi(-(self.decimals as i32)) { 0.0 } else { v };
        format!("{:.*}{}", self.decimals, v, self.suffix)
    }
}

/// A plot area with grids, ticks and labels. Lines are logical units wide.
#[derive(Clone, Debug, PartialEq)]
pub struct Graticule {
    pub x: Option<Axis>,
    pub y: Option<Axis>,
    pub minor: (f32, Color),
    pub major: (f32, Color),
    /// Minor marks as dots at the crossings instead of lines.
    pub minor_dots: bool,
    /// Tick length outside the plot, 0 for none.
    pub tick: f32,
    /// The plot's outline: width and colour.
    pub border: Option<(f32, Color)>,
    /// Label text and colour; `None` for no labels.
    pub labels: Option<(TextStyle, Color)>,
    /// Room left of the plot for the y labels and below it for the x labels.
    pub gutter: Size,
}

impl Graticule {
    pub fn new(major: Color, minor: Color) -> Graticule {
        Graticule {
            x: None,
            y: None,
            minor: (1.0, minor),
            major: (1.0, major),
            minor_dots: false,
            tick: 4.0,
            border: None,
            labels: None,
            gutter: Size::ZERO,
        }
    }

    pub fn x(self, axis: Axis) -> Graticule {
        Graticule { x: Some(axis), ..self }
    }

    pub fn y(self, axis: Axis) -> Graticule {
        Graticule { y: Some(axis), ..self }
    }

    pub fn widths(self, minor: f32, major: f32) -> Graticule {
        Graticule { minor: (minor, self.minor.1), major: (major, self.major.1), ..self }
    }

    pub fn minor_dots(self) -> Graticule {
        Graticule { minor_dots: true, ..self }
    }

    pub fn tick(self, length: f32) -> Graticule {
        Graticule { tick: length.max(0.0), ..self }
    }

    pub fn border(self, width: f32, color: Color) -> Graticule {
        Graticule { border: Some((width, color)), ..self }
    }

    /// Number labels in `style` and `color`, in a gutter `left` wide for the y axis and
    /// `bottom` high for the x axis.
    pub fn labels(self, style: TextStyle, color: Color, left: f32, bottom: f32) -> Graticule {
        Graticule { labels: Some((style, color)), gutter: Size::new(left, bottom), ..self }
    }

    /// The plot area's box in a view of `size`.
    pub fn plot(&self, size: Size) -> (Point, Size) {
        let at = Point::new(self.gutter.w, 0.0);
        (at, Size::new((size.w - self.gutter.w).max(0.0), (size.h - self.gutter.h).max(0.0)))
    }

    /// Grid fills with a line on the first mark of each axis, `step` picking major or minor.
    fn grid(&self, plot: Size, step: impl Fn(&Axis) -> f32, width: f32, color: Color, dots: bool) -> Option<Grid> {
        let axis = |a: &Option<Axis>, len: f32| {
            a.as_ref().and_then(|a| {
                let st = step(a);
                let first = *a.marks(st).first()?;
                Some(((st * a.scale(len)).abs(), a.place(first, len)))
            })
        };
        let (cx, ox) = axis(&self.x, plot.w).unwrap_or((0.0, 0.0));
        // Data y grows up the screen.
        let (cy, oy) = axis(&self.y, plot.h).map_or((0.0, 0.0), |(c, o)| (c, plot.h - o));
        if cx <= 0.0 && cy <= 0.0 {
            return None;
        }
        let g = if dots {
            Grid::dots(Size::new(cx, cy), width, color)
        } else {
            Grid::lines(Size::new(cx, cy), width, color)
        };
        Some(g.origin(Point::new(ox, oy)))
    }

    /// The graticule as an element tree of `size`: the plot frame, the tick strips and the
    /// labels, children of a `key` box.
    pub fn view<M>(&self, key: impl Into<Key>, size: Size) -> Element<M> {
        let (at, plot) = self.plot(size);
        let mut frame = Frame::new();
        if let Some(g) = self.grid(plot, |a| a.minor, self.minor.0, self.minor.1, self.minor_dots) {
            frame = frame.grid(g);
        }
        if let Some(g) = self.grid(plot, |a| a.major, self.major.0, self.major.1, false) {
            frame = frame.grid(g);
        }
        if let Some((w, c)) = self.border {
            frame = frame.stroke(w, c);
        }
        let mut root = Element::column(key).size(size.w, size.h).child(place(Element::frame("plot", frame), at, plot));
        let (w, c) = self.major;
        if self.tick > 0.0 {
            if let Some(x) = &self.x {
                if let Some(&first) = x.marks(x.major).first() {
                    let ticks = Grid::lines(Size::new((x.major * x.scale(plot.w)).abs(), 0.0), w, c)
                        .origin(Point::new(x.place(first, plot.w), 0.0));
                    let strip = Element::frame("x-ticks", Frame::new().grid(ticks));
                    root = root.child(place(strip, Point::new(at.x, at.y + plot.h), Size::new(plot.w, self.tick)));
                }
            }
            if let Some(y) = &self.y {
                if let Some(&first) = y.marks(y.major).first() {
                    let ticks = Grid::lines(Size::new(0.0, (y.major * y.scale(plot.h)).abs()), w, c)
                        .origin(Point::new(0.0, plot.h - y.place(first, plot.h)));
                    let strip = Element::frame("y-ticks", Frame::new().grid(ticks));
                    root = root.child(place(strip, Point::new(at.x - self.tick, at.y), Size::new(self.tick, plot.h)));
                }
            }
        }
        if let Some((style, color)) = self.labels {
            let lh = style.line_height;
            if let Some(x) = &self.x {
                let box_w = (x.major * x.scale(plot.w)).abs().max(2.0 * style.size);
                for v in x.marks(x.major) {
                    let cx = at.x + x.place(v, plot.w);
                    let label = Element::label(format!("x{v}"), x.label(v)).text_style(style.centered()).color(color);
                    root = root.child(place(
                        label,
                        Point::new(cx - 0.5 * box_w, at.y + plot.h + self.tick + 2.0),
                        Size::new(box_w, lh),
                    ));
                }
            }
            if let Some(y) = &self.y {
                let box_w = (self.gutter.w - self.tick - 4.0).max(0.0);
                for v in y.marks(y.major) {
                    let cy = at.y + plot.h - y.place(v, plot.h);
                    let label = Element::label(format!("y{v}"), y.label(v))
                        .text_style(style.align(crate::text::Align::End))
                        .color(color);
                    root = root.child(place(label, Point::new(0.0, cy - 0.5 * lh), Size::new(box_w, lh)));
                }
            }
        }
        root
    }
}

/// `e` out of flow at `at` with `size`, in its parent's box.
fn place<M>(e: Element<M>, at: Point, size: Size) -> Element<M> {
    use taffy::style::{LengthPercentageAuto, Position};
    e.size(size.w, size.h).layout(|s| {
        s.position = Position::Absolute;
        s.inset.left = LengthPercentageAuto::length(at.x);
        s.inset.top = LengthPercentageAuto::length(at.y);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::VectorPaint;
    use crate::scene::Scene;

    #[test]
    fn marks_sit_on_whole_steps() {
        let a = Axis::new(-3.2, 7.0, 2.0, 0.5);
        assert_eq!(a.marks(2.0), vec![-2.0, 0.0, 2.0, 4.0, 6.0]);
        assert_eq!(a.marks(0.5).len(), 21);
        assert_eq!(a.place(-3.2, 102.0), 0.0);
        assert!((a.place(7.0, 102.0) - 102.0).abs() < 1e-3);
        assert_eq!(Axis::new(0.0, 1.0, 0.25, 0.0).format(2, " s").label(0.5), "0.50 s");
        assert_eq!(Axis::new(-1.0, 1.0, 0.5, 0.0).format(1, "").label(-0.01), "0.0");
    }

    #[test]
    fn grids_line_up_with_the_marks() {
        let g = Graticule::new(Color::WHITE, Color::WHITE)
            .x(Axis::new(-3.0, 7.0, 2.0, 0.5))
            .y(Axis::new(0.0, 10.0, 5.0, 1.0));
        let plot = Size::new(100.0, 50.0);
        let major = g.grid(plot, |a| a.major, 1.0, Color::WHITE, false).unwrap();
        // x: every 20 px from -2 at 10 px; y: every 25 px from 0 at the bottom.
        assert_eq!(major.cell, Size::new(20.0, 25.0));
        assert_eq!(major.origin, Point::new(10.0, 50.0));
    }

    #[test]
    fn the_view_draws_grids_ticks_and_labels() {
        let g = Graticule::new(Color::WHITE, Color::WHITE)
            .x(Axis::new(0.0, 4.0, 1.0, 0.25))
            .y(Axis::new(-1.0, 1.0, 1.0, 0.5))
            .border(1.0, Color::WHITE)
            .labels(TextStyle::new(0, 10.0, 12.0), Color::WHITE, 30.0, 18.0);
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(230.0, 118.0), 1.0);
        scene.apply(g.view("g", Size::new(230.0, 118.0)), 0.0);
        let list = scene.sample(0.0);
        let grids = list.vectors.iter().filter(|v| matches!(v.paint, VectorPaint::Grid { .. })).count();
        // Minor and major grids, x and y ticks.
        assert_eq!(grids, 4);
        // Five x labels and three y labels.
        assert!(scene.find(&["g", "x4"]).is_some());
        assert!(scene.find(&["g", "y-1"]).is_some());
        assert!(scene.find(&["g", "x5"]).is_none());
    }
}
