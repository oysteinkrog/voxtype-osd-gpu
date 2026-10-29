//! The renderer's particles: emitter elements and particle words from a display list
//! (`vo_scene::emitter`), drawn inside the layer tree so an offscreen layer with bloom can
//! wrap them.
//!
//! A frame goes through four steps, all called by the renderer:
//!
//! 1. [`ParticleLayer::advance`] before the layers are walked. It adds a pool for each new
//!    emitter element, starting at the tick of the element's birth, steps every pool to the
//!    frame time with each source asked at each tick's own time, and applies particle-word
//!    changes at the times the scene recorded for them. So the frames do not depend on when
//!    frames happen. A frame earlier than the last one starts everything again from 0 and
//!    replays up to it.
//! 2. While walking: [`ParticleLayer::place_emitter`] and [`ParticleLayer::place_words`] for
//!    each item, which say where it is drawn and return its pixel bounds, and
//!    [`ParticleLayer::glyph_alpha`] for each live glyph, its hand-over opacity.
//! 3. [`ParticleLayer::prepare`] once the surfaces have their places: one particle view per
//!    surface that draws particles.
//! 4. [`ParticleLayer::draw`] for each particle command in a surface's pass.
//!
//! Positions: a view maps units to its surface's pixels by the frame scale and the surface
//! origin. An item's origin is its layer-space position mapped to target pixels, divided by
//! the scale; a layer's rotation and scale go into the emitter's rotation. Particle words
//! follow the layer's position only.

use std::ops::Range;
use std::sync::Arc;

use vo_scene::display::{DisplayList, GlyphInstance, NO_WORD};
use vo_scene::emitter::{
    EmitterInstance, EmitterLook, EmitterSim, EmitterSource, Shape, ShapeTarget, WordParticles,
    MAX_SHAPE_POINTS,
};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Affine, Point};

use crate::particle_text::{word_runs, ParticleText};
use crate::particles::{
    Camera, EmitterId, EmitterSpec, FrameParams, Part, ParticleConfig, Particles, PointForm, SlotLook, MAX_EMITTERS,
    VIEWS,
};

/// Word slots: live words that can form at once.
const WORD_SLOTS: u32 = 16;
/// Shape slots after the word slots: emitters whose particles form shapes (EG-26).
const SHAPE_SLOTS: u32 = 4;
/// Particles a word may use, as before shapes came; shapes use the whole slot.
const WORD_CAPACITY: u32 = 1024;
/// Free particles for all emitter elements together.
const FREE_CAPACITY: u32 = 131_072;
const TRAIL_CAPACITY: u32 = 1024;
/// The eye's distance in front of the target for 3D emitters, in target sizes (the larger
/// side): about a 50 degree field of view.
const EYE_DISTANCE: f32 = 1.1;

struct Pool {
    node: u64,
    generation: u32,
    id: EmitterId,
    source: Arc<dyn EmitterSource>,
    /// Not in the last frame: spawns nothing more.
    gone: bool,
    /// The path last uploaded for a path emitter.
    path: Option<Arc<[Point]>>,
    /// Shapes (EG-26): the slot its particles form in, the time of the last target applied,
    /// and how far its points reach from the origin.
    slot: Option<u32>,
    target_at: f64,
    shape_reach: f32,
    /// The time of the last mask applied.
    mask_at: f64,
    /// Its pool size: the points a shape gets, up to a slot.
    capacity: u32,
    /// Reduced motion: shapes appear already formed.
    reduced: bool,
}

impl Pool {
    fn new(node: u64, generation: u32, id: EmitterId, source: Arc<dyn EmitterSource>) -> Pool {
        Pool {
            node,
            generation,
            id,
            source,
            gone: false,
            path: None,
            slot: None,
            target_at: f64::NEG_INFINITY,
            shape_reach: 0.0,
            mask_at: f64::NEG_INFINITY,
            capacity: 0,
            reduced: false,
        }
    }
}

struct Placement {
    part: Part,
    surface: usize,
    /// In units: target pixels over the frame scale.
    origin: Point,
    /// Emitters: the look at the frame time, with the layer's rotation and scale in it.
    look: Option<EmitterLook>,
    /// Slots: the word's look and opacity.
    word: Option<(WordParticles, f32)>,
    /// Shape slots: the look, its origin set in `prepare`.
    shape: Option<SlotLook>,
}

/// Particle state that lives from frame to frame.
pub struct ParticleLayer {
    fonts: FontRegistry,
    parts: Option<Particles>,
    pools: Vec<Pool>,
    text: ParticleText,
    /// Each live particle word and the change time already applied to it.
    words: Vec<(u32, f64, WordParticles)>,
    /// The newest word, while `WordParticles::hold` keeps its last glyphs as particles.
    held: Option<u32>,
    /// Where each slot was last drawn, in units, for the scatter of a word that went.
    slot_origins: Vec<Point>,
    t: f64,
    scale: f32,
    placements: Vec<Placement>,
    /// Glyphs of each word seen so far this frame, for the glyph's index in its word.
    glyph_index: Vec<(u32, usize)>,
    /// The particle view of each surface that draws particles.
    views: Vec<(usize, u32)>,
    warned: bool,
    /// Which pool (node, generation) holds each shape slot.
    shape_slots: [Option<(u64, u32)>; SHAPE_SLOTS as usize],
}

impl ParticleLayer {
    pub fn new(fonts: FontRegistry) -> ParticleLayer {
        ParticleLayer {
            fonts,
            parts: None,
            pools: Vec::new(),
            text: ParticleText::new(WORD_SLOTS, 1.0, vo_scene::geom::Color::WHITE),
            words: Vec::new(),
            held: None,
            slot_origins: vec![Point::ZERO; WORD_SLOTS as usize],
            t: f64::NEG_INFINITY,
            scale: 1.0,
            placements: Vec::new(),
            glyph_index: Vec::new(),
            views: Vec::new(),
            warned: false,
            shape_slots: [None; SHAPE_SLOTS as usize],
        }
    }

    /// Whether anything is drawn this frame.
    pub fn active(&self) -> bool {
        !self.placements.is_empty()
    }

    /// Steps everything to the frame time. Submits its own compute work. The time comes
    /// with the particle items; a frame without any draws no particles.
    pub fn advance(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, list: &DisplayList) {
        self.placements.clear();
        self.glyph_index.clear();
        self.views.clear();
        self.scale = list.scale.max(1e-6);
        let (words, t) = match (&list.particle_words, list.emitters.first()) {
            (Some(w), _) => (&w.words[..], w.t),
            (None, Some(e)) => (&[][..], e.t),
            (None, None) => return,
        };
        if t < self.t {
            // A seek back: start again from 0 and replay to `t`.
            self.parts = None;
            self.pools.clear();
            self.text.clear();
            self.words.clear();
            self.held = None;
            self.shape_slots = [None; SHAPE_SLOTS as usize];
        }
        self.t = t;
        let parts = self.parts.get_or_insert_with(|| {
            let cfg = ParticleConfig {
                free_capacity: FREE_CAPACITY,
                word_slots: WORD_SLOTS + SHAPE_SLOTS,
                slot_capacity: MAX_SHAPE_POINTS as u32,
                word_capacity: WORD_CAPACITY,
                trail_capacity: TRAIL_CAPACITY,
                ..Default::default()
            };
            Particles::new(device, queue, self.fonts.clone(), cfg)
        });

        // Pools: one per element and generation, added in birth order so a replay adds them
        // in the same order.
        for p in &mut self.pools {
            p.gone = true;
        }
        let mut new: Vec<&EmitterInstance> = Vec::new();
        for e in &list.emitters {
            match self.pools.iter_mut().find(|p| p.node == e.node && p.generation == e.generation) {
                Some(p) => {
                    p.gone = false;
                    p.reduced = e.reduced;
                    p.source = list.emitter_sources[e.source as usize].0.clone();
                }
                None => new.push(e),
            }
        }
        new.sort_by(|a, b| a.since.total_cmp(&b.since).then(a.node.cmp(&b.node)));
        for e in new {
            let used: u32 = self.pools.len() as u32;
            if used >= MAX_EMITTERS || parts.free_count() + e.capacity.next_multiple_of(64) > FREE_CAPACITY {
                if !self.warned {
                    eprintln!("vo-render: no room for another particle emitter; it is not drawn");
                    self.warned = true;
                }
                continue;
            }
            let spec = EmitterSpec { trails: e.trails, three_d: e.three_d, ..EmitterSpec::new(e.capacity, e.seed) };
            let start = parts.ticks_for(e.since) - 1;
            let id = parts.add_emitter_at(spec, start);
            let source = list.emitter_sources[e.source as usize].0.clone();
            let mut pool = Pool::new(e.node, e.generation, id, source);
            pool.capacity = e.capacity;
            pool.reduced = e.reduced;
            self.pools.push(pool);
        }
        let mut masks: Vec<(f64, Arc<vo_scene::emitter::MaskField>)> = Vec::new();
        for p in &mut self.pools {
            if let Some(path) = p.source.path() {
                if !p.path.as_ref().is_some_and(|old| Arc::ptr_eq(old, &path)) {
                    parts.set_path(p.id, &path);
                    p.path = Some(path);
                }
            }
            // Shapes and masks (EG-26) at their recorded times; a seek back replays them.
            if let Some(list) = p.source.masks() {
                let since = p.mask_at;
                for m in list.iter().filter(|m| m.at > since && m.at <= t) {
                    masks.push((m.at, m.mask.clone()));
                    p.mask_at = m.at;
                }
            }
            if let Some(list) = p.source.targets() {
                if list.iter().any(|g| g.at > p.target_at && g.at <= t) {
                    apply_targets(parts, p, &list, t, &mut self.shape_slots, &mut self.warned);
                }
            }
        }
        // Each mask switches on the tick its time falls in: step up to it first, so ticks
        // before it keep the mask before.
        masks.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (at, mask) in masks {
            let pools = &self.pools;
            let switch = (at * parts.config().tick_hz).ceil().max(0.0);
            let before = (switch - 1.0) / parts.config().tick_hz;
            if parts.ticks_for(before) > parts.tick() && before <= t {
                parts.advance(before, &|id, tt| pool_sim(pools, id, tt));
            }
            parts.set_mask(at, &mask);
        }
        let pools = &self.pools;
        parts.advance(t, &|id, tt| pool_sim(pools, id, tt));

        // Particle words, at the times their text changed.
        let mut changes: Vec<_> = words
            .iter()
            .filter(|w| !self.words.iter().any(|(id, since, _)| *id == w.word && *since == w.since))
            .collect();
        changes.sort_by(|a, b| a.since.total_cmp(&b.since));
        let gone: Vec<u32> = self.words.iter().map(|w| w.0).filter(|id| !words.iter().any(|w| w.word == *id)).collect();
        for id in gone {
            self.text.remove(parts, id, t);
            self.words.retain(|w| w.0 != id);
        }
        // The newest word holds its last glyphs as particles while its look asks for it. A held
        // word lets go when another word becomes the newest or the hold goes to 0, at the
        // latest change time, so the release does not depend on when frames happen.
        let newest = words.last().filter(|w| w.look.hold > 0.0);
        let release = self.held.filter(|h| newest.map(|w| w.word) != Some(*h));
        if changes.is_empty() && release.is_none() {
            return;
        }
        let runs = word_runs(&list.glyphs);
        if let Some(h) = release {
            self.held = None;
            let at = words.iter().map(|w| w.since).fold(f64::NEG_INFINITY, f64::max).min(t);
            if let (Some(run), Some(w)) = (runs.iter().find(|r| r.word == h), self.words.iter().find(|w| w.0 == h)) {
                self.text.px_per_unit = w.2.density * self.scale;
                self.text.dot_area = w.2.dot_area;
                self.text.set_held(parts, at, run, run.glyphs.len());
            }
        }
        for w in changes {
            let Some(run) = runs.iter().find(|r| r.word == w.word) else { continue };
            self.text.px_per_unit = w.look.density * self.scale;
            self.text.dot_area = w.look.dot_area;
            self.text.from = w.look.from.map(|s| {
                let q = s.start(w.since);
                (Point::new(q.x - run.origin.x, q.y - run.origin.y), s.spread)
            });
            let n = run.glyphs.len();
            let is_newest = newest.is_some_and(|x| x.word == w.word);
            let held_from = if is_newest { n - ((w.look.hold.min(1.0) * n as f32).round() as usize).min(n) } else { n };
            if self.text.set_held(parts, w.since.min(t), run, held_from) {
                self.words.retain(|x| x.0 != w.word);
                self.words.push((w.word, w.since, w.look));
                if is_newest {
                    self.held = Some(w.word);
                }
            }
        }
    }

    /// The hand-over opacity of a live glyph: 1 for glyphs of words without particles. Call
    /// once per glyph, in display-list order.
    pub fn glyph_alpha(&mut self, g: &GlyphInstance) -> f32 {
        if g.word == NO_WORD {
            return 1.0;
        }
        let (Some(parts), Some(slot)) = (&self.parts, self.text.slot_of(g.word)) else { return 1.0 };
        let index = match self.glyph_index.iter_mut().find(|(w, _)| *w == g.word) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                self.glyph_index.push((g.word, 0));
                0
            }
        };
        parts.glyph_alpha(slot, index, self.t)
    }

    /// Places emitter instance `i` of `list`, drawn in `surface` through `to_px` (layer space
    /// to target pixels) at `opacity`. Returns the placement range and its pixel bounds.
    pub fn place_emitter(
        &mut self,
        list: &DisplayList,
        i: usize,
        to_px: &Affine,
        opacity: f32,
        surface: usize,
    ) -> Option<(Range<u32>, [f32; 4])> {
        let e = list.emitters.get(i)?;
        let pool = self.pools.iter().find(|p| p.node == e.node && p.generation == e.generation)?;
        let s = self.scale;
        let mut look = pool.source.look(self.t);
        let centre = to_px.apply(Point::new(e.pos.x + look.origin.x, e.pos.y + look.origin.y));
        // The layer's rotation and scale, without the frame scale, applied after the look's.
        let l = [[to_px.a / s, to_px.c / s, 0.0], [to_px.b / s, to_px.d / s, 0.0], [0.0, 0.0, 1.0]];
        let r = look.rotation;
        let mut m = [[0.0f32; 3]; 3];
        for (row, out) in m.iter_mut().enumerate() {
            for (col, v) in out.iter_mut().enumerate() {
                *v = (0..3).map(|k| l[row][k] * r[k][col]).sum();
            }
        }
        look.rotation = m;
        look.brightness *= opacity * e.opacity;
        let k = ((to_px.a * to_px.d - to_px.b * to_px.c).abs().sqrt() / s).max(1e-6);
        look.size *= k;
        let path = pool.path.as_ref().map_or(0.0, |p| p.iter().map(|q| q.x.hypot(q.y)).fold(0.0, f32::max));
        let reach = ((extent(&pool.source.sim(self.t)) + path) * k + 4.0 * look.size) * s;
        // A shape's particles (EG-26) are drawn round the same origin, without the layer's
        // rotation and scale.
        let reach = reach.max((pool.shape_reach + 4.0 * look.size) * s);
        let origin = Point::new(centre.x / s, centre.y / s);
        let start = self.placements.len() as u32;
        self.placements.push(Placement { part: Part::Emitter(pool.id), surface, origin, look: Some(look), word: None, shape: None });
        if let Some(slot) = pool.slot {
            let shape = SlotLook { size: look.size, style: look.style, ..SlotLook::new(origin, look.ramp[0].fade(look.brightness)) };
            self.placements.push(Placement { part: Part::Slot(slot), surface, origin, look: None, word: None, shape: Some(shape) });
        }
        let end = self.placements.len() as u32;
        Some((start..end, [centre.x - reach, centre.y - reach, centre.x + reach, centre.y + reach]))
    }

    /// Places the formed particles of the particle words among `glyphs` (one glyph item run),
    /// drawn in `surface` through `to_px` at `opacity`.
    pub fn place_words(
        &mut self,
        glyphs: &[GlyphInstance],
        to_px: &Affine,
        opacity: f32,
        surface: usize,
    ) -> Option<(Range<u32>, [f32; 4])> {
        if self.words.is_empty() {
            return None;
        }
        let s = self.scale;
        let start = self.placements.len() as u32;
        let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        let mut seen: Vec<u32> = Vec::new();
        for g in glyphs.iter().filter(|g| g.word != NO_WORD) {
            let p = to_px.apply(g.pos);
            // Scattered particles fly about two sizes out.
            let r = 2.5 * g.size * s;
            b = [b[0].min(p.x - r), b[1].min(p.y - r), b[2].max(p.x + 2.0 * r), b[3].max(p.y + r)];
            if seen.contains(&g.word) {
                continue;
            }
            seen.push(g.word);
            let (Some(slot), Some(&(_, _, look))) =
                (self.text.slot_of(g.word), self.words.iter().find(|w| w.0 == g.word))
            else {
                continue;
            };
            let origin = Point::new(p.x / s, p.y / s);
            self.slot_origins[slot as usize] = origin;
            self.placements.push(Placement {
                part: Part::Slot(slot),
                surface,
                origin,
                look: None,
                word: Some((look, opacity)),
                shape: None,
            });
        }
        let end = self.placements.len() as u32;
        (end > start).then_some((start..end, b))
    }

    /// Places the scatter of words that went, at their last place, in `surface`.
    pub fn place_fading(&mut self, surface: usize) -> Option<(Range<u32>, [f32; 4])> {
        let start = self.placements.len() as u32;
        let look = self.words.first().map_or_else(WordParticles::default, |w| w.2);
        let s = self.scale;
        let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        let fading: Vec<(u32, Point)> = self.text.fading(self.t).collect();
        for (slot, _) in fading {
            let origin = self.slot_origins[slot as usize];
            let r = 200.0 * s;
            b = [
                b[0].min(origin.x * s - r),
                b[1].min(origin.y * s - r),
                b[2].max(origin.x * s + r),
                b[3].max(origin.y * s + r),
            ];
            self.placements.push(Placement {
                part: Part::Slot(slot),
                surface,
                origin,
                look: None,
                word: Some((look, 1.0)),
                shape: None,
            });
        }
        let end = self.placements.len() as u32;
        (end > start).then_some((start..end, b))
    }

    /// Writes the frame's particle views. `surface(i)` gives surface `i`'s origin in target
    /// pixels and its size; surface 0 is the target, `width` by `height`.
    pub fn prepare(
        &mut self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        surface: &dyn Fn(usize) -> ([i32; 2], [u32; 2]),
    ) {
        let Some(parts) = &mut self.parts else { return };
        if self.placements.is_empty() {
            return;
        }
        let s = self.scale;
        let mut looks: Vec<Option<EmitterLook>> = vec![None; MAX_EMITTERS as usize];
        let mut slots = vec![
            SlotLook { visible: false, ..SlotLook::new(Point::ZERO, Default::default()) };
            (WORD_SLOTS + SHAPE_SLOTS) as usize
        ];
        for p in &self.placements {
            if let (Part::Slot(slot), Some(shape)) = (p.part, p.shape) {
                slots[slot as usize] = SlotLook { origin: p.origin, ..shape };
                continue;
            }
            match (p.part, p.look, p.word) {
                (Part::Emitter(id), Some(look), _) => {
                    looks[id as usize] = Some(EmitterLook { origin: p.origin, ..look });
                }
                (Part::Slot(slot), _, Some((look, opacity))) => {
                    slots[slot as usize] = SlotLook {
                        size: look.size,
                        style: look.style,
                        ..SlotLook::new(p.origin, look.color.fade(opacity))
                    };
                }
                _ => {}
            }
        }
        let hidden = EmitterLook { brightness: 0.0, ..EmitterLook::default() };
        // 3D emitters (D302) are seen in perspective from in front of the target's centre.
        let target = [width as f32, height as f32];
        let distance = EYE_DISTANCE * target[0].max(target[1]) / s;
        let mut params = FrameParams::new(self.t, Affine::scale(s), width, height, format);
        params.camera = Some(Camera::overlay_view(target, [0.0, 0.0], target, s, distance));
        parts.prepare(&params, &|id| looks.get(id as usize).copied().flatten().unwrap_or(hidden), &slots);
        self.views.push((0, 0));
        for p in &self.placements {
            if self.views.iter().any(|v| v.0 == p.surface) {
                continue;
            }
            let view = self.views.len() as u32;
            if view >= VIEWS {
                continue;
            }
            let ([ox, oy], [w, h]) = surface(p.surface);
            let to_px = Affine { a: s, b: 0.0, c: 0.0, d: s, e: -ox as f32, f: -oy as f32 };
            let origin = [ox as f32, oy as f32];
            let camera = Camera::overlay_view(target, origin, [w as f32, h as f32], s, distance);
            parts.set_view(view, to_px, w, h, format, Some(camera));
            self.views.push((p.surface, view));
        }
    }

    /// Draws placements `range` into `pass`.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, range: Range<u32>) {
        let Some(parts) = &self.parts else { return };
        for p in &self.placements[range.start as usize..range.end as usize] {
            if let Some(&(_, view)) = self.views.iter().find(|v| v.0 == p.surface) {
                parts.draw_part(pass, view, p.part);
            }
        }
    }
}

/// How far an emitter's particles reach from its origin, in units: its shape plus a second
/// of flight.
fn extent(s: &EmitterSim) -> f32 {
    let shape = match s.shape {
        Shape::Point => 0.0,
        Shape::Ring { radius, spread } => radius * (1.0 + spread),
        Shape::Disc { radius } => radius,
        Shape::Line { half_length, .. } => half_length,
        Shape::Box { half } => half[0].max(half[1]).max(half[2]),
        Shape::Torus { major, minor } => major + minor,
        Shape::Path { sway, width, .. } => sway + width,
    };
    let flight = (s.spawn_speed + s.noise * 0.2) * s.life.max(0.1) + s.gravity[0].abs().max(s.gravity[1].abs()) * 0.5;
    let shock = s.shock.map_or(0.0, |k| (k.speed * 0.3).min(4.0 * shape.max(10.0)));
    shape + flight + shock + 8.0
}

/// An emitter's inputs at tick time `t`: a pool that left the scene spawns nothing more.
fn pool_sim(pools: &[Pool], id: EmitterId, t: f64) -> EmitterSim {
    match pools.iter().find(|p| p.id == id) {
        Some(p) if !p.gone => p.source.sim(t),
        Some(p) => EmitterSim { emit: 0.0, ..p.source.sim(t) },
        None => EmitterSim { emit: 0.0, ..Default::default() },
    }
}

/// Forms the targets of `list` that apply after the pool's last one and by `t`, in order, each
/// at its own time, in the pool's shape slot (taken on its first target).
fn apply_targets(
    parts: &mut Particles,
    p: &mut Pool,
    list: &[ShapeTarget],
    t: f64,
    slots: &mut [Option<(u64, u32)>; SHAPE_SLOTS as usize],
    warned: &mut bool,
) {
    let slot = match p.slot {
        Some(s) => s,
        None => {
            let Some(i) = slots.iter().position(|s| s.is_none()) else {
                if !*warned {
                    eprintln!("vo-render: no room for another particle shape; it is not drawn");
                    *warned = true;
                }
                return;
            };
            slots[i] = Some((p.node, p.generation));
            p.slot = Some(WORD_SLOTS + i as u32);
            WORD_SLOTS + i as u32
        }
    };
    let n = (p.capacity as usize).min(MAX_SHAPE_POINTS);
    let since = p.target_at;
    for g in list.iter().filter(|g| g.at > since && g.at <= t) {
        let points = g.points(n);
        let form = PointForm {
            points: &points,
            // Reduced motion: each particle starts on its target, so the shape is there at once.
            starts: p.reduced.then_some(&points[..]),
            size: g.size,
            persist: true,
            sweep: if p.reduced { 0.0 } else { g.sweep },
            scatter: g.scatter && !p.reduced,
        };
        parts.set_points(slot, g.at, &form);
        p.target_at = g.at;
        p.shape_reach = points.iter().map(|q| q.x.hypot(q.y)).fold(0.0, f32::max) + g.size;
    }
}
