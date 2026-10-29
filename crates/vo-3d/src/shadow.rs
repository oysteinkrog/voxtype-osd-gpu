//! Soft shadows from the key light (EG-18): a shadow map for the directional [`crate::Light`],
//! fit to the casting meshes' bounds each frame and read with a Poisson disc of 16
//! comparison taps (PCF), so edges come out soft.
//!
//! Casters are the frame's meshes (those with a colour alpha of at least 0.5, so clear paper
//! casts nothing). Receivers are the meshes and the floor ([`crate::Floor`]): on the floor
//! the shadow darkens whatever lies there (the background, the reflection, the pools of
//! light). The raymarched hero neither casts nor receives.
//!
//! Stateless: the map is drawn again from the current pose every frame, so replay is exact.
//! [`crate::Frame3d::shadows`] `None` draws exactly as without this module and allocates no
//! shadow map.

use crate::math::{Mat4, Vec3};

pub const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth16Unorm;

/// Shadows from the key light, for [`crate::Frame3d::shadows`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadows {
    /// Half the width of the soft edge, in logical pixels on the receiving surface. The edge
    /// runs from fully lit to fully shadowed over about twice this.
    pub softness: f32,
    /// Shadow map width and height in texels. 512 (512 KB at Depth16) is plenty for an
    /// overlay; larger maps give crisper contact where casters cover a wide area.
    pub size: u32,
    /// How much of the light a shadow takes away, 0 to 1. On a mesh it scales the key
    /// light's diffuse and specular terms (ambient and rim stay); on the floor it is the
    /// opacity of the darkening.
    pub strength: f32,
}

impl Default for Shadows {
    fn default() -> Shadows {
        Shadows { softness: 3.0, size: 512, strength: 0.7 }
    }
}

/// The light's view of the casters: an orthographic box along the light direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightFit {
    /// World to light clip space: x and y -1 to 1 across the map, depth 0 to 1 over the
    /// casters' extent along the light.
    pub view_proj: Mat4,
    /// World size of one shadow map texel.
    pub texel: f32,
    /// Free texels kept on each side of the casters, so a soft edge never runs off the map.
    pub margin: u32,
}

/// Free texels on each side of the casters for a map of `size` texels.
pub fn margin(size: u32) -> u32 {
    (size / 16).max(4)
}

/// Fits the light's orthographic box to world-space boxes `(min, max)`, the casters'
/// bounds, for a map of `size` texels. The box looks along `dir` (the way the light
/// travels), is square in texels and leaves [`margin`] texels free on every side. `None`
/// when there are no boxes or `dir` is zero.
pub fn fit_light(dir: Vec3, boxes: &[(Vec3, Vec3)], size: u32) -> Option<LightFit> {
    let f = dir.normalize();
    if boxes.is_empty() || f == Vec3::ZERO {
        return None;
    }
    let up = if f.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_at(Vec3::ZERO, f, up);
    let s = f.cross(up).normalize();
    let u = s.cross(f);
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for &(a, b) in boxes {
        for k in 0..8 {
            let p = Vec3::new(
                if k & 1 == 0 { a.x } else { b.x },
                if k & 2 == 0 { a.y } else { b.y },
                if k & 4 == 0 { a.z } else { b.z },
            );
            for (i, v) in [p.dot(s), p.dot(u), p.dot(f)].into_iter().enumerate() {
                lo[i] = lo[i].min(v);
                hi[i] = hi[i].max(v);
            }
        }
    }
    let m = margin(size);
    let half = 0.5 * (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1e-4);
    let half = half * size as f32 / (size - 2 * m) as f32;
    let (cx, cy) = (0.5 * (lo[0] + hi[0]), 0.5 * (lo[1] + hi[1]));
    let pad = 0.01 * (hi[2] - lo[2]) + 1e-3;
    let proj = Mat4::orthographic(cx - half, cx + half, cy - half, cy + half, lo[2] - pad, hi[2] + pad);
    Some(LightFit { view_proj: view.then(&proj), texel: 2.0 * half / size as f32, margin: m })
}

/// The world bounds of a local box `(min, max)` moved by `model`.
pub fn world_box(model: &Mat4, (a, b): (Vec3, Vec3)) -> (Vec3, Vec3) {
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for k in 0..8 {
        let p = Vec3::new(
            if k & 1 == 0 { a.x } else { b.x },
            if k & 2 == 0 { a.y } else { b.y },
            if k & 4 == 0 { a.z } else { b.z },
        );
        let q = model.transform_point(p);
        for i in 0..3 {
            lo[i] = lo[i].min(q[i] / q[3]);
            hi[i] = hi[i].max(q[i] / q[3]);
        }
    }
    (Vec3::new(lo[0], lo[1], lo[2]), Vec3::new(hi[0], hi[1], hi[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corners((a, b): (Vec3, Vec3)) -> Vec<Vec3> {
        (0..8)
            .map(|k| {
                Vec3::new(
                    if k & 1 == 0 { a.x } else { b.x },
                    if k & 2 == 0 { a.y } else { b.y },
                    if k & 4 == 0 { a.z } else { b.z },
                )
            })
            .collect()
    }

    /// Every caster corner lands inside the map, clear of the margin, and inside the depth
    /// range, for several light directions including straight down.
    #[test]
    fn casters_fit_inside_the_map() {
        let boxes = [
            world_box(
                &Mat4::rotate_y(0.6).then(&Mat4::translation(Vec3::new(0.5, 0.4, 0.0))),
                (Vec3::new(-0.3, -0.3, -0.3), Vec3::new(0.3, 0.3, 0.3)),
            ),
            (Vec3::new(-1.3, -0.1, -0.2), Vec3::new(-0.7, 0.2, 1.1)),
        ];
        for dir in [
            Vec3::new(-0.4, -0.8, -0.5),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(1.0, -0.2, 0.3),
            Vec3::new(0.0, 0.0, -1.0),
        ] {
            let size = 512;
            let fit = fit_light(dir, &boxes, size).unwrap();
            let edge = 1.0 - 2.0 * fit.margin as f32 / size as f32 + 1e-4;
            for c in boxes.iter().flat_map(|b| corners(*b)) {
                let q = fit.view_proj.transform_point(c);
                let (x, y, z) = (q[0] / q[3], q[1] / q[3], q[2] / q[3]);
                assert!(x.abs() <= edge && y.abs() <= edge, "{dir:?}: {c:?} at {x}, {y} outside {edge}");
                assert!((0.0..=1.0).contains(&z), "{dir:?}: {c:?} depth {z}");
            }
            assert!(fit.texel > 0.0);
        }
    }

    /// Depth grows along the light: a point further along `dir` is deeper.
    #[test]
    fn depth_runs_along_the_light() {
        let dir = Vec3::new(-0.4, -0.8, -0.5);
        let fit = fit_light(dir, &[(Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0))], 512).unwrap();
        let near = fit.view_proj.transform_point(dir.normalize() * -0.5)[2];
        let far = fit.view_proj.transform_point(dir.normalize() * 0.5)[2];
        assert!(near < far, "{near} {far}");
    }

    #[test]
    fn no_casters_no_fit() {
        assert!(fit_light(Vec3::new(0.0, -1.0, 0.0), &[], 512).is_none());
        assert!(fit_light(Vec3::ZERO, &[(Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0))], 512).is_none());
    }
}
