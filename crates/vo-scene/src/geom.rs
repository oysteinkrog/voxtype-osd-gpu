//! Geometry in logical units.
//!
//! One logical unit is one pixel at scale 1. The display list carries the scale factor, and
//! only the renderer converts to physical pixels. Everything is `f32`; time stays `f64` in
//! `vo-core` and never reaches these types.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[repr(C)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[repr(C)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const ZERO: Size = Size { w: 0.0, h: 0.0 };

    pub const fn new(w: f32, h: f32) -> Size {
        Size { w, h }
    }
}

/// An axis-aligned rectangle: top-left corner and size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[repr(C)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn from_size(size: Size) -> Rect {
        Rect { x: 0.0, y: 0.0, w: size.w, h: size.h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.right() && p.y < self.bottom()
    }
}

/// A rectangle with per-corner radii: top-left, top-right, bottom-right, bottom-left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[repr(C)]
pub struct RRect {
    pub rect: Rect,
    pub radii: [f32; 4],
}

impl RRect {
    pub fn new(rect: Rect, radius: f32) -> RRect {
        RRect { rect, radii: [radius; 4] }
    }

    /// Whether `p` is inside, corners included. Radii larger than half a side are clamped.
    pub fn contains(&self, p: Point) -> bool {
        let r = self.rect;
        if !r.contains(p) {
            return false;
        }
        let max = 0.5 * r.w.min(r.h);
        let corners = [
            (self.radii[0], r.x, r.y, 1.0, 1.0),
            (self.radii[1], r.right(), r.y, -1.0, 1.0),
            (self.radii[2], r.right(), r.bottom(), -1.0, -1.0),
            (self.radii[3], r.x, r.bottom(), 1.0, -1.0),
        ];
        for (radius, cx, cy, sx, sy) in corners {
            let radius = radius.clamp(0.0, max);
            if radius <= 0.0 {
                continue;
            }
            // Centre of this corner's circle.
            let (ox, oy) = (cx + sx * radius, cy + sy * radius);
            let inside_x = if sx > 0.0 { p.x < ox } else { p.x > ox };
            let inside_y = if sy > 0.0 { p.y < oy } else { p.y > oy };
            if inside_x && inside_y {
                let (dx, dy) = (p.x - ox, p.y - oy);
                if dx * dx + dy * dy > radius * radius {
                    return false;
                }
            }
        }
        true
    }
}

/// A colour with straight (not premultiplied) alpha, components in `[0, 1]`, sRGB encoded
/// as in CSS. The renderer linearises and premultiplies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[repr(C)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Color = Color::rgba(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Color = Color::rgba(1.0, 1.0, 1.0, 1.0);

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
        Color { r, g, b, a }
    }

    /// From `0xRRGGBB` and an alpha.
    pub fn hex(rgb: u32, a: f32) -> Color {
        let c = |shift: u32| ((rgb >> shift) & 0xff) as f32 / 255.0;
        Color { r: c(16), g: c(8), b: c(0), a }
    }

    /// The same colour with alpha multiplied by `k`.
    pub fn fade(self, k: f32) -> Color {
        Color { a: self.a * k, ..self }
    }
}

/// A 2D affine transform: `x' = a x + c y + e`, `y' = b x + d y + f`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[repr(C)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for Affine {
    fn default() -> Affine {
        Affine::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub const fn translate(x: f32, y: f32) -> Affine {
        Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: x, f: y }
    }

    pub const fn scale(s: f32) -> Affine {
        Affine { a: s, b: 0.0, c: 0.0, d: s, e: 0.0, f: 0.0 }
    }

    /// Rotation by `radians`, clockwise on screen (y points down).
    pub fn rotate(radians: f32) -> Affine {
        let (s, c) = radians.sin_cos();
        Affine { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }

    /// `self` followed by `next`: maps a point through `self` first.
    pub fn then(&self, next: &Affine) -> Affine {
        Affine {
            a: next.a * self.a + next.c * self.b,
            b: next.b * self.a + next.d * self.b,
            c: next.a * self.c + next.c * self.d,
            d: next.b * self.c + next.d * self.d,
            e: next.a * self.e + next.c * self.f + next.e,
            f: next.b * self.e + next.d * self.f + next.f,
        }
    }

    pub fn apply(&self, p: Point) -> Point {
        Point { x: self.a * p.x + self.c * p.y + self.e, y: self.b * p.x + self.d * p.y + self.f }
    }

    /// The inverse, or `None` when the transform collapses the plane (scale 0).
    pub fn invert(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        let k = 1.0 / det;
        Some(Affine {
            a: self.d * k,
            b: -self.b * k,
            c: -self.c * k,
            d: self.a * k,
            e: (self.c * self.f - self.d * self.e) * k,
            f: (self.b * self.e - self.a * self.f) * k,
        })
    }

    /// True when the transform keeps rectangles axis-aligned (no rotation or skew).
    pub fn is_axis_aligned(&self) -> bool {
        self.b == 0.0 && self.c == 0.0
    }

    pub fn is_translation(&self) -> bool {
        self.is_axis_aligned() && self.a == 1.0 && self.d == 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_inverse_round_trips() {
        let m = Affine::rotate(0.3).then(&Affine::scale(1.5)).then(&Affine::translate(10.0, -4.0));
        let inv = m.invert().unwrap();
        let p = Point::new(3.0, 7.0);
        let q = inv.apply(m.apply(p));
        assert!((q.x - p.x).abs() < 1e-5 && (q.y - p.y).abs() < 1e-5, "{q:?}");
        assert_eq!(Affine::scale(0.0).invert(), None);
    }

    #[test]
    fn then_applies_left_first() {
        let m = Affine::scale(2.0).then(&Affine::translate(5.0, 0.0));
        assert_eq!(m.apply(Point::new(1.0, 1.0)), Point::new(7.0, 2.0));
    }

    #[test]
    fn rrect_corners_are_round() {
        let r = RRect::new(Rect::new(0.0, 0.0, 100.0, 40.0), 20.0);
        assert!(r.contains(Point::new(50.0, 20.0)));
        assert!(r.contains(Point::new(20.0, 1.0)));
        assert!(!r.contains(Point::new(1.0, 1.0)));
        assert!(!r.contains(Point::new(99.0, 39.0)));
        assert!(!r.contains(Point::new(101.0, 20.0)));
    }
}
