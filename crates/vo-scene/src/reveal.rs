//! A frame that draws itself on when a popup shows: the border grows out of its four corners
//! one after another, hairlines open from their centres, corner brackets slide out, and the
//! content enters once the frame has closed.
//!
//! [`FrameReveal`] is a pure function of `t` drawn through a [`Painter`], so it goes in a view
//! as an `Element::canvas` behind the content. [`FrameReveal::content_at`] says when the
//! content should start to enter, and [`FrameReveal::content_alpha`] gives a fade for content
//! drawn by the same canvas.

use crate::canvas::Painter;
use crate::geom::{Affine, Rect};
use crate::path::{make, Contour, StrokeStyle};

/// See the module docs. Times are in seconds; lengths in logical units.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameReveal {
    /// When the reveal starts.
    pub start: f64,
    /// Corner radius of the frame.
    pub radius: f32,
    /// The border stroke.
    pub border: StrokeStyle,
    /// Delay between one corner and the next (clockwise from top left).
    pub stagger: f32,
    /// How long each corner takes to reach the middles of its two edges.
    pub draw: f32,
    /// How long a hairline takes to open.
    pub open: f32,
    /// Horizontal hairlines across the frame, as fractions of its height, opening from the
    /// centre once the second corner has started.
    pub hairlines: Vec<f32>,
    pub hairline: StrokeStyle,
    /// Corner brackets outside the frame: arm length, gap to the frame, and the stroke. They
    /// slide from the centre out to the corners, `bracket_slide` seconds each, `stagger` apart.
    pub brackets: Option<(f32, f32, StrokeStyle)>,
    pub bracket_slide: f32,
    /// How long the content takes to fade in after the frame closes.
    pub content_fade: f32,
}

impl Default for FrameReveal {
    fn default() -> FrameReveal {
        FrameReveal {
            start: 0.0,
            radius: 12.0,
            border: StrokeStyle::hairline(crate::geom::Color::hex(0xffffff, 0.5)),
            stagger: 0.040,
            draw: 0.300,
            open: 0.240,
            hairlines: Vec::new(),
            hairline: StrokeStyle::hairline(crate::geom::Color::hex(0xffffff, 0.25)),
            brackets: None,
            bracket_slide: 0.200,
            content_fade: 0.160,
        }
    }
}

/// Ease out, cubic.
fn ease(u: f64) -> f32 {
    let u = u.clamp(0.0, 1.0);
    (1.0 - (1.0 - u).powi(3)) as f32
}

impl FrameReveal {
    /// When the border is whole.
    pub fn closed_at(&self) -> f64 {
        self.start + 3.0 * self.stagger as f64 + self.draw as f64
    }

    /// When the content should start to enter: once the frame has closed.
    pub fn content_at(&self) -> f64 {
        self.closed_at()
    }

    /// The content's opacity at `t`, for content drawn by the same canvas.
    pub fn content_alpha(&self, t: f64) -> f32 {
        ease((t - self.content_at()) / self.content_fade.max(1e-3) as f64)
    }

    /// When everything has stopped moving.
    pub fn until(&self) -> f64 {
        let brackets = self.start + 3.0 * self.stagger as f64 + self.bracket_slide as f64;
        (self.content_at() + self.content_fade as f64).max(brackets)
    }

    /// How far corner `k` (0 top left, then clockwise) has drawn at `t`, 0 to 1.
    pub fn corner(&self, k: usize, t: f64) -> f32 {
        ease((t - self.start - k as f64 * self.stagger as f64) / self.draw.max(1e-3) as f64)
    }

    /// The visible pieces of the border of `rect` at `t`.
    pub fn border_pieces(&self, rect: Rect, t: f64) -> Vec<Contour> {
        let r = self.radius.min(0.5 * rect.w.min(rect.h)).max(0.0);
        let ring = make::panel(rect.x, rect.y, rect.w, rect.h, r);
        let len = ring.length();
        // The panel starts at the middle of the top edge and runs clockwise: half the top
        // edge, the top right arc, the right edge, and so on. Edge middles in arc length:
        let (hw, hh) = (0.5 * rect.w - r, 0.5 * rect.h - r);
        let quarter = std::f32::consts::FRAC_PI_2 * r;
        let right = hw + quarter + hh;
        let bottom = right + hh + quarter + hw;
        let left = bottom + hw + quarter + hh;
        // Each corner draws from the middle of its arc out to the middles of its two edges.
        // Top left first, then clockwise; the top left one ends at the seam (len).
        let corners = [
            (left + hh + 0.5 * quarter, left, len),
            (hw + 0.5 * quarter, 0.0, right),
            (right + hh + 0.5 * quarter, right, bottom),
            (bottom + hw + 0.5 * quarter, bottom, left),
        ];
        // Once every corner is done the border is one closed contour, so a translucent stroke
        // does not darken where the pieces meet.
        if (0..4).all(|k| self.corner(k, t) >= 1.0) {
            return vec![ring];
        }
        let mut out = Vec::new();
        for (k, (mid, from, to)) in corners.into_iter().enumerate() {
            let a = self.corner(k, t);
            if a <= 0.0 {
                continue;
            }
            let (s0, s1) = (mid - a * (mid - from), mid + a * (to - mid));
            out.push(ring.trim(s0, s1));
        }
        out
    }

    /// Draws the frame of `rect` at `t`.
    pub fn paint(&self, t: f64, rect: Rect, p: &mut Painter) {
        for c in self.border_pieces(rect, t) {
            p.stroke(&c, &self.border);
        }
        // Hairlines open from the centre once the second corner has started.
        let open = ease((t - self.start - self.stagger as f64) / self.open.max(1e-3) as f64);
        if open > 0.0 {
            let cx = rect.x + 0.5 * rect.w;
            let half = 0.5 * rect.w * open;
            for &f in &self.hairlines {
                let y = rect.y + rect.h * f;
                p.stroke(&make::underline(cx - half, cx + half, y), &self.hairline);
            }
        }
        if let Some((arm, gap, style)) = self.brackets {
            // Each bracket starts at the centre and slides out to its corner, top left first.
            let outer = Rect::new(rect.x - gap, rect.y - gap, rect.w + 2.0 * gap, rect.h + 2.0 * gap);
            let centre = (rect.x + 0.5 * rect.w, rect.y + 0.5 * rect.h);
            for (k, c) in make::brackets(outer.x, outer.y, outer.w, outer.h, arm).into_iter().enumerate() {
                let u = (t - self.start - k as f64 * self.stagger as f64) / self.bracket_slide.max(1e-3) as f64;
                let d = ease(u);
                if d <= 0.0 {
                    continue;
                }
                // The bracket's corner point is the middle point of its polyline.
                let corner = c.segs[0].p2;
                let shift = Affine::translate((centre.0 - corner.x) * (1.0 - d), (centre.1 - corner.y) * (1.0 - d));
                let a = (2.0 * d).min(1.0);
                let s = StrokeStyle { c0: style.c0.fade(a), c1: style.c1.fade(a), ..style };
                p.stroke(&c.transform(&shift), &s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(pieces: &[Contour]) -> f32 {
        pieces.iter().map(|c| c.length() - c.segs.first().map_or(0.0, |s| s.s0)).sum()
    }

    #[test]
    fn corners_draw_one_after_another_and_close_the_frame() {
        let f = FrameReveal { start: 1.0, ..FrameReveal::default() };
        let rect = Rect::new(10.0, 10.0, 200.0, 80.0);
        assert!(f.border_pieces(rect, 0.99).is_empty());
        // 20 ms in: only the first corner has started.
        assert_eq!(f.border_pieces(rect, 1.02).len(), 1);
        assert_eq!(f.border_pieces(rect, 1.13).len(), 4);
        let whole = make::panel(rect.x, rect.y, rect.w, rect.h, f.radius).length();
        let closed = total(&f.border_pieces(rect, f.closed_at()));
        assert!((closed - whole).abs() < 0.5, "{closed} of {whole}");
        let mid = total(&f.border_pieces(rect, 1.1));
        assert!(mid > 0.2 * whole && mid < 0.9 * whole, "{mid}");
        assert_eq!(f.content_alpha(f.content_at()), 0.0);
        assert_eq!(f.content_alpha(f.content_at() + 1.0), 1.0);
    }
}
