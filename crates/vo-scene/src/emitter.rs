//! Particle emitters as elements (`Element::emitter`) and particle words for live text.
//!
//! The renderer owns the particle simulation (`vo_render::particles`). It steps at a fixed
//! tick, and each tick's inputs come from the element's [`EmitterSource`], asked at that tick's
//! own time. Sources are pure functions of `t`, like `Wave` and `DotSource`, so a 144 Hz
//! session, a 60 fps export and a replay after a seek draw the same frames.
//!
//! Values an app sets over time, such as a voice level, go into the source as history: a
//! [`LevelTrack`] holds every target with its time and eases between them with a closed-form
//! spring. The app pushes a new level and rebuilds its view; nothing in the source is
//! mutable.
//!
//! The types here are plain data. The GPU side packs them.

use std::fmt::Debug;
use std::sync::Arc;

use serde::Serialize;

use crate::geom::{Color, Point};

/// Where an emitter spawns, in its own space. Parameters may change from tick to tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Point,
    /// A circle in the xy plane; `spread` is the band width as a share of the radius.
    Ring {
        radius: f32,
        spread: f32,
    },
    Disc {
        radius: f32,
    },
    /// A segment through the origin, `angle` in radians from +x.
    Line {
        half_length: f32,
        angle: f32,
    },
    Box {
        half: [f32; 3],
    },
    /// A torus about z: `major` to the tube's centre, `minor` the tube's radius (D302).
    Torus {
        major: f32,
        minor: f32,
    },
}

/// A shockwave: a front leaving the emitter's origin (PA05).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shock {
    /// Seconds since it started.
    pub age: f32,
    /// Front speed in units per second, and its width in units.
    pub speed: f32,
    pub width: f32,
    /// Push in units per second squared at the front.
    pub strength: f32,
}

/// An emitter's simulation inputs at one tick time. Everything that should follow a level, a
/// state or a burst goes here, computed from the tick's own time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterSim {
    pub shape: Shape,
    /// Share of deaths that respawn, 0 to 1. 0 lets the pool die out.
    pub emit: f32,
    /// Mean life in seconds and its jitter as a share of it.
    pub life: f32,
    pub life_jitter: f32,
    /// Speed outward from the shape at spawn.
    pub spawn_speed: f32,
    /// Swirl about z in radians per second.
    pub swirl: f32,
    /// Pull towards the circle of `ring_radius` (the torus core in 3D), per second squared.
    pub ring_pull: f32,
    pub ring_radius: f32,
    /// Velocity damping per second.
    pub drag: f32,
    /// Curl noise strength (units per second squared), scale (cells per unit) and time speed.
    pub noise: f32,
    pub noise_scale: f32,
    pub noise_speed: f32,
    /// A point attractor and its strength per second squared per unit of distance (negative
    /// repels).
    pub attract: [f32; 3],
    pub attract_strength: f32,
    /// Gravity plus wind, units per second squared.
    pub gravity: [f32; 3],
    pub shock: Option<Shock>,
    /// Text mask force (PA08): positive pushes out of the text, negative hugs glyph edges.
    pub mask: f32,
    /// Burst counter: when it changes, the whole pool respawns on that tick.
    pub epoch: u32,
    /// The level, 0 to 1. Only recorded; the other fields carry its effect.
    pub level: f32,
}

impl Default for EmitterSim {
    fn default() -> Self {
        EmitterSim {
            shape: Shape::Point,
            emit: 1.0,
            life: 1.0,
            life_jitter: 0.5,
            spawn_speed: 0.0,
            swirl: 0.0,
            ring_pull: 0.0,
            ring_radius: 0.0,
            drag: 1.5,
            noise: 0.0,
            noise_scale: 0.02,
            noise_speed: 0.7,
            attract: [0.0; 3],
            attract_strength: 0.0,
            gravity: [0.0; 3],
            shock: None,
            mask: 0.0,
            epoch: 0,
            level: 0.0,
        }
    }
}

impl EmitterSim {
    /// A ring of `radius` that holds its particles on it (the voice ring, PA04).
    pub fn ring(radius: f32, spread: f32) -> EmitterSim {
        EmitterSim { shape: Shape::Ring { radius, spread }, ring_pull: 40.0, ring_radius: radius, ..Default::default() }
    }

    /// A torus about z that holds its particles near its core (D302).
    pub fn torus(major: f32, minor: f32) -> EmitterSim {
        EmitterSim { shape: Shape::Torus { major, minor }, ring_pull: 30.0, ring_radius: major, ..Default::default() }
    }
}

/// How a sprite is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum SpriteStyle {
    /// A soft glow.
    Soft,
    /// A hard disc, snapped to a grid of `pitch` pixels when `pitch` is above 0 (PA09).
    Dot { pitch: f32 },
    /// Out of focus by depth (FX16): sprites at clip depth `focus` are sharp, others grow by
    /// `aperture` pixels per unit of depth into flat discs with the same light. 3D only.
    Bokeh { focus: f32, aperture: f32 },
}

/// An emitter's look at the frame time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmitterLook {
    /// Where the emitter's origin is in layer space, and its z (3D only). An element's
    /// emitter adds this to the centre of its box.
    pub origin: Point,
    pub z: f32,
    /// Rows of a rotation applied to emitter space before the origin: a tilt for a 3D ring.
    pub rotation: [[f32; 3]; 3],
    /// Sprite radius in logical units.
    pub size: f32,
    pub brightness: f32,
    /// Colour at birth, in mid-life and at death (sRGB, straight alpha).
    pub ramp: [Color; 3],
    pub style: SpriteStyle,
    /// Ribbon width as a share of the sprite, and its opacity (PA07; hero particles only).
    pub trail_width: f32,
    pub trail_alpha: f32,
}

impl Default for EmitterLook {
    fn default() -> Self {
        EmitterLook {
            origin: Point::ZERO,
            z: 0.0,
            rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            size: 1.5,
            brightness: 1.0,
            ramp: [Color::WHITE, Color::WHITE, Color::hex(0xffffff, 0.0)],
            style: SpriteStyle::Soft,
            trail_width: 0.8,
            trail_alpha: 0.6,
        }
    }
}

/// A rotation about x by `a` radians, as rows, for [`EmitterLook::rotation`].
pub fn tilt_x(a: f32) -> [[f32; 3]; 3] {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

/// What an emitter element does over time.
pub trait EmitterSource: Debug + Send + Sync {
    /// The simulation inputs at `t`. Called once per tick, with the tick's time.
    fn sim(&self, t: f64) -> EmitterSim;

    /// The look at the frame time `t`. The origin is relative to the centre of the box.
    fn look(&self, t: f64) -> EmitterLook;
}

/// An emitter element's content (`Element::emitter`).
#[derive(Clone, Debug)]
pub struct Emitter {
    pub source: Arc<dyn EmitterSource>,
    /// Particles in its pool; the renderer rounds it up to a multiple of 64.
    pub capacity: u32,
    pub seed: u32,
    /// Particles that keep a ribbon of their last positions (PA07).
    pub trails: u32,
    /// Drawn through the 3D camera (D302). A 2D emitter keeps z at 0.
    pub three_d: bool,
    /// The renderer keeps one particle pool per element and starts it again when this
    /// changes. A new source alone does not restart it, so a view that rebuilds its source
    /// for every new level keeps its particles.
    pub generation: u32,
}

impl Emitter {
    pub fn new(source: Arc<dyn EmitterSource>, capacity: u32, seed: u32) -> Emitter {
        Emitter { source, capacity, seed, trails: 0, three_d: false, generation: 0 }
    }
}

impl PartialEq for Emitter {
    /// Same source object, same pool. Sources are never compared by value.
    fn eq(&self, o: &Emitter) -> bool {
        Arc::ptr_eq(&self.source, &o.source)
            && (self.capacity, self.seed, self.trails, self.three_d, self.generation)
                == (o.capacity, o.seed, o.trails, o.three_d, o.generation)
    }
}

/// A source handle in the display list. Compared by identity; not serialized.
#[derive(Clone, Debug)]
pub struct SourceRef(pub Arc<dyn EmitterSource>);

impl PartialEq for SourceRef {
    fn eq(&self, o: &SourceRef) -> bool {
        Arc::ptr_eq(&self.0, &o.0)
    }
}

/// One emitter element in a frame, in its layer's space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct EmitterInstance {
    /// The element's node, as `NodeId::to_bits`: with `generation`, the key of its pool.
    pub node: u64,
    pub generation: u32,
    /// Index into `DisplayList::emitter_sources`.
    pub source: u32,
    pub capacity: u32,
    pub seed: u32,
    pub trails: u32,
    pub three_d: bool,
    /// When the element was created. Its pool spawns nothing before.
    pub since: f64,
    /// The time this frame shows.
    pub t: f64,
    /// The centre of its box.
    pub pos: Point,
    /// Inherited opacity, multiplied into the brightness.
    pub opacity: f32,
}

/// Adds an emitter element's instance and its source to `out`; returns the instance index.
pub(crate) fn push_instance(
    out: &mut crate::display::DisplayList,
    e: &Emitter,
    node: u64,
    since: f64,
    t: f64,
    pos: Point,
    opacity: f32,
) -> u32 {
    let source = match out.emitter_sources.iter().position(|s| Arc::ptr_eq(&s.0, &e.source)) {
        Some(i) => i,
        None => {
            out.emitter_sources.push(SourceRef(e.source.clone()));
            out.emitter_sources.len() - 1
        }
    };
    out.emitters.push(EmitterInstance {
        node,
        generation: e.generation,
        source: source as u32,
        capacity: e.capacity,
        seed: e.seed,
        trails: e.trails,
        three_d: e.three_d,
        since,
        t,
        pos,
        opacity,
    });
    out.emitters.len() as u32 - 1
}

/// How a live text's words look while they form from particles
/// (`Element::word_particles`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct WordParticles {
    /// Colour of the forming particles (sRGB, straight alpha).
    pub color: Color,
    /// Sprite radius in logical units; 0 sizes it from the stroke.
    pub size: f32,
    pub style: SpriteStyle,
    /// Particle density: particles per logical unit of stroke at 1 means one per pixel at
    /// scale 1.
    pub density: f32,
}

impl Default for WordParticles {
    fn default() -> WordParticles {
        WordParticles { color: Color::hex(0xbfe4ff, 1.0), size: 0.0, style: SpriteStyle::Soft, density: 1.0 }
    }
}

/// A live-text word that forms from particles, with the time its text last changed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ParticleWord {
    /// `GlyphInstance::word` of its glyphs.
    pub word: u32,
    /// When the word entered or its text last changed: the particles re-form from there.
    pub since: f64,
    pub look: WordParticles,
}

/// The particle words of a frame.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParticleWords {
    /// The time this frame shows.
    pub t: f64,
    pub words: Vec<ParticleWord>,
}

/// How long after a word comes, changes or goes its particles still move, in seconds: the
/// scene keeps frames coming this long.
pub const WORD_PARTICLES_BUSY: f64 = 0.5;

/// A level an app sets over time, eased by a critically damped spring. Pure in `t`: each
/// entry keeps the spring's value and speed at its own time, so a lookup is a binary search
/// and one closed-form step.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelTrack {
    /// `(time, target, value at time, speed at time)`, in time order.
    points: Arc<Vec<(f64, f32, f32, f32)>>,
    /// Spring rate per second: about 4 / rate seconds to settle.
    pub rate: f32,
    /// History kept behind the newest entry, in seconds.
    pub keep: f64,
}

impl Default for LevelTrack {
    fn default() -> LevelTrack {
        LevelTrack::new(18.0)
    }
}

impl LevelTrack {
    pub fn new(rate: f32) -> LevelTrack {
        LevelTrack { points: Arc::new(Vec::new()), rate, keep: 30.0 }
    }

    /// Sets the target to `level` from `t` on. Times must not go back.
    pub fn push(&mut self, t: f64, level: f32) {
        let (x, v) = self.state(t);
        let points = Arc::make_mut(&mut self.points);
        points.push((t, level, x, v));
        let cut = points.partition_point(|p| p.0 < t - self.keep);
        if cut > 0 {
            points.drain(..cut);
        }
    }

    /// The eased level at `t`.
    pub fn at(&self, t: f64) -> f32 {
        self.state(t).0
    }

    fn state(&self, t: f64) -> (f32, f32) {
        let i = self.points.partition_point(|p| p.0 <= t);
        let Some(&(t0, goal, x0, v0)) = i.checked_sub(1).and_then(|i| self.points.get(i)) else {
            return (self.points.first().map_or(0.0, |p| p.2), 0.0);
        };
        let tau = (t - t0) as f32;
        let w = self.rate;
        let d0 = x0 - goal;
        let b = v0 + w * d0;
        let e = (-w * tau).exp();
        (goal + (d0 + b * tau) * e, (v0 - w * b * tau) * e)
    }
}

/// A ring that follows a voice level (PA04): it swells, spins faster and throws particles
/// out as the level rises, and a burst sends a shockwave through it.
#[derive(Clone, Debug, PartialEq)]
pub struct RingEmitter {
    pub radius: f32,
    /// Band width as a share of the radius.
    pub spread: f32,
    pub level: LevelTrack,
    /// Burst times, in order.
    pub bursts: Arc<[f64]>,
    /// Colour at birth, in mid-life and at death (sRGB, straight alpha).
    pub ramp: [Color; 3],
    /// Sprite radius in logical units.
    pub size: f32,
}

impl RingEmitter {
    pub fn new(radius: f32) -> RingEmitter {
        RingEmitter {
            radius,
            spread: 0.08,
            level: LevelTrack::default(),
            bursts: Arc::from([]),
            ramp: [Color::hex(0xdff4ff, 1.0), Color::hex(0x7cc4ff, 0.9), Color::hex(0x3a6bff, 0.0)],
            size: 1.4,
        }
    }
}

impl EmitterSource for RingEmitter {
    fn sim(&self, t: f64) -> EmitterSim {
        let l = self.level.at(t).clamp(0.0, 1.5);
        let epoch = self.bursts.partition_point(|&b| b <= t);
        let shock = epoch.checked_sub(1).map(|i| Shock {
            age: (t - self.bursts[i]) as f32,
            speed: self.radius * 4.0,
            width: self.radius * 0.25,
            strength: self.radius * 30.0,
        });
        EmitterSim {
            swirl: 0.5 + 2.5 * l,
            spawn_speed: self.radius * (0.1 + 0.9 * l),
            noise: self.radius * 2.0 * l,
            noise_scale: 2.0 / self.radius.max(1.0),
            life: 1.2 - 0.4 * l,
            epoch: epoch as u32,
            shock,
            level: l,
            ..EmitterSim::ring(self.radius * (1.0 + 0.15 * l), self.spread * (1.0 + 2.0 * l))
        }
    }

    fn look(&self, t: f64) -> EmitterLook {
        let l = self.level.at(t).clamp(0.0, 1.5);
        EmitterLook { size: self.size, brightness: 0.55 + 0.6 * l, ramp: self.ramp, ..EmitterLook::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_eases_to_its_target_and_does_not_depend_on_lookups() {
        let mut a = LevelTrack::new(20.0);
        a.push(0.0, 0.0);
        a.push(0.1, 1.0);
        assert_eq!(a.at(0.1), 0.0);
        assert!(a.at(0.2) > 0.5 && a.at(0.2) < 1.0);
        assert!((a.at(1.0) - 1.0).abs() < 1e-3);
        // A push mid-flight starts from the eased value and speed, so the curve stays smooth.
        let mut b = a.clone();
        b.push(0.15, 0.2);
        let (x0, _) = a.state(0.15);
        assert!((b.at(0.15) - x0).abs() < 1e-6);
        assert!((b.at(0.1501) - b.at(0.15)).abs() < 1e-3);
    }

    #[test]
    fn bursts_count_up_and_shock_from_the_last_one() {
        let mut r = RingEmitter::new(40.0);
        r.bursts = Arc::from([1.0, 2.0]);
        assert_eq!(r.sim(0.5).epoch, 0);
        assert!(r.sim(0.5).shock.is_none());
        assert_eq!(r.sim(1.5).epoch, 1);
        assert_eq!(r.sim(2.25).shock.unwrap().age, 0.25);
    }
}
