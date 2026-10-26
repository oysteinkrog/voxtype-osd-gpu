//! The small amount of 3D maths the layer needs: `Vec3` and a column-major `Mat4`.
//!
//! Conventions: right-handed world, y up, the camera looks down its own -z. Clip depth runs
//! 0 (near) to 1 (far), as in wgpu. `Mat4` is stored by columns, the layout WGSL's
//! `mat4x4f` reads.

use std::ops::{Add, Mul, Neg, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3::new(0.0, 0.0, 0.0);
    pub const X: Vec3 = Vec3::new(1.0, 0.0, 0.0);
    pub const Y: Vec3 = Vec3::new(0.0, 1.0, 0.0);
    pub const Z: Vec3 = Vec3::new(0.0, 0.0, 1.0);

    pub const fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { x, y, z }
    }

    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// The unit vector, or zero for a zero vector.
    pub fn normalize(self) -> Vec3 {
        let l = self.length();
        if l > 0.0 {
            self * (1.0 / l)
        } else {
            Vec3::ZERO
        }
    }

    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Mul<f32> for Vec3 {
    type Output = Vec3;
    fn mul(self, k: f32) -> Vec3 {
        Vec3::new(self.x * k, self.y * k, self.z * k)
    }
}

impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

/// A 4x4 matrix by columns: `cols[c][r]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4 {
    pub cols: [[f32; 4]; 4],
}

impl Default for Mat4 {
    fn default() -> Mat4 {
        Mat4::IDENTITY
    }
}

impl Mat4 {
    pub const IDENTITY: Mat4 =
        Mat4 { cols: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]] };

    pub fn translation(v: Vec3) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.cols[3] = [v.x, v.y, v.z, 1.0];
        m
    }

    pub fn scale(v: Vec3) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        (m.cols[0][0], m.cols[1][1], m.cols[2][2]) = (v.x, v.y, v.z);
        m
    }

    pub fn uniform_scale(k: f32) -> Mat4 {
        Mat4::scale(Vec3::new(k, k, k))
    }

    /// Rotation about x by `a` radians, counter-clockwise looking down the axis.
    pub fn rotate_x(a: f32) -> Mat4 {
        let (s, c) = a.sin_cos();
        Mat4 { cols: [[1.0, 0.0, 0.0, 0.0], [0.0, c, s, 0.0], [0.0, -s, c, 0.0], [0.0, 0.0, 0.0, 1.0]] }
    }

    pub fn rotate_y(a: f32) -> Mat4 {
        let (s, c) = a.sin_cos();
        Mat4 { cols: [[c, 0.0, -s, 0.0], [0.0, 1.0, 0.0, 0.0], [s, 0.0, c, 0.0], [0.0, 0.0, 0.0, 1.0]] }
    }

    pub fn rotate_z(a: f32) -> Mat4 {
        let (s, c) = a.sin_cos();
        Mat4 { cols: [[c, s, 0.0, 0.0], [-s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]] }
    }

    /// Right-handed perspective with depth 0 at `near` and 1 at `far`. `fov_y` in radians.
    pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (0.5 * fov_y).tan();
        let r = far / (near - far);
        Mat4 { cols: [[f / aspect, 0.0, 0.0, 0.0], [0.0, f, 0.0, 0.0], [0.0, 0.0, r, -1.0], [0.0, 0.0, r * near, 0.0]] }
    }

    /// Right-handed orthographic box, depth 0 at `near` and 1 at `far`.
    pub fn orthographic(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Mat4 {
        let (w, h, d) = (right - left, top - bottom, near - far);
        Mat4 {
            cols: [
                [2.0 / w, 0.0, 0.0, 0.0],
                [0.0, 2.0 / h, 0.0, 0.0],
                [0.0, 0.0, 1.0 / d, 0.0],
                [-(right + left) / w, -(top + bottom) / h, near / d, 1.0],
            ],
        }
    }

    /// The view matrix of a camera at `eye` looking at `target`.
    pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
        let f = (target - eye).normalize();
        let s = f.cross(up).normalize();
        let u = s.cross(f);
        Mat4 {
            cols: [
                [s.x, u.x, -f.x, 0.0],
                [s.y, u.y, -f.y, 0.0],
                [s.z, u.z, -f.z, 0.0],
                [-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0],
            ],
        }
    }

    /// `self * o`: `o` applies first.
    pub fn mul(&self, o: &Mat4) -> Mat4 {
        let mut out = [[0.0; 4]; 4];
        for (c, col) in out.iter_mut().enumerate() {
            for (r, v) in col.iter_mut().enumerate() {
                *v = (0..4).map(|k| self.cols[k][r] * o.cols[c][k]).sum();
            }
        }
        Mat4 { cols: out }
    }

    /// `next * self`: `self` applies first, then `next`, like `Affine::then`.
    pub fn then(&self, next: &Mat4) -> Mat4 {
        next.mul(self)
    }

    /// The inverse transpose of the upper 3x3, for normals, in a `Mat4` with no translation.
    /// A singular matrix gives the plain 3x3.
    pub fn normal_matrix(&self) -> Mat4 {
        let m = |c: usize, r: usize| self.cols[c][r];
        // Cofactors of the 3x3 equal det times the inverse transpose.
        let cof = |c: usize, r: usize| {
            let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
            let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
            m(c1, r1) * m(c2, r2) - m(c2, r1) * m(c1, r2)
        };
        let det: f32 = (0..3).map(|r| m(0, r) * cof(0, r)).sum();
        let mut out = Mat4::IDENTITY;
        for c in 0..3 {
            for r in 0..3 {
                out.cols[c][r] = if det.abs() > 1e-12 { cof(c, r) / det } else { m(c, r) };
            }
        }
        out
    }

    pub fn transform_point(&self, p: Vec3) -> [f32; 4] {
        let m = &self.cols;
        std::array::from_fn(|r| m[0][r] * p.x + m[1][r] * p.y + m[2][r] * p.z + m[3][r])
    }

    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        let m = &self.cols;
        Vec3::new(
            m[0][0] * v.x + m[1][0] * v.y + m[2][0] * v.z,
            m[0][1] * v.x + m[1][1] * v.y + m[2][1] * v.z,
            m[0][2] * v.x + m[1][2] * v.y + m[2][2] * v.z,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn perspective_maps_near_and_far_to_zero_and_one() {
        let p = Mat4::perspective(1.0, 1.5, 0.5, 20.0);
        let n = p.transform_point(Vec3::new(0.0, 0.0, -0.5));
        let f = p.transform_point(Vec3::new(0.0, 0.0, -20.0));
        assert!((n[2] / n[3]).abs() < 1e-6);
        assert!((f[2] / f[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn look_at_puts_target_on_the_minus_z_axis() {
        let v = Mat4::look_at(Vec3::new(3.0, 2.0, 5.0), Vec3::new(0.0, 0.5, 0.0), Vec3::Y);
        let t = v.transform_point(Vec3::new(0.0, 0.5, 0.0));
        let d = (Vec3::new(3.0, 1.5, 5.0)).length();
        assert!(close(t, [0.0, 0.0, -d, 1.0]), "{t:?}");
    }

    #[test]
    fn normal_matrix_keeps_normals_perpendicular_under_stretch() {
        let m = Mat4::scale(Vec3::new(3.0, 1.0, 1.0)).then(&Mat4::rotate_z(0.4));
        // A surface along (1, 1, 0) has normal (1, -1, 0).
        let tangent = m.transform_vector(Vec3::new(1.0, 1.0, 0.0));
        let normal = m.normal_matrix().transform_vector(Vec3::new(1.0, -1.0, 0.0));
        assert!(tangent.dot(normal).abs() < 1e-5);
    }

    #[test]
    fn then_applies_self_first() {
        let a = Mat4::translation(Vec3::X);
        let b = Mat4::rotate_z(std::f32::consts::FRAC_PI_2);
        let p = a.then(&b).transform_point(Vec3::ZERO);
        assert!(close(p, [0.0, 1.0, 0.0, 1.0]), "{p:?}");
    }
}
