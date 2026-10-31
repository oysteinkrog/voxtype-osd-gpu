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
    /// Along the source's path ([`EmitterSource::path`]): particles flow from its start to its
    /// end at `EmitterSim::spawn_speed`, in `strands` strands. Each strand sways across the
    /// path by up to `sway` units in a wave of `wavelength` units that travels at
    /// `wave_speed` cycles per second, tapering to nothing at both ends; `width` scatters
    /// particles across their strand. Forces do not apply: the path holds them.
    Path {
        strands: u32,
        sway: f32,
        wavelength: f32,
        wave_speed: f32,
        width: f32,
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
    /// Points of light: a crisp core that keeps its energy, so a point smaller than a pixel
    /// gets dimmer, not blurrier, and dense spots saturate towards white. A share `accents`
    /// (0 to 1) of the particles are four times brighter and whiter; `gain` scales all the
    /// light; `twinkle` (0 to 1) makes each point flicker a little.
    Spark { accents: f32, gain: f32, twinkle: f32 },
    /// Tiny letters (EG-27): each free particle shows one letter of `chars` in font `font`,
    /// picked by a hash of its index, `size` logical units to the em. It turns to its
    /// direction of travel, or by `spin` turns a second when that is not 0. In light mode
    /// the letters blend normally rather than adding light, so they show on a light
    /// background. Free particles only: formed words draw a glyph style as soft sprites.
    Glyph { font: u32, chars: GlyphChars, size: f32, spin: f32 },
}

impl SpriteStyle {
    /// Points of light with a few accents.
    pub const SPARK: SpriteStyle = SpriteStyle::Spark { accents: 0.03, gain: 1.0, twinkle: 0.15 };

    /// Letters of `chars` in font `font`, `size` units to the em, turned along their travel.
    pub fn glyphs(font: u32, chars: &str, size: f32) -> SpriteStyle {
        SpriteStyle::Glyph { font, chars: GlyphChars::new(chars), size, spin: 0.0 }
    }
}

/// The letters a glyph sprite picks from (`SpriteStyle::Glyph`): up to
/// [`GlyphChars::MAX`], whitespace left out. A set is stored once for the life of the process
/// and named by its index, so a sprite style stays small and `Copy` (it sits in every
/// element's word particles).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphChars(u32);

/// Every letter set made so far, in the order they were first made.
static GLYPH_SETS: std::sync::Mutex<Vec<Arc<[char]>>> = std::sync::Mutex::new(Vec::new());

impl GlyphChars {
    /// Letters a glyph sprite can pick from.
    pub const MAX: usize = 16;

    /// The first [`GlyphChars::MAX`] letters of `text` that are not whitespace.
    pub fn new(text: &str) -> GlyphChars {
        let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).take(GlyphChars::MAX).collect();
        let mut sets = GLYPH_SETS.lock().unwrap_or_else(|e| e.into_inner());
        let at = sets.iter().position(|s| **s == *chars).unwrap_or_else(|| {
            sets.push(chars.into());
            sets.len() - 1
        });
        GlyphChars(at as u32)
    }

    /// The letters, in order.
    pub fn letters(&self) -> Arc<[char]> {
        GLYPH_SETS.lock().unwrap_or_else(|e| e.into_inner())[self.0 as usize].clone()
    }

    /// The letter particle `index` of its pool shows: the same hash (`vo_core::hash::pcg`) the
    /// GPU uses, when the font has every letter (the GPU leaves out the ones it lacks).
    pub fn pick(&self, index: u32) -> Option<char> {
        let letters = self.letters();
        let n = letters.len() as u32;
        (n > 0).then(|| letters[(vo_core::hash::pcg(index) % n) as usize])
    }
}

impl std::fmt::Debug for GlyphChars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GlyphChars({:?})", self.letters().iter().collect::<String>())
    }
}

/// The letters, not the index, which depends on the order sets were first made.
impl Serialize for GlyphChars {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.letters().iter().collect::<String>())
    }
}

/// What picks a free particle's colour from an emitter's ramp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub enum ColorBy {
    /// Birth, mid-life and death.
    #[default]
    Age,
    /// The angle about the emitter's z axis, once round the ramp and back, so it closes.
    /// The alpha still fades by age.
    Angle,
    /// The x position as drawn (after the look's rotation): the ramp's three colours sit at
    /// x = -half, 0 and +half, left to right. The alpha still fades by age.
    X { half: f32 },
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
    pub color_by: ColorBy,
    /// Path emitters: continuous glowing ribbons along the first `ribbons` strands,
    /// `ribbon_width` units wide at their core, at `ribbon_alpha`, coloured along the path by
    /// the ramp.
    pub ribbons: u32,
    pub ribbon_width: f32,
    pub ribbon_alpha: f32,
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
            color_by: ColorBy::Age,
            ribbons: 0,
            ribbon_width: 1.2,
            ribbon_alpha: 0.8,
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

    /// How far a particle can get from the box centre at `t`, in logical units, for the
    /// surface margin (`crate::reach`). The default estimates it from `sim` and `look`.
    fn reach(&self, t: f64) -> f32 {
        let path = self.path().map_or(0.0, |p| p.iter().map(|q| q.x.hypot(q.y)).fold(0.0, f32::max));
        // A fitted shape spans its square: about 0.71 of its side from the centre, plus the
        // cloud its particles come from.
        let shape = self.targets().map_or(0.0, |l| l.iter().filter(|g| g.at <= t).map(|g| g.size).fold(0.0, f32::max));
        (path + crate::reach::particle_reach(&self.sim(t), &self.look(t))).max(1.4 * shape)
    }

    /// For `Shape::Path`: the path's points in emitter space, start to end. Return the same
    /// `Arc` while the path does not change; a changed path takes effect at the next frame.
    fn path(&self) -> Option<Arc<[Point]>> {
        None
    }

    /// Shapes the particles form, each from its own time on (EG-26), in time order. The
    /// renderer forms them with up to `Emitter::capacity` particles (at most
    /// [`MAX_SHAPE_POINTS`]) next to the free pool, applying each at its recorded time, so a
    /// seek replays them. Return the same `Arc` while the list does not change; append to it
    /// as time goes on.
    fn targets(&self) -> Option<Arc<[ShapeTarget]>> {
        None
    }

    /// Masks for the PA08 mask force (`EmitterSim::mask`), each from its own time on, in
    /// time order ([`text_mask`] builds one from text). One mask is live at a time for all
    /// emitters: the latest one applied. Return the same `Arc` while the list does not change.
    fn masks(&self) -> Option<Arc<[TimedMask]>> {
        None
    }
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
    /// Reduced motion: shape targets appear already formed.
    pub reduced: bool,
}

/// Adds an emitter element's instance and its source to `out`; returns the instance index.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_instance(
    out: &mut crate::display::DisplayList,
    e: &Emitter,
    node: u64,
    since: f64,
    t: f64,
    pos: Point,
    opacity: f32,
    reduced: bool,
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
        reduced,
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
    /// Sampling density of the glyph outlines: 1 samples them at one point per pixel.
    pub density: f32,
    /// Pixels² of ink each particle stands for: 1.25 makes a dense swarm, 4 to 6 makes
    /// distinct dots. The particles together carry the solid word's light whatever this is.
    pub dot_area: f32,
    /// Share of the newest word's glyphs, counted from its end, that stay as particles while
    /// it is the newest word: 0.4 keeps the last 40 % as dots. They hand over at the normal
    /// rate once another word arrives, or once this goes to 0 (set it to 0 with the final
    /// text; the release then counts from the latest word change). 0 holds nothing.
    pub hold: f32,
    /// Where a new word's particles come from: a point on an orbit at the time the word was
    /// said, instead of a cloud round the word. `None` forms the word from a cloud round it.
    pub from: Option<WordSource>,
}

impl Default for WordParticles {
    fn default() -> WordParticles {
        WordParticles {
            color: Color::hex(0xbfe4ff, 1.0),
            size: 0.0,
            style: SpriteStyle::Soft,
            density: 1.0,
            dot_area: 1.25,
            hold: 0.0,
            from: None,
        }
    }
}

/// A light that runs along an arc of an ellipse, and the point a word's particles come
/// from: where the light was when the word was said (MO-69). The app draws the light with
/// [`WordSource::at`] so the two agree.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct WordSource {
    /// Centre of the ellipse, in the space the live text's glyphs are drawn in.
    pub centre: Point,
    /// Half width and half height.
    pub radius: [f32; 2],
    /// The arc the light runs along, in radians (y down: -pi/2 is the top), from `from`
    /// to `to`, then again from `from`.
    pub from: f32,
    pub to: f32,
    /// Seconds per run.
    pub period: f32,
    /// Seconds from a word being said to its text on screen: the light's place
    /// `lag` before the word came.
    pub lag: f32,
    /// Particles start this far round the point, as a share of the text size.
    pub spread: f32,
}

impl WordSource {
    /// The angle of the light at scene time `t`.
    pub fn angle(&self, t: f64) -> f32 {
        let k = (t / self.period.max(1e-3) as f64).rem_euclid(1.0) as f32;
        self.from + (self.to - self.from) * k
    }

    /// The light's place at scene time `t`.
    pub fn at(&self, t: f64) -> Point {
        let a = self.angle(t);
        Point::new(self.centre.x + self.radius[0] * a.cos(), self.centre.y + self.radius[1] * a.sin())
    }

    /// Where the particles of a word that came at `since` start.
    pub fn start(&self, since: f64) -> Point {
        self.at(since - self.lag as f64)
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

    /// The time of the newest entry, or `None` before the first.
    pub fn last(&self) -> Option<f64> {
        self.points.last().map(|p| p.0)
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

// --- Shapes the particles form, and the mask force's field (EG-26) ---

/// Most points a shape target forms: the renderer's formed-particle pool per shape.
pub const MAX_SHAPE_POINTS: usize = 2048;

/// What a shape target's particles form.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// Nothing: the formed particles scatter and fade.
    None,
    /// A path in emitter space. Closed contours are filled (nonzero winding): points go
    /// along the outline and inside; open ones get points along their line only.
    Path(kurbo::BezPath),
    /// An icon read by [`crate::svg::parse`], fitted into a square of the target's `size`
    /// round the emitter's origin. Strokes count as filled outlines.
    Svg(Arc<crate::svg::SvgIcon>),
    /// A check mark drawn as a line ([`crate::path::icons::check`]), fitted like an icon.
    CheckMark,
}

/// A shape the particles form from time `at` on.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeTarget {
    pub at: f64,
    pub target: Target,
    /// The side of the square an icon or the check mark is fitted into, in units. For a
    /// path, the shape's size for the cloud the particles come from and their scatter.
    pub size: f32,
    /// A left to right sweep of start delays across the points, in seconds.
    pub sweep: f32,
    /// Particles already formed scatter before they move on to this shape.
    pub scatter: bool,
}

impl ShapeTarget {
    pub fn new(at: f64, target: Target, size: f32) -> ShapeTarget {
        ShapeTarget { at, target, size, sweep: 0.0, scatter: false }
    }

    /// `n` points of the shape in emitter space: along its outlines and inside its fills.
    /// The same target always gives the same points.
    pub fn points(&self, n: usize) -> Vec<Point> {
        let fit = |path: kurbo::BezPath, view: crate::geom::Rect| {
            let side = view.w.max(view.h).max(1e-3) as f64;
            let k = self.size as f64 / side;
            let (cx, cy) = (view.x as f64 + 0.5 * view.w as f64, view.y as f64 + 0.5 * view.h as f64);
            kurbo::Affine::scale(k) * kurbo::Affine::translate((-cx, -cy)) * path
        };
        match &self.target {
            Target::None => Vec::new(),
            Target::Path(p) => sample_path(p, n, true),
            Target::Svg(icon) => sample_path(&fit(icon.filled(), icon.view), n, true),
            Target::CheckMark => {
                let view = crate::geom::Rect::new(0.0, 0.0, 24.0, 24.0);
                sample_path(&fit(crate::path::icons::check(), view), n, false)
            }
        }
    }
}

/// A mask for the PA08 force from time `at` on.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedMask {
    pub at: f64,
    pub mask: Arc<MaskField>,
}

/// `n` points of `path`: along its contours by arc length, and, when `fill` and it has closed
/// contours, 60 % of them inside on a jittered grid (nonzero winding), row by row.
pub fn sample_path(path: &kurbo::BezPath, n: usize, fill: bool) -> Vec<Point> {
    if n == 0 {
        return Vec::new();
    }
    let (lines, closed) = polylines(path);
    let total: f64 = lines.iter().flat_map(|l| l.windows(2)).map(|w| w[0].distance(w[1])).sum();
    let inside = if fill && closed { interior(&lines, (n as f64 * 0.6).round() as usize) } else { Vec::new() };
    let n_c = if total > 0.0 { n - inside.len() } else { 0 };
    let mut out = Vec::with_capacity(n);
    if n_c > 0 {
        let step = total / n_c as f64;
        let mut next = 0.5 * step;
        let mut run = 0.0;
        for w in lines.iter().flat_map(|l| l.windows(2)) {
            let d = w[0].distance(w[1]);
            while next < run + d && out.len() < n_c {
                let p = w[0].lerp(w[1], (next - run) / d);
                out.push(Point::new(p.x as f32, p.y as f32));
                next += step;
            }
            run += d;
        }
        // Rounding at the very end of the length.
        let end = lines.last().and_then(|l| l.last()).copied().unwrap_or_default();
        out.resize(n_c, Point::new(end.x as f32, end.y as f32));
    }
    out.extend(inside);
    out
}

/// `path` flattened to polylines, one per subpath, closed ones ending at their start; and
/// whether any subpath was closed.
fn polylines(path: &kurbo::BezPath) -> (Vec<Vec<kurbo::Point>>, bool) {
    use kurbo::PathEl;
    let mut lines: Vec<Vec<kurbo::Point>> = Vec::new();
    let mut closed = false;
    kurbo::flatten(path, 0.05, |el| match el {
        PathEl::MoveTo(p) => lines.push(vec![p]),
        PathEl::LineTo(p) => {
            if let Some(l) = lines.last_mut() {
                l.push(p);
            }
        }
        PathEl::ClosePath => {
            if let Some(l) = lines.last_mut() {
                if let Some(&first) = l.first() {
                    l.push(first);
                }
                closed = true;
            }
        }
        _ => {}
    });
    (lines, closed)
}

/// The x ranges where the horizontal line at `y` is inside `lines` by the nonzero rule, each
/// polyline taken as closed (as a fill takes it), into `out` in order.
fn spans(lines: &[Vec<kurbo::Point>], y: f64, cross: &mut Vec<(f64, i32)>, out: &mut Vec<(f64, f64)>) {
    cross.clear();
    out.clear();
    for l in lines {
        let n = l.len();
        for i in 0..n {
            let (a, b) = (l[i], l[(i + 1) % n]);
            if (a.y <= y) != (b.y <= y) {
                let x = a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x);
                cross.push((x, if b.y > a.y { 1 } else { -1 }));
            }
        }
    }
    cross.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut wind = 0;
    for c in cross.iter() {
        let was = wind;
        wind += c.1;
        if was == 0 && wind != 0 {
            out.push((c.0, c.0));
        } else if was != 0 && wind == 0 {
            if let Some(s) = out.last_mut() {
                s.1 = c.0;
            }
        }
    }
}

/// Most work [`interior`] does for one shape, in polyline segments crossed by its scanlines
/// plus grid cells tried: a few milliseconds, whatever the shape.
const INTERIOR_WORK: usize = 4_000_000;

/// `n` points inside `lines` (nonzero winding) on a jittered grid, row by row; none when they
/// have no inside. Only cells under the shape's spans are tried, so a thin shape across a
/// large box costs no more than a compact one; a shape that would still take more than
/// [`INTERIOR_WORK`] gets fewer points (the rest go on its outline).
fn interior(lines: &[Vec<kurbo::Point>], n: usize) -> Vec<Point> {
    let (mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in lines.iter().flatten() {
        y0 = y0.min(p.y);
        y1 = y1.max(p.y);
    }
    if n == 0 || y1 <= y0 {
        return Vec::new();
    }
    let segs: usize = lines.iter().map(|l| l.len()).sum::<usize>().max(1);
    let (mut cross, mut row) = (Vec::new(), Vec::new());
    // The filled area, from 256 scanlines, sizes the grid.
    let dy = (y1 - y0) / 256.0;
    let mut area = 0.0;
    for k in 0..256 {
        spans(lines, y0 + (k as f64 + 0.5) * dy, &mut cross, &mut row);
        area += row.iter().map(|s| s.1 - s.0).sum::<f64>() * dy;
    }
    if area <= 0.0 {
        return Vec::new();
    }
    let mut work = 256 * segs;
    let mut best: Vec<Point> = Vec::new();
    // About 1.25 cells per point; finer if that falls short.
    let mut cell = (area / (1.25 * n as f64)).sqrt();
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for _ in 0..4 {
        let rows = ((y1 - y0) / cell).ceil() as usize;
        if work + rows * 4 * segs > INTERIOR_WORK {
            break;
        }
        let mut cand = Vec::new();
        for r in 0..rows as u32 {
            // Four scanlines per row; each cell takes one of them, and a jittered x.
            let mut subs: [Vec<(f64, f64)>; 4] = Default::default();
            for (k, sub) in subs.iter_mut().enumerate() {
                spans(lines, y0 + (r as f64 + (k as f64 + 0.5) / 4.0) * cell, &mut cross, sub);
            }
            work += 4 * segs;
            // The cells a point can fall in: those under the four scanlines' spans, merged.
            merged.clear();
            merged.extend(subs.iter().flatten().copied());
            merged.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut next = i64::MIN;
            for &(a, b) in merged.iter() {
                let c0 = ((a / cell).floor() as i64).max(next);
                let c1 = (b / cell).ceil() as i64;
                for c in c0..=c1 {
                    let k = (vo_core::hash::rand_f32(0xe626, c as u32, r, 1) * 4.0) as usize % 4;
                    let x = (c as f64 + vo_core::hash::rand_f32(0xe626, c as u32, r, 0) as f64) * cell;
                    if subs[k].iter().any(|s| s.0 <= x && x < s.1) {
                        let y = y0 + (r as f64 + (k as f64 + 0.5) / 4.0) * cell;
                        cand.push(Point::new(x as f32, y as f32));
                    }
                }
                work += (c1 - c0 + 1).max(0) as usize;
                next = next.max(c1 + 1);
            }
            if work > INTERIOR_WORK {
                break;
            }
        }
        if cand.len() >= n {
            return (0..n).map(|i| cand[((i as f64 + 0.5) * cand.len() as f64 / n as f64) as usize]).collect();
        }
        if cand.len() > best.len() {
            best = cand.clone();
        }
        if work > INTERIOR_WORK {
            break;
        }
        // Finer by what fell short, but at most halving the cell each time, so a shape the
        // grid misses does not make the next pass's rows explode.
        cell *= (0.95 * (cand.len().max(1) as f64 / n as f64).sqrt()).clamp(0.5, 0.9025);
    }
    best
}

/// A coverage field of text (or any shape) for the PA08 mask force, in emitter space.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskField {
    pub width: u32,
    pub height: u32,
    /// The field's box in emitter space: x, y, w, h.
    pub rect: [f32; 4],
    /// Coverage 0 to 255, rows top to bottom.
    pub data: Vec<u8>,
}

impl MaskField {
    /// A field over `rect` with cells of `cell` units, from points that fill shapes (as
    /// glyph points do), blurred by `blur` cells so the force reaches past the edges.
    pub fn from_points(points: &[[f32; 2]], rect: [f32; 4], cell: f32, blur: u32) -> MaskField {
        let (w, h) = mask_size(rect, cell);
        let mut acc = vec![0f32; (w * h) as usize];
        for p in points {
            let x = ((p[0] - rect[0]) / rect[2] * w as f32).floor();
            let y = ((p[1] - rect[1]) / rect[3] * h as f32).floor();
            if x >= 0.0 && y >= 0.0 && (x as u32) < w && (y as u32) < h {
                acc[(y as u32 * w + x as u32) as usize] = 1.0;
            }
        }
        MaskField::blurred(acc, w, h, rect, blur)
    }

    /// A field over `rect` with cells of `cell` units, covered where `path` is (nonzero
    /// winding at each cell's centre), with a halo blurred `blur` cells out. The covered
    /// cells stay fully covered, so the hug force (a negative `EmitterSim::mask`) holds
    /// particles at the shape's own edge, and its halo reaches about `2 * blur` cells out.
    pub fn from_path(path: &kurbo::BezPath, rect: [f32; 4], cell: f32, blur: u32) -> MaskField {
        let (w, h) = mask_size(rect, cell);
        let (lines, _) = polylines(path);
        let (cw, ch) = (rect[2] as f64 / w as f64, rect[3] as f64 / h as f64);
        let mut acc = vec![0f32; (w * h) as usize];
        let (mut cross, mut row) = (Vec::new(), Vec::new());
        for y in 0..h {
            spans(&lines, rect[1] as f64 + (y as f64 + 0.5) * ch, &mut cross, &mut row);
            for s in &row {
                // Cells whose centres are inside the span.
                let a = ((s.0 - rect[0] as f64) / cw - 0.5).ceil().max(0.0) as u32;
                let b = (((s.1 - rect[0] as f64) / cw - 0.5).ceil().max(0.0) as u32).min(w);
                for x in a..b {
                    acc[(y * w + x) as usize] = 1.0;
                }
            }
        }
        let mut m = MaskField::blurred(acc.clone(), w, h, rect, blur);
        for (d, a) in m.data.iter_mut().zip(&acc) {
            if *a > 0.0 {
                *d = 255;
            }
        }
        m
    }

    /// The coverage at `p` in emitter space, 0 to 1 (nearest cell; 0 outside the box).
    pub fn at(&self, p: Point) -> f32 {
        let x = ((p.x - self.rect[0]) / self.rect[2] * self.width as f32).floor();
        let y = ((p.y - self.rect[1]) / self.rect[3] * self.height as f32).floor();
        if x < 0.0 || y < 0.0 || x as u32 >= self.width || y as u32 >= self.height {
            return 0.0;
        }
        self.data[(y as u32 * self.width + x as u32) as usize] as f32 / 255.0
    }

    fn blurred(mut acc: Vec<f32>, w: u32, h: u32, rect: [f32; 4], blur: u32) -> MaskField {
        for _ in 0..2 {
            acc = box_blur(&acc, w, h, blur);
        }
        let data = acc.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
        MaskField { width: w, height: h, rect, data }
    }
}

fn mask_size(rect: [f32; 4], cell: f32) -> (u32, u32) {
    let cell = cell.max(1e-3);
    (((rect[2] / cell).ceil() as u32).clamp(1, 512), ((rect[3] / cell).ceil() as u32).clamp(1, 512))
}

fn box_blur(src: &[f32], w: u32, h: u32, r: u32) -> Vec<f32> {
    if r == 0 {
        return src.to_vec();
    }
    let (w, h, r) = (w as i32, h as i32, r as i32);
    let n = (2 * r + 1) as f32;
    let mut tmp = vec![0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let s: f32 = (-r..=r).map(|d| src[(y * w + (x + d).clamp(0, w - 1)) as usize]).sum();
            tmp[(y * w + x) as usize] = s / n;
        }
    }
    let mut out = vec![0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let s: f32 = (-r..=r).map(|d| tmp[((y + d).clamp(0, h - 1) * w + x) as usize]).sum();
            out[(y * w + x) as usize] = s / n;
        }
    }
    out
}

/// The outlines of shaped text as one path, the text box's top left at `origin`, glyphs at
/// `size` (placed as `WordOutline::new` places them).
pub fn text_path(
    fonts: &crate::font::FontRegistry,
    shaped: &crate::text::ShapedText,
    size: f32,
    origin: Point,
) -> kurbo::BezPath {
    let mut out = kurbo::BezPath::new();
    for g in &shaped.glyphs {
        let Some(face) = fonts.get(g.font) else { continue };
        let at = kurbo::Affine::translate((origin.x as f64 + g.pos.x as f64, origin.y as f64 + g.pos.y as f64));
        out.extend((at * crate::path::glyph_path(&face, g.glyph, size)).iter());
    }
    out
}

/// A mask of shaped text for the PA08 force (`EmitterSim::mask`), in emitter space with the
/// text box's top left at `origin`: cells of `cell` units over the text's ink plus `margin`
/// units round it, blurred by `blur` cells so the force reaches past the edges.
pub fn text_mask(
    fonts: &crate::font::FontRegistry,
    shaped: &crate::text::ShapedText,
    size: f32,
    origin: Point,
    cell: f32,
    margin: f32,
    blur: u32,
) -> MaskField {
    use kurbo::Shape as _;
    let path = text_path(fonts, shaped, size, origin);
    let b = path.bounding_box();
    let rect = if b.area() > 0.0 {
        [b.x0 as f32 - margin, b.y0 as f32 - margin, b.width() as f32 + 2.0 * margin, b.height() as f32 + 2.0 * margin]
    } else {
        [origin.x, origin.y, 1.0, 1.0]
    };
    MaskField::from_path(&path, rect, cell, blur)
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

    #[test]
    fn a_word_source_runs_its_arc_and_starts_words_where_they_were_said() {
        use std::f32::consts::PI;
        let s = WordSource {
            centre: Point::new(100.0, 50.0),
            radius: [80.0, 20.0],
            from: -PI,
            to: 0.0,
            period: 2.0,
            lag: 0.25,
            spread: 0.3,
        };
        let close = |a: Point, x: f32, y: f32| (a.x - x).abs() < 1e-3 && (a.y - y).abs() < 1e-3;
        // Left end at the start of a run, the top in the middle, then again from the left.
        assert!(close(s.at(0.0), 20.0, 50.0));
        assert!(close(s.at(1.0), 100.0, 30.0));
        assert!(close(s.at(2.0), 20.0, 50.0));
        // A word that came at 1.25 was said at 1.0.
        assert!(close(s.start(1.25), 100.0, 30.0));
    }

    fn seg_dist(p: Point, a: Point, b: Point) -> f32 {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let k = (((p.x - a.x) * dx + (p.y - a.y) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        (p.x - a.x - k * dx).hypot(p.y - a.y - k * dy)
    }

    #[test]
    fn a_check_mark_gives_points_along_its_line_spread_by_length() {
        let g = ShapeTarget::new(0.0, Target::CheckMark, 48.0);
        let pts = g.points(600);
        assert_eq!(pts.len(), 600);
        // The 24-unit grid fitted to 48 units round the origin.
        let m = |x: f32, y: f32| Point::new((x - 12.0) * 2.0, (y - 12.0) * 2.0);
        let (a, b, c) = (m(4.5, 12.5), m(9.5, 17.5), m(19.5, 6.5));
        for p in &pts {
            let d = seg_dist(*p, a, b).min(seg_dist(*p, b, c));
            assert!(d < 0.05, "{p:?} is {d} off the line");
        }
        // The long arm is about 2.1 times the short one, and so is its share of points.
        let short = pts.iter().filter(|p| seg_dist(**p, a, b) < 0.05 && seg_dist(**p, b, c) > 0.05).count();
        let ratio = (600 - short) as f32 / short as f32;
        assert!((ratio - 2.1).abs() < 0.15, "long over short arm: {ratio}");
        // The same target gives the same points.
        assert_eq!(pts, g.points(600));
    }

    #[test]
    fn a_closed_path_gives_outline_and_inside_points() {
        let square = kurbo::Rect::new(-20.0, -10.0, 20.0, 10.0);
        let path = kurbo::Shape::to_path(&square, 0.01);
        let pts = sample_path(&path, 500, true);
        assert_eq!(pts.len(), 500);
        let on_edge = |p: &Point| (p.x.abs() - 20.0).abs() < 1e-3 || (p.y.abs() - 10.0).abs() < 1e-3;
        let edge = pts.iter().filter(|p| on_edge(p)).count();
        let inside = pts.iter().filter(|p| p.x.abs() < 20.0 && p.y.abs() < 10.0 && !on_edge(p)).count();
        assert_eq!(edge + inside, 500, "every point is on the square or in it");
        assert!((190..=210).contains(&edge) && (290..=310).contains(&inside), "{edge} on the edge, {inside} inside");
        // Inside points cover the square: each quarter gets a fair share.
        let quarter = pts[edge..].iter().filter(|p| p.x < 0.0 && p.y < 0.0).count();
        assert!((55..=95).contains(&quarter), "{quarter} of {inside} in one quarter");
        // Not filled: only the outline.
        assert!(sample_path(&path, 100, false).iter().all(on_edge));
    }

    #[test]
    fn an_svg_icon_is_fitted_round_the_origin_and_filled() {
        let svg = r#"<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10" fill="none" stroke="black" stroke-width="2"/></svg>"#;
        let icon = Arc::new(crate::svg::parse(svg).unwrap());
        let g = ShapeTarget::new(0.0, Target::Svg(icon), 120.0);
        let pts = g.points(2000);
        assert_eq!(pts.len(), 2000);
        // A ring 10 of 24 units out, 2 wide: 45 to 55 units out at 120 units across.
        for p in &pts {
            let r = p.x.hypot(p.y);
            assert!((44.9..=55.1).contains(&r), "{p:?} is {r} from the centre");
        }
        let mean = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.x, a.1 + p.y));
        assert!(mean.0.abs() < 1000.0 && mean.1.abs() < 1000.0, "centred: {mean:?}");
        assert!(ShapeTarget::new(0.0, Target::None, 10.0).points(10).is_empty());
    }

    /// A closed strip `len` long and `width` wide from the origin at `angle` radians.
    fn strip(len: f64, width: f64, angle: f64) -> kurbo::BezPath {
        let (c, s) = (angle.cos(), angle.sin());
        let (dx, dy) = (len * c, len * s);
        let (nx, ny) = (-s * width, c * width);
        let mut p = kurbo::BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((dx, dy));
        p.line_to((dx + nx, dy + ny));
        p.line_to((nx, ny));
        p.close_path();
        p
    }

    /// The fastest of three runs of `f`, in milliseconds, and its result.
    fn timed<T>(f: impl Fn() -> T) -> (f64, T) {
        let mut best = f64::INFINITY;
        let mut out = f();
        for _ in 0..3 {
            let t0 = std::time::Instant::now();
            out = f();
            best = best.min(t0.elapsed().as_secs_f64() * 1e3);
        }
        (best, out)
    }

    #[test]
    fn a_long_thin_shape_samples_in_a_few_milliseconds() {
        // A 1000 by 2 strip on the diagonal, and a V of two of them: before RV-04 the V took
        // 7 ms, and 1.3 s at 0.01 wide, as the grid walked its whole box.
        let diagonal = strip(1000.0, 2.0, 45f64.to_radians());
        let (ms, pts) = timed(|| sample_path(&diagonal, MAX_SHAPE_POINTS, true));
        assert_eq!(pts.len(), MAX_SHAPE_POINTS);
        eprintln!("diagonal: {ms:.2} ms");
        assert!(ms < 5.0, "the diagonal strip took {ms:.2} ms");
        for w in [2.0, 0.01] {
            let mut v = strip(1000.0, w, 45f64.to_radians());
            let mut right = strip(1000.0, w, 135f64.to_radians());
            right.apply_affine(kurbo::Affine::translate((1000.0 * 2f64.sqrt(), 0.0)));
            v.extend(right);
            let (ms, pts) = timed(|| sample_path(&v, MAX_SHAPE_POINTS, true));
            assert_eq!(pts.len(), MAX_SHAPE_POINTS);
            // A hair-thin V takes a few passes of a finer grid.
            let limit = if w < 1.0 { 50.0 } else { 5.0 };
            eprintln!("V {w} wide: {ms:.2} ms");
            assert!(ms < limit, "the V {w} wide took {ms:.2} ms");
            let (lines, _) = polylines(&v);
            assert_eq!(interior(&lines, 1229).len(), 1229, "the V {w} wide is filled");
        }
        // However many segments, the work stays bounded: points come back, maybe fewer inside.
        let mut many = kurbo::BezPath::new();
        for k in 0..400 {
            many.extend(strip(1000.0, 0.01, (k as f64 * 0.45).to_radians()));
        }
        let (ms, pts) = timed(|| sample_path(&many, MAX_SHAPE_POINTS, true));
        assert_eq!(pts.len(), MAX_SHAPE_POINTS);
        eprintln!("400 strips: {ms:.2} ms");
        assert!(ms < 200.0, "400 thin strips took {ms:.2} ms");
    }

    #[test]
    fn a_text_mask_covers_the_ink_and_blurs_past_it() {
        use crate::text::TextLayout;
        let fonts = crate::font::FontRegistry::new();
        fonts.add_file(&crate::font::test_font_path()).unwrap();
        let layout = crate::text::ShapedLayout::new(fonts.clone());
        let shaped = layout.layout("I", &crate::element::TextStyle::new(0, 80.0, 90.0), None);
        let m = text_mask(&fonts, &shaped, 80.0, Point::new(-20.0, -45.0), 1.0, 6.0, 1);
        let path = text_path(&fonts, &shaped, 80.0, Point::new(-20.0, -45.0));
        let b = kurbo::Shape::bounding_box(&path);
        let mid = Point::new(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
        assert!(m.at(mid) > 0.95, "inside the stem: {}", m.at(mid));
        assert_eq!(m.at(Point::new(b.x1 as f32 + 5.0, mid.y)), 0.0);
        // Going right from the stem, the coverage falls through the middle values (the blur)
        // within a few units.
        let row: Vec<f32> = (0..40).map(|i| m.at(Point::new(mid.x + i as f32 * 0.5, mid.y))).collect();
        let first_soft = row.iter().position(|v| *v < 0.9).unwrap();
        let first_zero = row.iter().position(|v| *v == 0.0).unwrap();
        assert!(row[first_soft] > 0.05 && first_zero - first_soft <= 8, "{row:?}");
    }

    #[test]
    fn glyph_letters_skip_whitespace_and_stop_at_the_limit() {
        let g = GlyphChars::new(" vox type ");
        assert_eq!(*g.letters(), ['v', 'o', 'x', 't', 'y', 'p', 'e']);
        assert_eq!(g, GlyphChars::new("voxtype"), "one set, one name");
        assert_ne!(g, GlyphChars::new("voxtyp"));
        let long = GlyphChars::new("abcdefghijklmnopqrstuvwxyz");
        assert_eq!(long.letters().len(), GlyphChars::MAX);
        assert_eq!(long.letters().last(), Some(&'p'));
        assert_eq!(format!("{g:?}"), "GlyphChars(\"voxtype\")");
        assert_eq!(GlyphChars::new(" \t\n").pick(3), None);
        // A style made from the same text is the same style (pools are keyed by it).
        assert_eq!(SpriteStyle::glyphs(0, "ab", 10.0), SpriteStyle::glyphs(0, "a b", 10.0));
    }

    #[test]
    fn glyph_picks_follow_the_hash_and_use_every_letter() {
        let g = GlyphChars::new("voxtype");
        let letters = g.letters();
        let mut seen = [0u32; 7];
        for i in 0..700 {
            let c = g.pick(i).unwrap();
            assert_eq!(c, letters[(vo_core::hash::pcg(i) % 7) as usize]);
            seen[letters.iter().position(|&x| x == c).unwrap()] += 1;
        }
        // The seven letters are distinct; each gets a fair share of 700 particles.
        assert!(seen.iter().all(|&n| (60..=140).contains(&n)), "{seen:?}");
    }
}
