//! Materials (FX15) on the GPU: the blocks of `DisplayList::materials` in a storage buffer at
//! bind group 2 of the shape, glyph and vector pipelines, and the WGSL that reads them
//! (`wgsl/materials.wgsl`, which documents a material function's inputs).
//!
//! An instance carries only a material id; the look is the function the block names. Built-in
//! functions are neon, hologram, chrome and gradient (`vo_scene::material`). An app adds its
//! own with [`crate::Renderer::add_material`]: a WGSL function `fn name (m: Material, i: MatIn)
//! -> vec4f` and an id from `vo_scene::material::CUSTOM_FIRST` up. The renderer appends the
//! source, adds a case for the id and rebuilds the three shader modules once.

use bytemuck::{Pod, Zeroable};
use vo_scene::geom::Color;
use vo_scene::material::{Material, PARAM_SLOTS};

use crate::color::oklab_alpha;

pub const MATERIALS_WGSL: &str = include_str!("wgsl/materials.wgsl");

/// The line in `materials.wgsl` where app cases go.
const CASES_MARK: &str = "    // vo: custom material cases\n";

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
}
