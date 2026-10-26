//! Extruded 3D text (D306) from the same glyph outlines the Slug path draws (TY01): shaped
//! with harfrust, outlines read with skrifa, caps tessellated by lyon, walls extruded.
//!
//! One mesh per glyph, each centred on its own ink box, so a word can turn in letter by
//! letter about each letter's centre.

use harfrust::{Direction, ShaperFont};
use lyon_tessellation::math::point;
use lyon_tessellation::path::Path;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::prelude::{LocationRef, Size};
use skrifa::{GlyphId, MetadataProvider};
use vo_scene::font::Face;

use crate::math::Vec3;
use crate::mesh::Mesh;

/// One extruded glyph.
#[derive(Clone, Debug)]
pub struct GlyphMesh {
    /// The glyph's mesh, centred on its ink box centre.
    pub mesh: Mesh,
    /// Where that centre sits in the word, with the word's ink box centred on the origin.
    pub center: Vec3,
    /// Index of the glyph in the shaped run.
    pub index: usize,
}

/// Shapes `text` and extrudes each glyph. `size` is world units per em, `depth` the
/// extrusion depth in world units. Glyphs without ink (spaces) are skipped.
pub fn extrude_text(face: &Face, text: &str, size: f32, depth: f32) -> Vec<GlyphMesh> {
    let upem = face.units_per_em() as f32;
    let k = size / upem;
    let shaper = ShaperFont::new(face.shaping()).with_scale(upem as i32);
    let mut buf = harfrust::Buffer::new();
    buf.push_str(text);
    buf.guess_segment_properties();
    buf.set_direction(Direction::LeftToRight);
    harfrust::shape(&shaper, &mut buf, harfrust::ShapeOptions::new()).expect("shaping a text buffer");
    let font = face.skrifa();
    let outlines = font.outline_glyphs();

    let mut pen_x = 0.0f32;
    let mut glyphs = Vec::new();
    for (i, (info, pos)) in buf.glyph_infos().iter().zip(buf.glyph_positions()).enumerate() {
        let (ox, oy) = (pen_x + pos.x_offset as f32, pos.y_offset as f32);
        pen_x += pos.x_advance as f32;
        let Some(outline) = outlines.get(GlyphId::new(info.glyph_id)) else { continue };
        let mut pen = LyonPen { builder: Path::builder(), open: false, k, ox: ox * k, oy: oy * k };
        if outline.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).is_err() {
            continue;
        }
        if pen.open {
            pen.builder.end(true);
        }
        let path = pen.builder.build();
        // Flatten to a hundredth of a pixel-sized em at 100 px, scaled to world units.
        let mesh = Mesh::extrude(&path, depth, size * 0.002);
        if mesh.vertices.is_empty() {
            continue;
        }
        glyphs.push(GlyphMesh { mesh, center: Vec3::ZERO, index: i });
    }

    // Centre the word, then each glyph on its own centre.
    let mut lo = Vec3::new(f32::INFINITY, f32::INFINITY, 0.0);
    let mut hi = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, 0.0);
    for g in &glyphs {
        let (a, b) = g.mesh.bounds();
        lo = Vec3::new(lo.x.min(a.x), lo.y.min(a.y), 0.0);
        hi = Vec3::new(hi.x.max(b.x), hi.y.max(b.y), 0.0);
    }
    let word_center = (lo + hi) * 0.5;
    for g in &mut glyphs {
        let (a, b) = g.mesh.bounds();
        let c = Vec3::new(0.5 * (a.x + b.x), 0.5 * (a.y + b.y), 0.0);
        for v in &mut g.mesh.vertices {
            v.pos = [v.pos[0] - c.x, v.pos[1] - c.y, v.pos[2]];
        }
        g.center = c - word_center;
    }
    glyphs
}

/// Collects a skrifa outline into a lyon path, in world units.
struct LyonPen {
    builder: lyon_tessellation::path::path::Builder,
    open: bool,
    k: f32,
    ox: f32,
    oy: f32,
}

impl LyonPen {
    fn p(&self, x: f32, y: f32) -> lyon_tessellation::math::Point {
        point(self.ox + x * self.k, self.oy + y * self.k)
    }
}

impl OutlinePen for LyonPen {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.open {
            self.builder.end(true);
        }
        self.builder.begin(self.p(x, y));
        self.open = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(self.p(x, y));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.builder.quadratic_bezier_to(self.p(cx0, cy0), self.p(x, y));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.builder.cubic_bezier_to(self.p(cx0, cy0), self.p(cx1, cy1), self.p(x, y));
    }

    fn close(&mut self) {
        if self.open {
            self.builder.end(true);
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::font::{test_font_path, FontRegistry};

    #[test]
    fn word_is_centred_and_letters_are_separate() {
        let fonts = FontRegistry::new();
        let id = fonts.add_file(&test_font_path()).unwrap();
        let face = fonts.get(id).unwrap();
        let glyphs = extrude_text(&face, "Voice", 1.0, 0.2);
        assert_eq!(glyphs.len(), 5);
        let xs: Vec<f32> = glyphs.iter().map(|g| g.center.x).collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
        assert!((xs[0] + xs[4]).abs() < 0.3, "word not centred: {xs:?}");
        let (lo, hi) = glyphs[1].mesh.bounds();
        assert!((hi.z - lo.z - 0.2).abs() < 1e-5);
    }
}
