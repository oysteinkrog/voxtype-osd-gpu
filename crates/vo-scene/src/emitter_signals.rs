//! Emitter parameters driven by the signal bus (SIG03).
//!
//! [`SignalEmitter`] wraps any [`EmitterSource`] and composes [`Bind`]s onto its simulation
//! inputs and its look, each evaluated at the tick's or frame's own time:
//!
//! ```ignore
//! let source = SignalEmitter::new(MyEmitter::default(), cell.clone())
//!     .bind(EmitParam::Emit, Bind::new(Sig::Level).output(0.1, 1.0))
//!     .bind(EmitParam::Swirl, Bind::new(Sig::Pitch).input(0.2, 0.6).output(-1.0, 1.0).mix())
//!     .bind(EmitParam::SpawnSpeed, Bind::new(Sig::OnsetPulse { tau: 0.1 }).output(0.0, 80.0).add());
//! Element::emitter("sparks", Emitter::new(Arc::new(source), 4096, 7))
//! ```
//!
//! `epoch` and `shock` are left alone: a changed epoch respawns the pool, and a shock's start
//! must stay fixed. The path of a `Shape::Path` source is passed through.
//!
//! The wrapped source's own values are the base, as with bound props (`crate::bind`): a
//! multiply or add eases back to them when the stream stops.

use vo_core::signals::{Bind, SignalsCell};

use std::sync::Arc;

use crate::emitter::{EmitterLook, EmitterSim, EmitterSource};
use crate::geom::Point;

/// An emitter input the signal bus can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EmitParam {
    /// Share of deaths that respawn.
    Emit,
    Life,
    SpawnSpeed,
    Swirl,
    RingPull,
    RingRadius,
    Drag,
    Noise,
    NoiseScale,
    NoiseSpeed,
    AttractStrength,
    GravityX,
    GravityY,
    /// The level the simulation sees (`EmitterSim::level`).
    Level,
    /// Look: sprite size, brightness and trail alpha.
    Size,
    Brightness,
    TrailAlpha,
}

/// An emitter whose inputs follow signals. See the module docs.
#[derive(Debug)]
pub struct SignalEmitter<S> {
    pub inner: S,
    pub cell: SignalsCell,
    pub binds: Vec<(EmitParam, Bind)>,
}

impl<S: EmitterSource> SignalEmitter<S> {
    pub fn new(inner: S, cell: SignalsCell) -> SignalEmitter<S> {
        SignalEmitter { inner, cell, binds: Vec::new() }
    }

    /// Drives `param` with `bind`. Bindings on one param apply in the order given.
    pub fn bind(mut self, param: EmitParam, bind: Bind) -> SignalEmitter<S> {
        self.binds.push((param, bind));
        self
    }

    fn apply(&self, param: EmitParam, base: f32, t: f64) -> f32 {
        let mut bound = self.binds.iter().filter(|(p, _)| *p == param).peekable();
        if bound.peek().is_none() {
            return base;
        }
        let signals = self.cell.get();
        bound.fold(base as f64, |v, (_, b)| b.apply(v, &signals, t)) as f32
    }
}

impl<S: EmitterSource> EmitterSource for SignalEmitter<S> {
    fn sim(&self, t: f64) -> EmitterSim {
        let mut s = self.inner.sim(t);
        let f = |p: EmitParam, v: &mut f32| *v = self.apply(p, *v, t);
        f(EmitParam::Emit, &mut s.emit);
        f(EmitParam::Life, &mut s.life);
        f(EmitParam::SpawnSpeed, &mut s.spawn_speed);
        f(EmitParam::Swirl, &mut s.swirl);
        f(EmitParam::RingPull, &mut s.ring_pull);
        f(EmitParam::RingRadius, &mut s.ring_radius);
        f(EmitParam::Drag, &mut s.drag);
        f(EmitParam::Noise, &mut s.noise);
        f(EmitParam::NoiseScale, &mut s.noise_scale);
        f(EmitParam::NoiseSpeed, &mut s.noise_speed);
        f(EmitParam::AttractStrength, &mut s.attract_strength);
        f(EmitParam::GravityX, &mut s.gravity[0]);
        f(EmitParam::GravityY, &mut s.gravity[1]);
        f(EmitParam::Level, &mut s.level);
        s.emit = s.emit.clamp(0.0, 1.0);
        s
    }

    fn look(&self, t: f64) -> EmitterLook {
        let mut l = self.inner.look(t);
        l.size = self.apply(EmitParam::Size, l.size, t);
        l.brightness = self.apply(EmitParam::Brightness, l.brightness, t);
        l.trail_alpha = self.apply(EmitParam::TrailAlpha, l.trail_alpha, t);
        l
    }

    fn path(&self) -> Option<Arc<[Point]>> {
        self.inner.path()
    }
}

#[cfg(test)]
mod tests {
    use vo_core::signals::{Sig, SignalFrame, Signals, FRAME_SECS};

    use super::*;

    #[derive(Debug)]
    struct Base;

    impl EmitterSource for Base {
        fn sim(&self, _: f64) -> EmitterSim {
            EmitterSim { emit: 0.5, spawn_speed: 10.0, ..EmitterSim::ring(20.0, 2.0) }
        }

        fn look(&self, _: f64) -> EmitterLook {
            EmitterLook { size: 2.0, ..EmitterLook::default() }
        }
    }

    fn cell(db: f32) -> SignalsCell {
        let frames = (0..100).map(|i| SignalFrame { rms_dbfs: db, ..SignalFrame::silent(i as f64 * FRAME_SECS) });
        SignalsCell::new(Signals::recorded(frames, 0.0).with_playout(0.0))
    }

    #[test]
    fn binds_compose_onto_the_inner_source() {
        let e = SignalEmitter::new(Base, cell(-30.0))
            .bind(EmitParam::Emit, Bind::new(Sig::Level).smooth(0.0, 0.0).output(0.0, 4.0))
            .bind(EmitParam::SpawnSpeed, Bind::new(Sig::Level).smooth(0.0, 0.0).output(0.0, 30.0).add())
            .bind(EmitParam::Size, Bind::new(Sig::Level).smooth(0.0, 0.0).output(1.0, 2.0));
        let s = e.sim(0.5);
        // 0.5 * 2.0, clamped to 1.
        assert_eq!(s.emit, 1.0);
        assert!((s.spawn_speed - 25.0).abs() < 1e-4);
        assert!((e.look(0.5).size - 3.0).abs() < 1e-4);
        // Unbound inputs pass through, and the binds ease out after the stream.
        assert_eq!(s.swirl, Base.sim(0.5).swirl);
        assert_eq!(e.sim(5.0), Base.sim(5.0));
    }
}
