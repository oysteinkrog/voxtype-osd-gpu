//! Expression channels (SIG02): element props driven by the signal bus.
//!
//! `Element::bind(Param::Scale, Bind::new(Sig::Level).output(1.0, 1.3))` makes the element's
//! scale follow the voice level. The bound value composes onto the prop's own spring value at
//! sample time (multiply, add or mix, see [`Bind`]), so view changes still animate as before
//! and the signal rides on top. `Element::bind_color` mixes a colour toward a target by a
//! signal.
//!
//! The scene reads the store the host gives it with `Scene::set_signals`. Sampling stays pure
//! in `t`: the store places frames by the audio clock. A node with bindings keeps the scene
//! awake while frames arrive and for the bindings' release tail after.
//!
//! A bound prop still needs a base: a glow radius bound with [`Compose::Add`] draws only if
//! the element has a glow.

use vo_core::signals::{Bind, Sig, Signals, SignalsCell};

use crate::element::{Element, Prop, Slot};
use crate::geom::Color;

/// A scalar the signal bus can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Param {
    Opacity,
    OffsetX,
    OffsetY,
    Scale,
    Rotation,
    Radius,
    StrokeWidth,
    GlowRadius,
    GlowStrength,
    ShadowX,
    ShadowY,
    ShadowBlur,
    BloomStrength,
    BloomRadius,
    RimWidth,
    RimAngle,
    TrimStart,
    TrimEnd,
    Morph,
    TextBlur,
    TextDissolve,
    TextWipe,
    TextChroma,
    TextHaloRadius,
    StyleChroma,
    StyleGlitch,
    StyleScanlines,
    StyleVignette,
    StyleFlicker,
    WaveAmp,
    WaveLength,
    WavePhase,
    /// Displacement strength (EG-03).
    DisplaceStrength,
}

impl Param {
    fn slot(self) -> u8 {
        (match self {
            Param::Opacity => Slot::Opacity,
            Param::OffsetX => Slot::OffsetX,
            Param::OffsetY => Slot::OffsetY,
            Param::Scale => Slot::Scale,
            Param::Rotation => Slot::Rotation,
            Param::Radius => Slot::Radius,
            Param::StrokeWidth => Slot::StrokeWidth,
            Param::GlowRadius => Slot::GlowRadius,
            Param::GlowStrength => Slot::GlowStrength,
            Param::ShadowX => Slot::ShadowX,
            Param::ShadowY => Slot::ShadowY,
            Param::ShadowBlur => Slot::ShadowBlur,
            Param::BloomStrength => Slot::BloomStrength,
            Param::BloomRadius => Slot::BloomRadius,
            Param::RimWidth => Slot::RimWidth,
            Param::RimAngle => Slot::RimAngle,
            Param::TrimStart => Slot::TrimStart,
            Param::TrimEnd => Slot::TrimEnd,
            Param::Morph => Slot::Morph,
            Param::TextBlur => Slot::TextBlur,
            Param::TextDissolve => Slot::TextDissolve,
            Param::TextWipe => Slot::TextWipe,
            Param::TextChroma => Slot::TextChroma,
            Param::TextHaloRadius => Slot::TextHaloRadius,
            Param::StyleChroma => Slot::StyleChroma,
            Param::StyleGlitch => Slot::StyleGlitch,
            Param::StyleScanlines => Slot::StyleScanlines,
            Param::StyleVignette => Slot::StyleVignette,
            Param::StyleFlicker => Slot::StyleFlicker,
            Param::WaveAmp => Slot::WaveAmp,
            Param::WaveLength => Slot::WaveLength,
            Param::WavePhase => Slot::WavePhase,
            Param::DisplaceStrength => Slot::DisplaceStrength,
        }) as u8
    }
}

/// One binding on one slot. `target` is set for a colour channel mixed toward it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bound {
    pub slot: u8,
    pub bind: Bind,
    pub target: Option<f32>,
}

impl<M> Element<M> {
    /// Hands the scene the signal store, on the root element. Bound props, `wake_on_signals`
    /// and the shaders' signal block read it. The same as `Scene::set_signals`.
    pub fn signals(mut self, cell: SignalsCell) -> Element<M> {
        self.signals = Some(cell);
        self
    }

    /// Drives `param` from the signal bus. Bindings on one param apply in the order given.
    pub fn bind(mut self, param: Param, bind: Bind) -> Element<M> {
        self.binds.push(Bound { slot: param.slot(), bind, target: None });
        self
    }

    /// Mixes a colour prop toward `target` by the bound signal (0 keeps the element's colour,
    /// 1 is `target`). `prop` is `Fill` (both fills), `Stroke`, `TextColor`, `Glow`, `Shadow`,
    /// `Rim` or `Sheen`; other props are ignored.
    pub fn bind_color(mut self, prop: Prop, target: Color, bind: Bind) -> Element<M> {
        let bases: &[Slot] = match prop {
            Prop::Fill => &[Slot::Fill0, Slot::Fill1],
            Prop::Stroke => &[Slot::Stroke],
            Prop::TextColor => &[Slot::Text],
            Prop::Glow => &[Slot::GlowColor],
            Prop::Shadow => &[Slot::ShadowColor],
            Prop::Rim => &[Slot::RimColor],
            Prop::Sheen => &[Slot::SheenColor],
            _ => &[],
        };
        let rgba = [target.r, target.g, target.b, target.a];
        for &base in bases {
            for (i, &c) in rgba.iter().enumerate() {
                self.binds.push(Bound { slot: base as u8 + i as u8, bind, target: Some(c) });
            }
        }
        self
    }
}

impl<M> Element<M> {
    /// Keeps the scene awake while signal frames arrive and for `tail` seconds after, for
    /// content that reads the signal bus itself (a canvas, a dot source) rather than through
    /// a bound prop.
    pub fn wake_on_signals(mut self, tail: f32) -> Element<M> {
        let bind = Bind::new(Sig::Level).smooth(0.0, tail / 6.0);
        self.binds.push(Bound { slot: WAKE_SLOT, bind, target: None });
        self
    }
}

/// A slot no prop reads: a binding on it only keeps the scene awake.
const WAKE_SLOT: u8 = u8::MAX;

/// `base` with every binding on `slot` applied at `t`.
pub(crate) fn apply(binds: &[Bound], signals: Option<&Signals>, slot: u8, base: f64, t: f64) -> f64 {
    let Some(signals) = signals else { return base };
    binds.iter().filter(|b| b.slot == slot).fold(base, |v, b| match b.target {
        Some(c) => {
            let k = b.bind.mix_factor(signals, t) as f64;
            v + (c as f64 - v) * k
        }
        None => b.bind.apply(v, signals, t),
    })
}

/// Seconds the bindings keep moving after the last frame.
pub(crate) fn tail(binds: &[Bound]) -> f64 {
    binds.iter().map(|b| b.bind.tail()).fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vo_core::signals::{Sig, SignalFrame, Signals, FRAME_SECS};

    use super::*;
    use crate::display::ShapeKind;
    use crate::Scene;

    /// One second of frames at `db`, starting at scene time 0, read without playout.
    fn steady(db: f32) -> SignalsCell {
        let frames = (0..100).map(|i| SignalFrame { rms_dbfs: db, ..SignalFrame::silent(i as f64 * FRAME_SECS) });
        let cell = SignalsCell::default();
        cell.set(Arc::new(Signals::recorded(frames, 0.0).with_playout(0.0)));
        cell
    }

    fn dot() -> Element {
        Element::shape("dot", ShapeKind::Circle).size(10.0, 10.0).fill(Color::WHITE)
    }

    #[test]
    fn opacity_follows_the_level_and_eases_back() {
        let mut scene = Scene::new();
        scene.set_signals(steady(-30.0));
        let bind = Bind::new(Sig::Level).output(0.2, 1.0).smooth(0.0, 0.0);
        scene.apply(dot().bind(Param::Opacity, bind), 0.0);
        let id = scene.find(&["dot"]).unwrap();
        let o = scene.sampled(id, 0.5).unwrap().opacity;
        assert!((o - 0.6).abs() < 1e-4, "{o}");
        // Long after the last frame: back to the element's own opacity, and asleep.
        assert_eq!(scene.sampled(id, 3.0).unwrap().opacity, 1.0);
        assert!(scene.next_wake(0.5).is_some());
        assert_eq!(scene.next_wake(3.0), None);
    }

    #[test]
    fn a_new_store_reaches_bound_nodes() {
        let mut scene = Scene::new();
        let bind = Bind::new(Sig::Level).output(0.0, 10.0).smooth(0.0, 0.0).add();
        scene.apply(dot().offset(5.0, 0.0).bind(Param::OffsetX, bind), 0.0);
        let id = scene.find(&["dot"]).unwrap();
        // No store yet: the spring value alone.
        assert_eq!(scene.sampled(id, 0.5).unwrap().transform.e, 5.0);
        scene.set_signals(steady(0.0));
        assert_eq!(scene.sampled(id, 0.5).unwrap().transform.e, 15.0);
    }

    #[test]
    fn colour_mixes_toward_the_target() {
        let red = Color::rgba(1.0, 0.0, 0.0, 1.0);
        let e = dot().bind_color(Prop::Fill, red, Bind::new(Sig::Level).smooth(0.0, 0.0));
        let signals = steady(-30.0);
        let g = apply(&e.binds, Some(&signals.get()), Slot::Fill0 as u8 + 1, 1.0, 0.5);
        assert!((g - 0.5).abs() < 1e-4, "{g}");
        assert_eq!(e.binds.len(), 8);
    }

    #[test]
    fn wake_on_signals_wakes_without_changing_anything() {
        let mut scene = Scene::new();
        scene.set_signals(steady(-30.0));
        scene.apply(dot().wake_on_signals(0.5), 0.0);
        let id = scene.find(&["dot"]).unwrap();
        assert_eq!(scene.sampled(id, 0.5).unwrap().opacity, 1.0);
        assert!(scene.next_wake(1.4).is_some());
        assert_eq!(scene.next_wake(3.0), None);
    }

    #[test]
    fn display_list_carries_the_block() {
        let mut scene = Scene::new();
        scene.apply(dot().signals(steady(-30.0)), 0.0);
        let dl = scene.sample(0.5);
        assert_eq!(dl.signals.head[1], 1.0);
        assert!((dl.signals.head[4] - 0.5).abs() < 1e-4);
    }
}
