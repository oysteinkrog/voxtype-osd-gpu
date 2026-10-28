//! Displacement (EG-03): a layer redraws what is under it with each pixel moved by a field.
//!
//! `Element::layer(..).displace(Displace::Ripples(..))` captures the layer's backdrop (EG-01)
//! and draws it back with each pixel taken from somewhere else: rings that spread from word
//! onsets, a noise or curl field, a page that lifts over a cylinder, or offsets read from a
//! texture (a water height field, a flow map). The layer's own content draws on top, as with
//! any backdrop.
//!
//! Every field is a pure function of time and the recorded events it is given: a ring's
//! radius and fade are closed forms in its age, and noise turns with time, so a seek or a
//! replay draws the same frame.
//!
//! The strength (`Element::displace_strength`) is an animated prop that can be bound to a
//! signal (`Param::DisplaceStrength`). At 0, and with reduced motion, nothing is emitted: the
//! layer draws as if it had no displacement and the frame pays nothing for it.
//!
//! The newest line of live text is kept sharp and still by default (the reading-line
//! envelope): the renderer draws its box as captured. `Element::displace_reading_line(true)`
//! turns that off for text that is not read.

use crate::display::{DisplaceField, Ring, DISPLACE_RINGS};
use crate::geom::Point;

/// A displacement source, as the app describes it. Lengths are in layer units, places in the
/// layer's space.
#[derive(Clone, Debug, PartialEq)]
pub enum Displace {
    /// Rings that grow from recorded onsets.
    Ripples(Ripples),
    /// Simplex noise: one cell `scale` wide, offsets up to `amount`, the pattern turning
    /// `speed` times a second.
    Noise { scale: f32, amount: f32, speed: f32, seed: u32 },
    /// A swirling flow (the curl of simplex noise), laid out like `Noise`.
    Curl { scale: f32, amount: f32, speed: f32, seed: u32 },
    /// A page that lifts over a cylinder.
    PageCurl(PageCurl),
    /// Offsets from image slot `slot` (`Renderer::set_material_image`): uv 0 to 1 over the
    /// layer's box, offset `(rg - bias) * amount`. A texture holding (1, 0) everywhere with
    /// `amount` (3, 0) moves the whole layer 3 units left (each pixel shows what is 3 units to
    /// its right).
    Texture { slot: u32, amount: [f32; 2], bias: [f32; 2] },
}

/// One recorded event a ring starts from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Onset {
    /// Scene time the ring starts, in seconds.
    pub t: f64,
    /// Where it starts, in layer space.
    pub at: Point,
}

/// Expanding rings, one per onset.
#[derive(Clone, Debug, PartialEq)]
pub struct Ripples {
    pub onsets: Vec<Onset>,
    /// How fast a ring grows, in units per second.
    pub speed: f32,
    /// Seconds a ring lives; it fades out over its life.
    pub life: f64,
    /// How far a ring pushes at its crest when it starts.
    pub amp: f32,
    /// Length of one wave of the ring's packet.
    pub wavelength: f32,
    /// How wide the packet is (about its standard deviation).
    pub width: f32,
}

/// Seconds a ring takes to reach full amplitude, so it does not pop in.
const RING_ATTACK: f64 = 0.06;

impl Ripples {
    /// Rings at 120 units a second, living 1.2 s, pushing up to 4 units.
    pub fn new() -> Ripples {
        Ripples { onsets: Vec::new(), speed: 120.0, life: 1.2, amp: 4.0, wavelength: 18.0, width: 10.0 }
    }

    /// A ring that starts at `t` from `at`.
    pub fn onset(mut self, t: f64, at: Point) -> Ripples {
        self.onsets.push(Onset { t, at });
        self
    }

    pub fn onsets(mut self, onsets: impl IntoIterator<Item = Onset>) -> Ripples {
        self.onsets.extend(onsets);
        self
    }

    pub fn speed(self, speed: f32) -> Ripples {
        Ripples { speed, ..self }
    }

    pub fn life(self, life: f64) -> Ripples {
        Ripples { life: life.max(1e-3), ..self }
    }

    pub fn amp(self, amp: f32) -> Ripples {
        Ripples { amp, ..self }
    }

    /// The packet's wavelength and width.
    pub fn wave(self, wavelength: f32, width: f32) -> Ripples {
        Ripples { wavelength: wavelength.max(1e-3), width: width.max(1e-3), ..self }
    }

    /// The rings alive at `t`, youngest first, at most `DISPLACE_RINGS`.
    pub fn rings(&self, t: f64) -> Vec<Ring> {
        let mut live: Vec<(f64, Ring)> = self
            .onsets
            .iter()
            .filter_map(|o| {
                let age = t - o.t;
                if !(0.0..self.life).contains(&age) {
                    return None;
                }
                let fade = (1.0 - age / self.life).powi(2) * (age / RING_ATTACK).min(1.0);
                let ring = Ring { centre: o.at, radius: (age * self.speed as f64) as f32, amp: self.amp * fade as f32 };
                Some((age, ring))
            })
            .collect();
        live.sort_by(|a, b| a.0.total_cmp(&b.0));
        live.into_iter().take(DISPLACE_RINGS).map(|(_, r)| r).collect()
    }
}

impl Default for Ripples {
    fn default() -> Ripples {
        Ripples::new()
    }
}

/// A page lifting over a cylinder. The fold line starts at `edge` and moves `reach` units
/// into the page against `dir` as the strength goes from 0 to 1; past it the page rolls up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageCurl {
    /// A point on the page's edge that lifts first, in layer space.
    pub edge: Point,
    /// Out of the page through that edge. Normalized when used.
    pub dir: Point,
    pub radius: f32,
    pub reach: f32,
}

impl Displace {
    /// The field at `t` for a node that entered at `since`, with strength `strength`.
    pub(crate) fn field(&self, t: f64, since: f64, strength: f32) -> DisplaceField {
        let turn = |speed: f32| (std::f64::consts::TAU * speed as f64 * (t - since).max(0.0)) as f32;
        match self {
            Displace::Ripples(r) => {
                let live = r.rings(t);
                let mut rings = [Ring::default(); DISPLACE_RINGS];
                rings[..live.len()].copy_from_slice(&live);
                DisplaceField::Ripples {
                    rings,
                    count: live.len() as u32,
                    wavelength: r.wavelength,
                    width: r.width,
                }
            }
            Displace::Noise { scale, amount, speed, seed } => {
                DisplaceField::Noise { scale: scale.max(1e-3), amount: *amount, seed: *seed, turn: turn(*speed) }
            }
            Displace::Curl { scale, amount, speed, seed } => {
                DisplaceField::Curl { scale: scale.max(1e-3), amount: *amount, seed: *seed, turn: turn(*speed) }
            }
            Displace::PageCurl(p) => {
                let len = (p.dir.x * p.dir.x + p.dir.y * p.dir.y).sqrt().max(1e-6);
                let dir = Point::new(p.dir.x / len, p.dir.y / len);
                let back = p.reach * strength;
                DisplaceField::PageCurl {
                    line: Point::new(p.edge.x - dir.x * back, p.edge.y - dir.y * back),
                    dir,
                    radius: p.radius.max(1e-3),
                }
            }
            Displace::Texture { slot, amount, bias } => {
                DisplaceField::Texture { slot: *slot, amount: *amount, bias: *bias }
            }
        }
    }

    /// When the field next changes after `t`: `t` itself while it moves, the next onset
    /// while it waits for one, `None` when it is still.
    pub(crate) fn wake(&self, t: f64) -> Option<f64> {
        match self {
            Displace::Ripples(r) => {
                if r.onsets.iter().any(|o| (o.t..o.t + r.life).contains(&t)) {
                    Some(t)
                } else {
                    r.onsets.iter().map(|o| o.t).filter(|&s| s > t).min_by(f64::total_cmp)
                }
            }
            Displace::Noise { speed, .. } | Displace::Curl { speed, .. } => (*speed != 0.0).then_some(t),
            Displace::PageCurl(_) | Displace::Texture { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rings_are_a_closed_form_in_time() {
        let r = Ripples::new().onset(0.2, Point::new(10.0, 10.0)).onset(0.5, Point::new(50.0, 10.0));
        let a = r.rings(0.8);
        assert_eq!(a.len(), 2);
        // Youngest first: the 0.5 s ring is 0.3 s old.
        assert!((a[0].radius - 36.0).abs() < 1e-3, "{:?}", a[0]);
        assert!((a[1].radius - 72.0).abs() < 1e-3, "{:?}", a[1]);
        assert!(a[0].amp > a[1].amp);
        // The same at the same time, whatever came before.
        assert_eq!(r.rings(0.8), a);
        assert!(r.rings(0.1).is_empty());
        assert!(r.rings(2.0).is_empty());
    }

    #[test]
    fn ripples_wake_while_a_ring_lives_and_for_the_next_onset() {
        let d = Displace::Ripples(Ripples::new().onset(1.0, Point::ZERO));
        assert_eq!(d.wake(0.5), Some(1.0));
        assert_eq!(d.wake(1.5), Some(1.5));
        assert_eq!(d.wake(3.0), None);
    }

    #[test]
    fn page_curl_line_moves_with_strength() {
        let d = Displace::PageCurl(PageCurl {
            edge: Point::new(100.0, 0.0),
            dir: Point::new(2.0, 0.0),
            radius: 10.0,
            reach: 40.0,
        });
        let DisplaceField::PageCurl { line, dir, .. } = d.field(0.0, 0.0, 0.5) else { panic!() };
        assert_eq!(line, Point::new(80.0, 0.0));
        assert_eq!(dir, Point::new(1.0, 0.0));
    }
}
