//! Silk ribbons and cloth strips that twist to show their back and swell with the voice
//! (EG-13).
//!
//! [`Ribbon3d`] is a strip of `segments` quads along a 3D centre curve, in closed form: the
//! vertices are a pure function of `t` and a level history ([`LevelTrack`]). Folds are
//! travelling sines that move the centre curve, the twist is a travelling sine that turns the
//! strip about the curve, and the width swells with the level, which reaches the far end
//! `lag` seconds after the near end. There is no verlet and no state, so any `t` can be drawn
//! in any order. Cloth is the same strip made wide, with more columns across; it has no
//! collisions.
//!
//! The mesh keeps its vertex count, so it is added once ([`Ribbon3d::mesh`],
//! [`crate::Renderer3d::add_mesh`]) and rewritten each frame ([`Ribbon3d::vertices`],
//! [`crate::Renderer3d::update_mesh`]). The mesh shader is two-sided: back faces take
//! [`Material::back`], and [`Material::fibres`] draws fine lines along the length.
//! [`RibbonItem`] is the whole thing as a [`Source3d`] for `Element::gpu`.
//!
//! Reduced motion holds the rest pose (the shape at phase 0, base width); the level then
//! changes only the brightness ([`Ribbon3d::glow`]).

use std::f32::consts::TAU;

use vo_scene::emitter::LevelTrack;
use vo_scene::geom::{Color, Size};

use crate::camera::Camera;
use crate::frame::{Background, Frame3d, Light, Material, MeshDraw, MeshId};
use crate::item::Source3d;
use crate::math::{Mat4, Vec3};
use crate::mesh::{Mesh, Vertex};

/// A twisting, folding strip. Lengths are world units; the strip lies along x, centred on
/// the origin, with its width along y and its front facing +z at rest.
#[derive(Clone, Debug, PartialEq)]
pub struct Ribbon3d {
    /// Quads along the length.
    pub segments: u32,
    /// Columns across the width: 1 for a ribbon, more for cloth so its folds shade smoothly.
    pub across: u32,
    pub length: f32,
    /// Width at level 0.
    pub width: f32,
    /// Extra width at level 1, as a share of `width`.
    pub swell: f32,
    /// Seconds the level takes to travel from the near end (x < 0) to the far end.
    pub lag: f64,
    /// Twist amplitude in radians: past a quarter turn the back shows.
    pub twist: f32,
    /// Twist waves along the length, and how many pass a point per second.
    pub twist_waves: f32,
    pub twist_speed: f32,
    /// How far the folds move the centre curve, in world units.
    pub fold: f32,
    pub fold_waves: f32,
    pub fold_speed: f32,
    /// A standing bow of the centre curve (the middle this much above the ends).
    pub bow: f32,
    /// Brightness added at level 1, as emissive.
    pub glow: f32,
    pub reduced_motion: bool,
}

impl Default for Ribbon3d {
    fn default() -> Ribbon3d {
        Ribbon3d {
            segments: 96,
            across: 1,
            length: 3.2,
            width: 0.3,
            swell: 0.7,
            lag: 0.4,
            twist: 150f32.to_radians(),
            twist_waves: 0.8,
            twist_speed: 0.22,
            fold: 0.22,
            fold_waves: 1.3,
            fold_speed: 0.16,
            bow: 0.1,
            glow: 0.5,
            reduced_motion: false,
        }
    }
}

/// `sin(TAU * (waves * s - speed * t) + offset)`, with the time phase taken modulo one in
/// f64 so long runs keep their precision.
fn wave(waves: f32, s: f32, speed: f32, t: f64, offset: f32) -> f32 {
    let ph = (speed as f64 * t).rem_euclid(1.0) as f32;
    (TAU * (waves * s - ph) + offset).sin()
}

impl Ribbon3d {
    /// Cloth: the same strip, wide, with eight columns across. Its folds are long and
    /// gentle: a fold tighter than half the width would fold the edges through themselves.
    pub fn cloth() -> Ribbon3d {
        Ribbon3d {
            across: 8,
            width: 1.1,
            swell: 0.35,
            twist: 60f32.to_radians(),
            twist_waves: 0.5,
            fold: 0.16,
            fold_waves: 0.9,
            ..Ribbon3d::default()
        }
    }

    /// The time the motion is drawn at: `t`, or 0 under reduced motion.
    fn phase_t(&self, t: f64) -> f64 {
        if self.reduced_motion {
            0.0
        } else {
            t
        }
    }

    /// The centre curve at `s` (0 to 1 along the length).
    pub fn centre(&self, s: f32, t: f64) -> Vec3 {
        let t = self.phase_t(t);
        let x = (s - 0.5) * self.length;
        let u = 2.0 * s - 1.0;
        let bow = self.bow * (1.0 - u * u);
        let (k, v) = (self.fold_waves, self.fold_speed);
        let y = self.fold * (0.75 * wave(k, s, v, t, 0.0) + 0.25 * wave(1.7 * k, s, 1.3 * v, t, 1.1));
        let z = self.fold * 0.6 * wave(0.8 * k, s, 0.7 * v, t, 2.3);
        Vec3::new(x, bow + y, z)
    }

    /// The twist at `s`, in radians: 0 shows the front flat to +z.
    pub fn twist_at(&self, s: f32, t: f64) -> f32 {
        let t = self.phase_t(t);
        let (k, v) = (self.twist_waves, self.twist_speed);
        self.twist * (0.8 * wave(k, s, v, t, 0.4) + 0.2 * wave(2.1 * k, s, 0.6 * v, t, 1.7))
    }

    /// The width at `s`: the level reaches each point `lag * s` seconds late.
    pub fn width_at(&self, s: f32, level: &LevelTrack, t: f64) -> f32 {
        if self.reduced_motion {
            return self.width;
        }
        let l = level.at(t - self.lag * s as f64).clamp(0.0, 1.0);
        self.width * (1.0 + self.swell * l)
    }

    /// Emissive to add for the level at `t` (in both motion modes).
    pub fn glow(&self, level: &LevelTrack, t: f64) -> f32 {
        self.glow * level.at(t).clamp(0.0, 1.0)
    }

    /// Vertices per row across the strip.
    fn row(&self) -> usize {
        self.across.max(1) as usize + 1
    }

    /// Vertex count: `(segments + 1) * (across + 1)`.
    pub fn vertex_count(&self) -> usize {
        (self.segments.max(1) as usize + 1) * self.row()
    }

    /// The strip at `t`, into `out` (cleared first). Row `i` along the length holds
    /// `across + 1` vertices from one edge to the other; `uv` is `(s, across share)`.
    pub fn vertices(&self, level: &LevelTrack, t: f64, out: &mut Vec<Vertex>) {
        out.clear();
        let n = self.segments.max(1);
        let cols = self.across.max(1);
        let h = 0.5 / n as f32;
        for i in 0..=n {
            let s = i as f32 / n as f32;
            let c = self.centre(s, t);
            // Tangent from the curve itself, so the frame is as pure as the curve.
            let (a, b) = (self.centre((s - h).max(0.0), t), self.centre((s + h).min(1.0), t));
            let tan = (b - a).normalize();
            let side0 = (Vec3::Y - tan * tan.dot(Vec3::Y)).normalize();
            let face0 = tan.cross(side0);
            let (sn, cs) = self.twist_at(s, t).sin_cos();
            let side = side0 * cs + face0 * sn;
            let face = face0 * cs - side0 * sn;
            let w = self.width_at(s, level, t);
            for j in 0..=cols {
                let v = j as f32 / cols as f32;
                let p = c + side * ((v - 0.5) * w);
                out.push(Vertex { pos: p.to_array(), normal: face.to_array(), uv: [s, v] });
            }
        }
    }

    /// Triangle indices, wound to face the vertex normal (the front).
    pub fn indices(&self) -> Vec<u32> {
        let (n, row) = (self.segments.max(1), self.row() as u32);
        let mut idx = Vec::with_capacity(n as usize * (row as usize - 1) * 6);
        for i in 0..n {
            for j in 0..row - 1 {
                let a = i * row + j;
                let b = a + row;
                idx.extend([a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        idx
    }

    /// The mesh at rest (no level, `t` = 0), to add once.
    pub fn mesh(&self) -> Mesh {
        let mut vertices = Vec::new();
        self.vertices(&LevelTrack::default(), 0.0, &mut vertices);
        Mesh { vertices, indices: self.indices() }
    }
}

/// How a [`RibbonItem`] looks: front and back colours, fibres and the camera.
#[derive(Clone, Debug, PartialEq)]
pub struct RibbonLook {
    pub front: Color,
    pub back: Color,
    pub fibres: f32,
    pub fibre: f32,
    pub emissive: f32,
    pub specular: f32,
    pub shininess: f32,
    pub camera: Camera,
    pub background: Background,
    pub light: Light,
}

impl Default for RibbonLook {
    fn default() -> RibbonLook {
        RibbonLook {
            front: Color::hex(0xe8b4ff, 1.0),
            back: Color::hex(0x5a3cff, 1.0),
            fibres: 14.0,
            fibre: 0.5,
            emissive: 0.15,
            specular: 0.6,
            shininess: 40.0,
            camera: Camera::orbit(Vec3::ZERO, 0.0, 0.12, 3.3),
            background: Background::Transparent,
            light: Light::default(),
        }
    }
}

/// A ribbon as a 3D item: `Element::gpu(key, vo_3d::item(RibbonItem { .. }))`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RibbonItem {
    pub ribbon: Ribbon3d,
    pub level: LevelTrack,
    pub look: RibbonLook,
}

impl RibbonItem {
    /// The material at `t`: the look plus the level's glow.
    pub fn material(&self, t: f64) -> Material {
        let l = &self.look;
        Material {
            color: l.front,
            emissive: l.emissive + self.ribbon.glow(&self.level, t),
            specular: l.specular,
            shininess: l.shininess,
            back: Some(l.back),
            fibres: l.fibres,
            fibre: l.fibre,
            texture: None,
        }
    }
}

impl Source3d for RibbonItem {
    fn meshes(&self) -> Vec<Mesh> {
        vec![self.ribbon.mesh()]
    }

    fn vertices(&self, t: f64, out: &mut Vec<(u32, Vec<Vertex>)>) {
        let mut v = Vec::with_capacity(self.ribbon.vertex_count());
        self.ribbon.vertices(&self.level, t, &mut v);
        out.push((0, v));
    }

    fn frame(&self, t: f64, _size: Size) -> Frame3d {
        Frame3d {
            camera: self.look.camera,
            background: self.look.background,
            light: self.look.light,
            meshes: vec![MeshDraw { mesh: MeshId(0), model: Mat4::IDENTITY, material: self.material(t) }],
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loud() -> LevelTrack {
        let mut l = LevelTrack::default();
        l.push(0.0, 0.0);
        l.push(1.0, 1.0);
        l
    }

    fn mean_width(r: &Ribbon3d, level: &LevelTrack, t: f64) -> f32 {
        let mut v = Vec::new();
        r.vertices(level, t, &mut v);
        let row = r.across as usize + 1;
        let rows: Vec<f32> =
            v.chunks(row).map(|c| (Vec3::from(c[row - 1].pos) - Vec3::from(c[0].pos)).length()).collect();
        rows.iter().sum::<f32>() / rows.len() as f32
    }

    #[test]
    fn counts_match_the_mesh() {
        for r in [Ribbon3d::default(), Ribbon3d::cloth()] {
            let m = r.mesh();
            assert_eq!(m.vertices.len(), r.vertex_count());
            assert_eq!(m.indices.len(), r.segments as usize * r.across as usize * 6);
            assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
        }
        assert_eq!(Ribbon3d::default().vertex_count(), 97 * 2);
    }

    #[test]
    fn width_swells_with_level() {
        let r = Ribbon3d::default();
        let quiet = mean_width(&r, &LevelTrack::default(), 3.0);
        let wide = mean_width(&r, &loud(), 3.0);
        assert!((quiet - r.width).abs() < 1e-4, "{quiet}");
        assert!(wide >= 1.3 * quiet, "level 1 width {wide} against {quiet}");
        // The far end hears it later: shortly after the rise the near end is wider.
        let mut v = Vec::new();
        r.vertices(&loud(), 1.15, &mut v);
        let w = |row: usize| (Vec3::from(v[2 * row + 1].pos) - Vec3::from(v[2 * row].pos)).length();
        assert!(w(0) > w(96) + 0.02, "{} {}", w(0), w(96));
    }

    #[test]
    fn reduced_motion_holds_the_rest_pose() {
        let r = Ribbon3d { reduced_motion: true, ..Ribbon3d::default() };
        let (mut a, mut b) = (Vec::new(), Vec::new());
        r.vertices(&LevelTrack::default(), 0.0, &mut a);
        r.vertices(&loud(), 7.3, &mut b);
        assert_eq!(a, b, "reduced motion moved");
        assert!(r.glow(&loud(), 7.3) > 0.4, "the level still brightens it");
    }

    #[test]
    fn twist_turns_the_face_round() {
        let r = Ribbon3d { twist: std::f32::consts::PI, ..Ribbon3d::default() };
        // Somewhere along the strip the front faces away from +z (the back shows).
        let mut v = Vec::new();
        r.vertices(&LevelTrack::default(), 1.0, &mut v);
        let away = v.iter().filter(|p| p.normal[2] < 0.0).count();
        assert!(away > v.len() / 10, "{away} of {}", v.len());
        // At rest twist and with twist 0 everything faces +z.
        let flat = Ribbon3d { twist: 0.0, fold: 0.0, bow: 0.0, ..Ribbon3d::default() };
        flat.vertices(&LevelTrack::default(), 1.0, &mut v);
        assert!(v.iter().all(|p| p.normal[2] > 0.999));
    }

    #[test]
    fn triangles_face_their_normals() {
        // The strip never folds through itself: every triangle faces its vertex normal, for
        // the ribbon and the cloth, quiet and loud, over a few seconds.
        for r in [Ribbon3d::default(), Ribbon3d::cloth()] {
            let idx = r.indices();
            let mut v = Vec::new();
            for level in [LevelTrack::default(), loud()] {
                for i in 0..40 {
                    r.vertices(&level, i as f64 * 0.25, &mut v);
                    for t in idx.as_chunks::<3>().0 {
                        let [a, b, c] = [t[0], t[1], t[2]].map(|i| v[i as usize]);
                        let face =
                            (Vec3::from(b.pos) - Vec3::from(a.pos)).cross(Vec3::from(c.pos) - Vec3::from(a.pos));
                        assert!(face.dot(Vec3::from(a.normal)) > 0.0, "{t:?} {a:?} {b:?} {c:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn pure_in_t_whatever_the_order() {
        let r = Ribbon3d::default();
        let level = loud();
        let times: Vec<f64> = (0..40).map(|i| i as f64 * 0.137).collect();
        let sample = |order: &[usize]| {
            let mut out = vec![Vec::new(); times.len()];
            let mut buf = Vec::new();
            for &i in order {
                r.vertices(&level, times[i], &mut buf);
                out[i] = buf
                    .iter()
                    .flat_map(|v| v.pos.into_iter().chain(v.normal).chain(v.uv))
                    .map(f32::to_bits)
                    .collect::<Vec<u32>>();
            }
            out
        };
        let forward: Vec<usize> = (0..times.len()).collect();
        // A fixed shuffle (an LCG), so the test is the same every run.
        let mut shuffled = forward.clone();
        let mut x = 12345u64;
        for i in (1..shuffled.len()).rev() {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            shuffled.swap(i, (x >> 33) as usize % (i + 1));
        }
        assert_ne!(forward, shuffled);
        assert_eq!(sample(&forward), sample(&shuffled));
    }

    #[test]
    fn first_cpu_frame_does_not_panic() {
        // The CPU first frame skips GPU items: a scene holding a ribbon still draws.
        use vo_scene::element::Element;
        use vo_scene::font::FontRegistry;
        use vo_scene::Scene;
        let src = crate::item(RibbonItem::default());
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(300.0, 150.0), 1.0);
        scene.apply(
            Element::panel("root")
                .size(300.0, 150.0)
                .fill(Color::hex(0x07090f, 1.0))
                .child(Element::gpu("ribbon", src).size(300.0, 150.0)),
            0.0,
        );
        let list = scene.sample(0.5);
        let mut cpu = vo_render::cpu::CpuRenderer::new(FontRegistry::new());
        cpu.draw(&list, 1.0, 300, 150);
        assert_eq!(cpu.pixels().len(), 300 * 150);
    }
}
