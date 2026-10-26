//! Dot grids (`Element::dots`): many small round dots on a regular grid, each with its own
//! position, size, brightness and colour. The dot-matrix design family builds its waveform,
//! status font and particles from them.
//!
//! The element owns only the grid: a pitch and a box. What the dots do comes from a
//! [`DotSource`], which answers "which dots, where, at `t`" in grid cells. Sources are pure
//! functions of `t`, like `Wave`, so a live session and an export draw the same frames.
//! `vo-dots` has the sources: a spring-driven field, a dot font, waveforms and particles.
//!
//! The pitch is rounded to whole physical pixels when the list is sampled, so a settled dot
//! lands on the same pixel phase as every other dot. Dots that still move are not snapped;
//! each dot says how much it wants snapping (`Dot::snap`), and the renderer moves its centre
//! that far toward the nearest pixel centre.

use std::fmt::Debug;
use std::sync::Arc;

use crate::geom::Color;

/// One dot from a source, in grid cells: `(0, 0)` is the centre of the top-left cell, and
/// `x` grows to the right, `y` down. Cells may be fractional while a dot moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dot {
    pub x: f32,
    pub y: f32,
    /// Diameter as a fraction of the pitch. 1 makes neighbouring dots touch.
    pub size: f32,
    /// Straight alpha sRGB. Brightness is folded into alpha by the source.
    pub color: Color,
    /// 0 for a dot in flight, 1 for one at rest that should sit on whole pixels.
    pub snap: f32,
}

/// What a dot element draws.
pub trait DotSource: Debug + Send + Sync {
    /// Appends the dots at `t`.
    fn dots(&self, t: f64, out: &mut Vec<Dot>);

    /// Whether the dots may differ from `dots(t)` at any later time.
    fn moving(&self, t: f64) -> bool;
}

/// A dot element's content: the source and the grid pitch.
#[derive(Clone, Debug)]
pub struct Dots {
    pub source: Arc<dyn DotSource>,
    /// Distance between cell centres in logical units. Rounded to whole physical pixels,
    /// never below one.
    pub pitch: f32,
    /// Offset of cell `(0, 0)`'s centre from the box's top-left corner, in cells. The default
    /// `(0.5, 0.5)` puts the first cell's whole area inside the box.
    pub origin: (f32, f32),
}

impl Dots {
    pub fn new(source: Arc<dyn DotSource>, pitch: f32) -> Dots {
        Dots { source, pitch, origin: (0.5, 0.5) }
    }

    /// The pitch in logical units at `scale` physical pixels per unit: a whole number of
    /// pixels, at least one.
    pub fn snapped_pitch(&self, scale: f32) -> f32 {
        let scale = scale.max(1e-6);
        (self.pitch * scale).round().max(1.0) / scale
    }
}

impl PartialEq for Dots {
    /// Same source object, same grid. Two sources are never compared by value: a new
    /// source is new content.
    fn eq(&self, other: &Dots) -> bool {
        Arc::ptr_eq(&self.source, &other.source) && self.pitch == other.pitch && self.origin == other.origin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Still;

    impl DotSource for Still {
        fn dots(&self, _: f64, out: &mut Vec<Dot>) {
            out.push(Dot { x: 0.0, y: 0.0, size: 0.5, color: Color::WHITE, snap: 1.0 });
        }

        fn moving(&self, _: f64) -> bool {
            false
        }
    }

    #[test]
    fn pitch_is_whole_pixels() {
        let d = Dots::new(Arc::new(Still), 3.3);
        assert_eq!(d.snapped_pitch(1.0), 3.0);
        assert!((d.snapped_pitch(1.5) * 1.5 - 5.0).abs() < 1e-6);
        assert_eq!(Dots::new(Arc::new(Still), 0.1).snapped_pitch(1.0), 1.0);
    }

    #[test]
    fn equal_only_for_the_same_source() {
        let s: Arc<dyn DotSource> = Arc::new(Still);
        assert_eq!(Dots::new(s.clone(), 4.0), Dots::new(s, 4.0));
        assert_ne!(Dots::new(Arc::new(Still), 4.0), Dots::new(Arc::new(Still), 4.0));
    }
}
