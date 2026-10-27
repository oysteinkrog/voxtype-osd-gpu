//! Typographic sound sculpture (MO-88): the spoken sentence as a ribbon of extruded letters
//! whose depth follows loudness over time, seen slightly from the side, flattening into
//! normal text when the dictation is finished.
//!
//! The sculpture lies under flat text and never replaces it. Each letter's front face sits
//! at z = 0 on the stage camera (`crate::OnStage`), exactly where the 2D layout draws the
//! glyph, and its body runs back from there, sheared up and across so the walls show from
//! slightly below and to the side. The caller draws the same text flat on top, so the
//! reading line keeps its envelope (rule 1) while the sound stands out behind it.
//!
//! Depth is a spring per letter: a letter born loud grows deep, a quiet one stays shallow,
//! and [`Sculpture::flatten`] lays every letter down from left to right. Glyph meshes are
//! built once for a character set and keyed by glyph id; a letter outside the set has no
//! body and shows as flat text alone.
//!
//! Keep one [`Sculpture`] per popup in an `Arc` and hand [`Sculpture::source`] to
//! `Element::gpu`: the painter uploads the meshes when it first meets the source, and the
//! letters change through [`Sculpture::set`] without a new upload. Frames are pure in `t`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use skrifa::MetadataProvider;
use vo_core::channel::{Channel, Rest};
use vo_core::spring::Spring;
use vo_scene::font::{Face, FontId};
use vo_scene::geom::{Color, Point, Size};
use vo_scene::gpu::GpuSource;

use crate::frame::{Frame3d, Light, Material, MeshDraw, MeshId, OnStage};
use crate::item::Source3d;
use crate::math::{Mat4, Vec3};
use crate::mesh::Mesh;
use crate::text3d::extrude_glyph;

/// The characters that get a body by default: printable ASCII and the Latin-1 letters.
pub const DEFAULT_CHARSET: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\
    .,;:!?'\"-()&/%$@#*+=<>[]{}_~^|\\`\
    æøåäöüéèêëàáâãçíìîïñóòôõúùûýÿßÆØÅÄÖÜÉÈÊËÀÁÂÃÇÍÌÎÏÑÓÒÔÕÚÙÛÝ";

/// How the sculpture looks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SculptLook {
    /// The body's colour; the walls are lit from above.
    pub color: Color,
    /// Added on top of the lit colour, as a share of `color`.
    pub emissive: f32,
    /// How far the back face moves across and up per pixel of depth (x right, y up), so
    /// the walls show from slightly to the side and below.
    pub shear: (f32, f32),
    /// The light and the springs.
    pub light: Light,
    /// Seconds a letter takes to reach a new depth.
    pub grow_secs: f64,
    /// Seconds from the first to the last letter laying down in a flatten.
    pub flatten_spread: f64,
}

impl Default for SculptLook {
    fn default() -> SculptLook {
        SculptLook {
            color: Color::hex(0x7d8696, 1.0),
            emissive: 0.0,
            shear: (0.3, 0.45),
            light: Light {
                dir: Vec3::new(0.35, -0.9, -0.45),
                color: Color::WHITE,
                intensity: 1.0,
                ambient: Color::rgba(0.2, 0.22, 0.27, 1.0),
                rim: 0.35,
            },
            grow_secs: 0.32,
            flatten_spread: 0.45,
        }
    }
}

/// One letter to sculpt, placed as the 2D layout places it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Letter {
    pub glyph: u32,
    pub font: FontId,
    /// Pen position on the baseline in the item's box, logical pixels, y down.
    pub pos: Point,
    /// Font size in logical pixels.
    pub size: f32,
    /// A key that stays the same for the same letter across updates, such as the word id
    /// and the letter's index in the word.
    pub key: u64,
    /// The depth this letter should reach, in logical pixels.
    pub depth: f32,
    /// The run of text the letter moves with, such as its line. A new letter in a run that
    /// is still moving starts as far from its place as the run's other letters, so it
    /// arrives with them.
    pub run: u64,
}

struct Body {
    letter: Letter,
    depth: Channel,
    /// Where the pen is, so a letter follows its text when a line moves.
    x: Channel,
    y: Channel,
}

#[derive(Default)]
struct State {
    bodies: Vec<Body>,
    until: f64,
}

/// The sculpture's meshes and its letters.
pub struct Sculpture {
    font: FontId,
    face: Arc<Face>,
    /// Glyph id to mesh index.
    index: HashMap<u32, u32>,
    glyphs: Vec<u32>,
    pub look: SculptLook,
    state: Mutex<State>,
}

impl std::fmt::Debug for Sculpture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sculpture").field("font", &self.font).field("glyphs", &self.glyphs.len()).finish()
    }
}

impl Sculpture {
    /// A sculpture for text in `font` (`face`), with a body for each glyph of `charset`.
    pub fn new(face: Arc<Face>, font: FontId, charset: &str, look: SculptLook) -> Sculpture {
        let map = face.skrifa().charmap();
        let mut glyphs: Vec<u32> = charset.chars().filter_map(|c| map.map(c)).map(|g| g.to_u32()).collect();
        glyphs.sort_unstable();
        glyphs.dedup();
        let index = glyphs.iter().enumerate().map(|(i, &g)| (g, i as u32)).collect();
        Sculpture { font, face, index, glyphs, look, state: Mutex::new(State::default()) }
    }

    /// The source for `Element::gpu`. Keep the `Arc` across views.
    pub fn source(self: &Arc<Self>) -> Arc<dyn GpuSource> {
        crate::painter::item(Shared(self.clone()))
    }

    /// Whether a glyph of this font has a body.
    pub fn has(&self, font: FontId, glyph: u32) -> bool {
        font == self.font && self.index.contains_key(&glyph)
    }

    /// The letters from `t`. A letter seen before (by key) springs from its depth now to
    /// its new depth; a new letter grows from flat. Letters not in the list are dropped.
    /// A letter whose place changed jumps there, or moves on `moves` when given: pass the
    /// spring its text moves on, so the body stays under the glyph. Calls must come in
    /// time order.
    pub fn set(&self, t: f64, letters: &[Letter], moves: Option<Spring>) {
        let mut s = self.state.lock().unwrap();
        let mut old: HashMap<u64, Body> = s.bodies.drain(..).map(|b| (b.letter.key, b)).collect();
        let spring = Spring::critical_to_98(self.look.grow_secs);
        let channel =
            |v: f32, spring: Spring| Channel::new(t, v as f64, spring).with_rest(Rest::PIXELS).with_retention(4.0);
        let mut until = s.until;
        // How far each run still has to go, from a letter of it seen before.
        let mut lag: HashMap<u64, (f64, f64)> = HashMap::new();
        for l in letters {
            if let Some(b) = old.get(&l.key) {
                let off = (b.x.value(t) - l.pos.x as f64, b.y.value(t) - l.pos.y as f64);
                lag.entry(l.run).or_insert(off);
            }
        }
        for l in letters {
            let mut body = old.remove(&l.key).unwrap_or_else(|| {
                let m = moves.unwrap_or(spring);
                let (dx, dy) = lag.get(&l.run).copied().filter(|_| moves.is_some()).unwrap_or((0.0, 0.0));
                let mut x = channel(l.pos.x, m);
                let mut y = channel(l.pos.y, m);
                // Start where the run is now and move in with it.
                if dx != 0.0 || dy != 0.0 {
                    x.snap(t, l.pos.x as f64 + dx);
                    x.retarget(t, l.pos.x as f64);
                    y.snap(t, l.pos.y as f64 + dy);
                    y.retarget(t, l.pos.y as f64);
                }
                Body { letter: *l, depth: channel(0.0, spring), x, y }
            });
            if (body.depth.target() - l.depth as f64).abs() > 1e-3 {
                body.depth.retarget(t, l.depth as f64);
            }
            for (c, v) in [(&mut body.x, l.pos.x), (&mut body.y, l.pos.y)] {
                if (c.target() - v as f64).abs() > 1e-3 {
                    match moves {
                        Some(m) => {
                            c.set_spring(m);
                            c.retarget(t, v as f64);
                        }
                        None => c.snap(t, v as f64),
                    }
                }
            }
            body.letter = *l;
            until = until.max(body.depth.settled_at()).max(body.x.settled_at()).max(body.y.settled_at());
            s.bodies.push(body);
        }
        s.until = until;
    }

    /// Lays every letter flat from `t`, left to right across `flatten_spread` seconds.
    pub fn flatten(&self, t: f64) {
        let mut s = self.state.lock().unwrap();
        let (lo, hi) = s
            .bodies
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), b| (lo.min(b.letter.pos.x), hi.max(b.letter.pos.x)));
        let span = (hi - lo).max(1.0);
        let mut until = s.until;
        for b in &mut s.bodies {
            b.letter.depth = 0.0;
            if b.depth.target() != 0.0 {
                let delay = self.look.flatten_spread * ((b.letter.pos.x - lo) / span) as f64;
                b.depth.retarget(t + delay, 0.0);
            }
            until = until.max(b.depth.settled_at());
        }
        s.until = until;
    }

    /// The depth of each letter at `t`, in order, for tests and tools.
    pub fn depths(&self, t: f64) -> Vec<f32> {
        self.state.lock().unwrap().bodies.iter().map(|b| b.depth.value(t) as f32).collect()
    }

    fn frame_at(&self, t: f64, size: Size) -> Frame3d {
        let s = self.state.lock().unwrap();
        let (sx, sy) = self.look.shear;
        let material =
            Material { color: self.look.color, emissive: self.look.emissive, specular: 0.5, shininess: 40.0 };
        let meshes = s
            .bodies
            .iter()
            .filter_map(|b| {
                let l = &b.letter;
                let mesh = *self.index.get(&l.glyph).filter(|_| l.font == self.font)?;
                let d = b.depth.value(t).max(0.0) as f32;
                let (px, py) = (b.x.value(t) as f32, b.y.value(t) as f32);
                if d < 0.25 {
                    return None;
                }
                // Unit glyph (em, y up, z from -0.5 to 0.5) to a body `d` deep behind z = 0,
                // its back face moved across and up by the shear, at the glyph's pen.
                let body =
                    Mat4::translation(Vec3::new(0.0, 0.0, -0.5)).then(&Mat4::scale(Vec3::new(l.size, l.size, d)));
                let shear = Mat4 {
                    cols: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [-sx, -sy, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]],
                };
                let at = Mat4::translation(Vec3::new(px - 0.5 * size.w, 0.5 * size.h - py, 0.0));
                Some(MeshDraw { mesh: MeshId(mesh), model: body.then(&shear).then(&at), material })
            })
            .collect();
        Frame3d { meshes, light: self.look.light, on_stage: Some(OnStage { overscan: 0.25 }), ..Default::default() }
    }
}

#[derive(Debug)]
struct Shared(Arc<Sculpture>);

impl Source3d for Shared {
    fn meshes(&self) -> Vec<Mesh> {
        let s = &self.0;
        s.glyphs.iter().map(|&g| extrude_glyph(&s.face, g, 1.0).unwrap_or_default()).collect()
    }

    fn frame(&self, t: f64, size: Size) -> Frame3d {
        self.0.frame_at(t, size)
    }

    fn until(&self) -> f64 {
        self.0.state.lock().unwrap().until
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::font::{test_font_path, FontRegistry};

    fn sculpture() -> (Arc<Sculpture>, FontId, u32) {
        let fonts = FontRegistry::new();
        let id = fonts.add_file(&test_font_path()).unwrap();
        let face = fonts.get(id).unwrap();
        let a = face.skrifa().charmap().map('a').unwrap().to_u32();
        (Arc::new(Sculpture::new(face, id, DEFAULT_CHARSET, SculptLook::default())), id, a)
    }

    fn letter(font: FontId, glyph: u32, key: u64, depth: f32) -> Letter {
        Letter { glyph, font, pos: Point::new(10.0 * key as f32, 30.0), size: 24.0, key, depth, run: 0 }
    }

    #[test]
    fn a_letter_grows_to_its_depth_and_keeps_its_spring_across_updates() {
        let (s, font, a) = sculpture();
        assert!(s.has(font, a));
        s.set(0.0, &[letter(font, a, 0, 20.0)], None);
        assert_eq!(s.depths(0.0), vec![0.0]);
        assert!((s.depths(2.0)[0] - 20.0).abs() < 0.01);
        // Same key: no regrowth from flat, it keeps its depth and adds a letter.
        s.set(2.0, &[letter(font, a, 0, 20.0), letter(font, a, 1, 8.0)], None);
        let d = s.depths(2.0);
        assert!((d[0] - 20.0).abs() < 0.01 && d[1] == 0.0, "{d:?}");
    }

    #[test]
    fn a_moved_letter_follows_its_text_on_the_given_spring() {
        let (s, font, a) = sculpture();
        s.set(0.0, &[letter(font, a, 1, 16.0)], None);
        let mut up = letter(font, a, 1, 16.0);
        up.pos.y -= 54.0;
        let roll = Spring::critical_to_98(0.3);
        s.set(1.0, &[up], Some(roll));
        let size = Size::new(200.0, 60.0);
        let y = |t: f64| s.frame_at(t, size).meshes[0].model.transform_point(Vec3::new(0.0, 0.0, 0.5))[1];
        assert!((y(1.0) - 0.0).abs() < 1e-3);
        assert!(y(1.1) > 1.0 && y(1.1) < 53.0, "moving: {}", y(1.1));
        assert!((y(2.0) - 54.0).abs() < 0.05);
    }

    #[test]
    fn a_new_letter_in_a_moving_run_starts_with_the_run() {
        let (s, font, a) = sculpture();
        let roll = Spring::critical_to_98(0.3);
        s.set(0.0, &[letter(font, a, 1, 16.0)], Some(roll));
        // The run moves 40 px left at 1 s and a new letter joins it.
        let mut moved = letter(font, a, 1, 16.0);
        moved.pos.x -= 40.0;
        let mut new = letter(font, a, 2, 16.0);
        new.pos.x -= 40.0;
        s.set(1.0, &[moved, new], Some(roll));
        let size = Size::new(200.0, 60.0);
        let xs = |t: f64| -> Vec<f32> {
            s.frame_at(t, size).meshes.iter().map(|m| m.model.transform_point(Vec3::new(0.0, 0.0, 0.5))[0]).collect()
        };
        let at = xs(1.1);
        assert!((at[1] - at[0] - 10.0).abs() < 0.01, "the new letter keeps its place in the run: {at:?}");
    }

    #[test]
    fn flatten_lays_letters_down_left_to_right() {
        let (s, font, a) = sculpture();
        s.set(0.0, &[letter(font, a, 0, 20.0), letter(font, a, 5, 20.0)], None);
        s.flatten(3.0);
        let mid = s.depths(3.2);
        assert!(mid[0] < mid[1], "the left letter lies down first: {mid:?}");
        assert!(s.depths(5.0).iter().all(|d| d.abs() < 0.01));
        assert!(Shared(s.clone()).until() > 3.0);
    }

    #[test]
    fn the_front_face_sits_on_the_glyph_and_the_body_runs_back() {
        let (s, font, a) = sculpture();
        s.set(0.0, &[letter(font, a, 1, 16.0)], None);
        let size = Size::new(200.0, 60.0);
        let f = s.frame_at(5.0, size);
        assert_eq!(f.meshes.len(), 1);
        let m = f.meshes[0].model;
        // Front face (z = 0.5 in the mesh) lands at the pen, z = 0.
        let front = m.transform_point(Vec3::new(0.0, 0.0, 0.5));
        assert!((front[0] - (10.0 - 100.0)).abs() < 1e-3 && (front[1] - 0.0).abs() < 1e-3 && front[2].abs() < 1e-3);
        // The back face is 16 px back, moved right and up by the shear.
        let back = m.transform_point(Vec3::new(0.0, 0.0, -0.5));
        assert!((back[2] + 16.0).abs() < 1e-3, "{back:?}");
        assert!(back[0] > front[0] && back[1] > front[1]);
        let meshes = Shared(s.clone()).meshes();
        assert_eq!(meshes.len(), s.glyphs.len());
    }
}
