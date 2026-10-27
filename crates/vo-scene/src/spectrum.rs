//! A radial spectrum ([`Element::spectrum_ring`]): bars round a ring, one per slot, whose
//! lengths follow band energies.
//!
//! The bands come from the signal bus (the mel bands, read and smoothed at sample time, so
//! the view is built once and the scene stays pure in `t`) or from fixed values the app
//! passes. `Bars` with `BarLayout::Radial` and `bands` draws a ring from values in the view;
//! this element reads the bus itself and adds a partial sweep, mirrored halves and colour by
//! energy.

use std::sync::Arc;

use vo_core::signals::{Sig, Signals, SignalsCell, MEL_BANDS};

use crate::display::{DisplayList, Item, ItemKind, Paint, ShapeInstance};
use crate::element::{Element, Key, Kind};
use crate::geom::{Color, Point, Size};

/// Where a spectrum's band energies come from.
#[derive(Clone, Debug, PartialEq)]
pub enum Bands {
    /// Mel bands `lo..=hi` of the signal bus, smoothed with `attack` and `release` seconds.
    Signals { lo: u8, hi: u8, attack: f32, release: f32 },
    /// Fixed energies from 0 to 1, lowest band first.
    Fixed(Arc<[f32]>),
}

/// What [`Element::spectrum_ring`] draws (`Kind::Spectrum`).
#[derive(Clone, Debug, PartialEq)]
pub struct Spectrum {
    pub bands: Bands,
    /// Bars round the sweep.
    pub count: u32,
    /// The ring the bars stand on, as a fraction of half the box's shorter side.
    pub inner: f32,
    /// A bar at full energy reaches this fraction of the room between the ring and the edge.
    pub reach: f32,
    /// Bar length at zero energy, logical units, so a quiet ring still shows.
    pub rest: f32,
    /// Bar width as a fraction of the space per bar on the ring.
    pub width: f32,
    /// First bar's angle (radians, clockwise, 0 right) and the angle the bars cover. A full
    /// turn spaces them evenly round the ring.
    pub start: f32,
    pub sweep: f32,
    /// Low bands at both ends of the sweep and high bands in the middle, so a full ring is
    /// symmetric about its start.
    pub mirror: bool,
    /// Bars grow out from the ring, or both ways.
    pub both_ways: bool,
    /// Energy from 0 to 1 is raised to this power: above 1 calms quiet bands.
    pub curve: f32,
    /// Colour at zero and at full energy, mixed by energy.
    pub low: Color,
    pub high: Color,
    /// Glow radius and strength.
    pub glow: Option<(f32, f32)>,
    /// Up to this fraction (0 to 1) taken off each bar's energy length, a fixed amount per
    /// bar from `seed`, so neighbouring bars differ. 0 for none.
    pub jitter: f32,
    pub seed: u32,
    /// How many times a second the jitter moves to new values, eased between them; 0 keeps it
    /// fixed. Above 0 the ring keeps redrawing.
    pub drift: f32,
}

impl Spectrum {
    /// 48 bars round a full ring from the top, fed by all the mel bands.
    pub fn new(low: Color, high: Color) -> Spectrum {
        Spectrum {
            bands: Bands::Signals { lo: 0, hi: MEL_BANDS as u8 - 1, attack: 0.015, release: 0.18 },
            count: 48,
            inner: 0.55,
            reach: 1.0,
            rest: 1.5,
            width: 0.55,
            start: -std::f32::consts::FRAC_PI_2,
            sweep: std::f32::consts::TAU,
            mirror: false,
            both_ways: false,
            curve: 1.0,
            low,
            high,
            glow: None,
            jitter: 0.0,
            seed: 0,
            drift: 0.0,
        }
    }

    pub fn bands(self, bands: Bands) -> Spectrum {
        Spectrum { bands, ..self }
    }

    pub fn count(self, count: u32) -> Spectrum {
        Spectrum { count, ..self }
    }

    /// The ring at `inner` of half the shorter side; full bars reach `reach` of the room.
    pub fn ring(self, inner: f32, reach: f32) -> Spectrum {
        Spectrum { inner: inner.clamp(0.0, 0.98), reach: reach.max(0.0), ..self }
    }

    pub fn width(self, width: f32) -> Spectrum {
        Spectrum { width: width.clamp(0.01, 1.0), ..self }
    }

    pub fn sweep(self, start: f32, sweep: f32) -> Spectrum {
        Spectrum { start, sweep, ..self }
    }

    pub fn mirror(self) -> Spectrum {
        Spectrum { mirror: true, ..self }
    }

    pub fn both_ways(self) -> Spectrum {
        Spectrum { both_ways: true, ..self }
    }

    pub fn rest(self, rest: f32) -> Spectrum {
        Spectrum { rest: rest.max(0.0), ..self }
    }

    pub fn curve(self, curve: f32) -> Spectrum {
        Spectrum { curve: curve.max(0.05), ..self }
    }

    /// Bars up to `amount` (0 to 1) shorter than their energy, a different amount per bar,
    /// from `seed`. The same seed gives the same ring.
    pub fn jitter(self, amount: f32, seed: u32) -> Spectrum {
        Spectrum { jitter: amount.clamp(0.0, 1.0), seed, ..self }
    }

    /// The jitter moves to new values `rate` times a second, eased between them.
    pub fn drift(self, rate: f32) -> Spectrum {
        Spectrum { drift: rate.max(0.0), ..self }
    }

    /// Bar `i`'s jitter at `t`, 0 to 1: a hash of the bar, the seed and the drift step.
    pub fn jitter_at(&self, i: u32, t: f64) -> f32 {
        if self.jitter <= 0.0 {
            return 0.0;
        }
        let h = |step: i64| hash(i, self.seed, step);
        if self.drift <= 0.0 {
            return self.jitter * h(0);
        }
        let x = t * self.drift as f64;
        let (k, f) = (x.floor() as i64, x.fract() as f32);
        let ease = f * f * (3.0 - 2.0 * f);
        self.jitter * (h(k) + (h(k + 1) - h(k)) * ease)
    }

    pub fn glow(self, radius: f32, strength: f32) -> Spectrum {
        Spectrum { glow: Some((radius, strength)), ..self }
    }

    /// The energy of each band at `t`, lowest first.
    pub fn energies(&self, signals: Option<&Signals>, t: f64) -> Vec<f32> {
        match &self.bands {
            Bands::Fixed(v) => v.iter().map(|e| e.clamp(0.0, 1.0)).collect(),
            Bands::Signals { lo, hi, attack, release } => {
                let (lo, hi) = (*lo.min(hi), *hi.max(lo));
                let hi = hi.min(MEL_BANDS as u8 - 1);
                match signals {
                    Some(s) => {
                        (lo..=hi).map(|b| s.envelope(Sig::Band(b), *attack, *release, t).clamp(0.0, 1.0)).collect()
                    }
                    None => vec![0.0; (hi - lo + 1) as usize],
                }
            }
        }
    }

    /// Bar `i`'s place across the bands, 0 lowest to 1 highest.
    fn band_at(&self, i: u32) -> f32 {
        let n = self.count.max(1);
        let closed = self.sweep.abs() >= std::f32::consts::TAU - 1e-4;
        let u = if closed || n == 1 { (i as f32 + 0.5) / n as f32 } else { i as f32 / (n - 1) as f32 };
        if self.mirror {
            1.0 - (2.0 * u - 1.0).abs()
        } else {
            u
        }
    }

    /// Bar `i`'s angle.
    fn angle(&self, i: u32) -> f32 {
        let n = self.count.max(1);
        let closed = self.sweep.abs() >= std::f32::consts::TAU - 1e-4;
        let steps = if closed { n } else { (n - 1).max(1) };
        self.start + self.sweep * i as f32 / steps as f32
    }

    /// Whether the scene must keep drawing at `t`: frames are arriving, the release runs, or
    /// the jitter drifts.
    pub(crate) fn moving(&self, signals: Option<&SignalsCell>, t: f64) -> bool {
        if self.jitter > 0.0 && self.drift > 0.0 {
            return true;
        }
        let Bands::Signals { release, .. } = self.bands else { return false };
        let Some(cell) = signals else { return false };
        cell.get().active_until(6.0 * release as f64).is_some_and(|until| t < until)
    }
}

impl<M> Element<M> {
    /// A radial spectrum filling the box: see [`Spectrum`]. It reads the scene's signal bus
    /// (`Scene::set_signals`).
    pub fn spectrum_ring(key: impl Into<Key>, spectrum: Spectrum) -> Element<M> {
        Element::new(key, Kind::Spectrum(spectrum), taffy::Style::default())
    }
}

/// A number from 0 to 1 for bar `i`, `seed` and drift `step` (a small integer hash).
fn hash(i: u32, seed: u32, step: i64) -> f32 {
    let mut x = (i as u64) ^ ((seed as u64) << 32) ^ (step as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

fn mix(a: Color, b: Color, k: f32) -> Color {
    Color::rgba(a.r + (b.r - a.r) * k, a.g + (b.g - a.g) * k, a.b + (b.b - a.b) * k, a.a + (b.a - a.a) * k)
}

/// The bars at `t`, at `origin` in the current layer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    s: &Spectrum,
    signals: Option<&SignalsCell>,
    t: f64,
    origin: Point,
    size: Size,
    alpha: f32,
    items: &mut Vec<Item>,
    out: &mut DisplayList,
) {
    if s.count == 0 {
        return;
    }
    let store = signals.map(SignalsCell::get);
    let energies = s.energies(store.as_deref(), t);
    if energies.is_empty() {
        return;
    }
    let centre = Point::new(origin.x + 0.5 * size.w, origin.y + 0.5 * size.h);
    let outer = 0.5 * size.w.min(size.h);
    let ring = outer * s.inner;
    let gap = s.sweep.abs() / s.count as f32;
    let w = (ring.max(1.0) * gap * s.width).max(0.5);
    let room = (outer - ring - 0.5 * w).max(0.0) * s.reach;
    for i in 0..s.count {
        let x = s.band_at(i) * (energies.len() - 1) as f32;
        let k = x.floor() as usize;
        let (a, b) = (energies[k], energies[(k + 1).min(energies.len() - 1)]);
        let e = (a + (b - a) * x.fract()).clamp(0.0, 1.0).powf(s.curve);
        let len = s.rest + e * room * (1.0 - s.jitter_at(i, t));
        let (r0, r1) = if s.both_ways {
            let half = 0.5 * len.min(2.0 * (ring - 0.5 * w).max(0.0));
            (ring - half, ring + half)
        } else {
            (ring, ring + len)
        };
        let (sin, cos) = s.angle(i).sin_cos();
        let at = |r: f32| Point::new(centre.x + r * cos, centre.y + r * sin);
        let c = mix(s.low, s.high, e).fade(alpha);
        if c.a <= 0.0 {
            continue;
        }
        let line = ShapeInstance::line(at(r0), at(r1), w, Paint::NONE);
        if let Some((radius, strength)) = s.glow {
            push(items, out, ShapeInstance { fill: Paint::glow(c, radius, strength * e.max(0.15)), ..line });
        }
        push(items, out, ShapeInstance { stroke: Paint::solid(c), ..line });
    }
}

fn push(items: &mut Vec<Item>, out: &mut DisplayList, s: ShapeInstance) {
    let index = out.shapes.len() as u32;
    out.shapes.push(s);
    match items.last_mut() {
        Some(last) if last.kind == ItemKind::Shapes && last.start + last.len == index => last.len += 1,
        _ => items.push(Item { kind: ItemKind::Shapes, start: index, len: 1 }),
    }
}

/// How far a spectrum draws outside its box: its glow. Bars stop at the box's inscribed
/// circle.
pub(crate) fn reach(s: &Spectrum) -> f32 {
    s.glow.map_or(0.0, |(r, k)| crate::reach::glow_reach(r, k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;
    use vo_core::signals::SignalFrame;

    fn lengths(list: &DisplayList) -> Vec<f32> {
        list.shapes.iter().map(|s| (s.geom[2] - s.geom[0]).hypot(s.geom[3] - s.geom[1])).collect()
    }

    fn ring_of(spectrum: Spectrum) -> Scene<()> {
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(200.0, 200.0), 1.0);
        scene.apply(Element::spectrum_ring("s", spectrum).size(200.0, 200.0), 0.0);
        scene
    }

    #[test]
    fn fixed_bands_set_the_bar_lengths() {
        let bands: Arc<[f32]> = Arc::from([0.0f32, 1.0].as_slice());
        let s = Spectrum::new(Color::WHITE, Color::WHITE).bands(Bands::Fixed(bands)).count(4).rest(0.0).ring(0.5, 1.0);
        let len = lengths(&ring_of(s).sample(0.0));
        assert_eq!(len.len(), 4);
        // Bars at 1/8, 3/8, 5/8 and 7/8 across the bands; the room is 100 - 50 - w / 2.
        let w = 50.0 * std::f32::consts::TAU / 4.0 * 0.55;
        let room = 50.0 - 0.5 * w;
        for (l, e) in len.iter().zip([0.125, 0.375, 0.625, 0.875]) {
            assert!((l - e * room).abs() < 1e-3, "{l} vs {}", e * room);
        }
    }

    #[test]
    fn mirror_puts_the_low_bands_at_both_ends() {
        let bands: Arc<[f32]> = Arc::from([0.0f32, 1.0].as_slice());
        let s = Spectrum::new(Color::WHITE, Color::WHITE).bands(Bands::Fixed(bands)).count(4).mirror();
        let len = lengths(&ring_of(s).sample(0.0));
        assert!((len[0] - len[3]).abs() < 1e-3 && (len[1] - len[2]).abs() < 1e-3 && len[1] > len[0]);
    }

    #[test]
    fn signal_bands_drive_the_ring_and_keep_it_awake() {
        let frames = (0..50).map(|i| {
            let mut f = SignalFrame::silent(i as f64 * 0.01);
            f.mel[MEL_BANDS - 4..].fill(1.0);
            f
        });
        let cell = SignalsCell::new(Signals::recorded(frames, 0.0));
        let s = Spectrum::new(Color::WHITE, Color::WHITE).count(8).rest(0.0).ring(0.5, 1.0);
        let mut scene = ring_of(s);
        scene.set_signals(cell);
        let len = lengths(&scene.sample(0.4));
        // The highest bands are loud: the last bar is the longest, the first is still.
        assert!(len[7] > 20.0 && len[0] < 1e-3, "{len:?}");
        assert_eq!(scene.next_wake(0.4), Some(0.4));
        assert_eq!(scene.next_wake(30.0), None);
    }

    #[test]
    fn jitter_varies_bars_the_same_way_every_time() {
        let bands: Arc<[f32]> = Arc::from([1.0f32, 1.0].as_slice());
        let s = Spectrum::new(Color::WHITE, Color::WHITE).bands(Bands::Fixed(bands)).count(16).rest(0.0).jitter(0.5, 7);
        let a = lengths(&ring_of(s.clone()).sample(0.0));
        // Equal energies, unequal bars, none shorter than half.
        let (lo, hi) = a.iter().fold((f32::MAX, 0.0f32), |(l, h), &x| (l.min(x), h.max(x)));
        assert!(hi - lo > 0.1 * hi && lo >= 0.5 * hi - 1e-3, "{a:?}");
        // The same seed gives the same ring, at any time; another seed a different one.
        assert_eq!(a, lengths(&ring_of(s.clone()).sample(3.0)));
        assert_ne!(a, lengths(&ring_of(s.clone().jitter(0.5, 8)).sample(0.0)));
        // With drift the bars move smoothly: close together 10 ms apart.
        let d = s.drift(2.0);
        assert!((d.jitter_at(3, 1.0) - d.jitter_at(3, 1.01)).abs() < 0.02);
        assert!((0..16).any(|i| (d.jitter_at(i, 0.0) - d.jitter_at(i, 0.5)).abs() > 0.05));
    }
}
