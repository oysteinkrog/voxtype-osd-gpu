//! Materials (FX15) on the GPU: the blocks of `DisplayList::materials` in a storage buffer at
//! bind group 2 of the shape, glyph and vector pipelines, and the WGSL that reads them
//! (`wgsl/materials.wgsl`, which documents a material function's inputs).
//!
//! An instance carries only a material id; the look is the function the block names. Built-in
//! functions are neon, hologram, chrome and gradient (`vo_scene::material`). An app adds its
//! own with [`crate::Renderer::add_material`]: a WGSL function `fn name (m: Material, i: MatIn)
//! -> vec4f` and an id from `vo_scene::material::CUSTOM_FIRST` up. The renderer appends the
//! source, adds a case for the id and rebuilds the three shader modules once.
//!
//! Textures (EG-02): a material that reads the backdrop or the image slots
//! (`vo_scene::material::READS_TEXTURES`) is drawn with textured variants of the three
//! pipelines. Their shader is the same source with the stub `tex_backdrop` and `tex_image` in
//! `materials.wgsl` swapped for `wgsl/material_tex.wgsl` ([`with_textures`]), and their group 2
//! adds the backdrop, a sampler and the image slots. Draws whose materials read no texture
//! keep the plain pipelines and bind group.
//!
//! Text (EG-31): a material with `vo_scene::material::READS_TEXT` takes the same textured
//! pipelines, whose group 2 also binds the text field (binding 7) and the word table (binding
//! 8) from [`crate::text_field`]. `text_dist`, `word_near`, `text_word` and `text_word_count`
//! are stubs in `materials.wgsl` and real in `material_tex.wgsl`.
//!
//! Tiles (EG-25): the grain materials (`Material::paper` and its siblings) read the built-in
//! tiles through an image slot. A frame that draws one whose slot is empty gets the slot
//! filled with [`tile_texels`] as a texture, made once and kept.

use bytemuck::{Pod, Zeroable};
use vo_scene::assets::{self, TILE};
use vo_scene::geom::Color;
use vo_scene::material::{Material, Tile, PARAM_SLOTS};

use crate::color::oklab_alpha;

pub const MATERIALS_WGSL: &str = include_str!("wgsl/materials.wgsl");
pub const MATERIAL_TEX_WGSL: &str = include_str!("wgsl/material_tex.wgsl");

/// The lines around the stub texture helpers in `materials.wgsl`.
const TEX_BEGIN: &str = "// vo: texture helpers begin\n";
const TEX_END: &str = "// vo: texture helpers end\n";

/// The line in `materials.wgsl` where app cases go.
const CASES_MARK: &str = "    // vo: custom material cases\n";

/// The side of the tile texture: a tile with a one-texel border on each side.
pub const TILE_TEXTURE: usize = TILE + 2;

/// The tile texture's RGBA8 texels (EG-25): the four built-in tiles one to a channel, in
/// [`Tile::ALL`] order, each with a one-texel border copied from its opposite edges, so a
/// bilinear sample near an edge blends with the texels across the wrap.
pub fn tile_texels() -> Vec<u8> {
    let tiles: Vec<&[u8]> = Tile::ALL.iter().map(|t| assets::get(t.asset()).expect("built-in tile").bytes).collect();
    let mut out = vec![0u8; TILE_TEXTURE * TILE_TEXTURE * 4];
    for y in 0..TILE_TEXTURE {
        for x in 0..TILE_TEXTURE {
            let (tx, ty) = ((x + TILE - 1) % TILE, (y + TILE - 1) % TILE);
            for (c, tile) in tiles.iter().enumerate() {
                out[(y * TILE_TEXTURE + x) * 4 + c] = tile[ty * TILE + tx];
            }
        }
    }
    out
}

/// The tile texture on the GPU.
pub(crate) fn tile_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let n = TILE_TEXTURE as u32;
    let size = wgpu::Extent3d { width: n, height: n, depth_or_array_layers: 1 };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("vo-material-tiles"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        tex.as_image_copy(),
        &tile_texels(),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * n), rows_per_image: Some(n) },
        size,
    );
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

/// One material as the shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuMaterial {
    /// Function id, flags.
    pub head: [u32; 4],
    /// Seconds since the element appeared, reach in layer units.
    pub timing: [f32; 4],
    pub params: [[f32; 4]; PARAM_SLOTS],
}

impl GpuMaterial {
    /// The block with its colour slots turned from straight sRGB into OKLab plus alpha.
    pub fn new(m: &Material) -> GpuMaterial {
        let mut params = m.params;
        for (i, p) in params.iter_mut().enumerate() {
            if m.colors & (1 << i) != 0 {
                *p = oklab_alpha(Color::rgba(p[0], p[1], p[2], p[3]), 1.0);
            }
        }
        GpuMaterial { head: [m.func, m.flags, 0, 0], timing: [m.time, m.reach, 0.0, 0.0], params }
    }
}

/// An app material function.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialFn {
    pub func: u32,
    /// The WGSL function's name.
    pub name: String,
    /// WGSL that defines it, and anything it needs.
    pub source: String,
}

/// The materials WGSL with `custom` functions appended and a case for each in the switch.
pub fn materials_source(custom: &[MaterialFn]) -> String {
    let base = crate::hot::wgsl("materials.wgsl", MATERIALS_WGSL);
    if custom.is_empty() {
        return base.into_owned();
    }
    let cases: String =
        custom.iter().map(|f| format!("    case {}u: {{ return {} (m, i); }}\n", f.func, f.name)).collect();
    let mut s = base.replacen(CASES_MARK, &(cases + CASES_MARK), 1);
    for f in custom {
        s.push('\n');
        s.push_str(&f.source);
    }
    s
}

/// A shape, glyph or vector shader source with the stub texture helpers replaced by the
/// textured ones (EG-02). A source without the stubs comes back unchanged.
pub fn with_textures(source: &str) -> String {
    let Some(a) = source.find(TEX_BEGIN) else { return source.to_string() };
    let Some(b) = source[a..].find(TEX_END).map(|b| a + b + TEX_END.len()) else { return source.to_string() };
    let tex = crate::hot::wgsl("material_tex.wgsl", MATERIAL_TEX_WGSL);
    [&source[..a], &tex, "\n", &source[b..]].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::material::CUSTOM_FIRST;

    #[test]
    fn colour_slots_become_oklab() {
        let m = Material::custom(CUSTOM_FIRST).color(0, Color::WHITE).param(1, [0.25, 0.5, 0.75, 1.0]);
        let g = GpuMaterial::new(&m);
        assert!((g.params[0][0] - 1.0).abs() < 1e-3 && g.params[0][1].abs() < 1e-3);
        assert_eq!(g.params[1], [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(g.head[0], CUSTOM_FIRST);
    }

    #[test]
    fn app_functions_get_a_case() {
        let f = MaterialFn {
            func: CUSTOM_FIRST + 1,
            name: "mat_flat".into(),
            source: "fn mat_flat (m: Material, i: MatIn) -> vec4f { return m.p[0] * i.cov; }".into(),
        };
        let s = materials_source(std::slice::from_ref(&f));
        assert!(s.contains("case 65u: { return mat_flat (m, i); }"));
        assert!(s.contains(&f.source));
        assert!(!materials_source(&[]).contains("mat_flat"));
    }

    #[test]
    fn textures_replace_the_stubs() {
        let plain = materials_source(&[]);
        assert!(plain.contains("fn tex_backdrop") && !plain.contains("mat_backdrop"));
        let tex = with_textures(&plain);
        assert!(tex.contains("var mat_backdrop") && tex.contains("fn tex_image"));
        assert_eq!(tex.matches("fn tex_backdrop").count(), 1);
        assert!(plain.contains("fn word_near") && !plain.contains("mat_words"));
        assert!(tex.contains("var<storage, read> mat_words") && tex.matches("fn text_dist").count() == 1);
    }
}
