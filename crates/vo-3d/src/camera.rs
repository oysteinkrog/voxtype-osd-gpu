//! Cameras. A camera is a plain value the app builds each frame from `t`, usually from
//! spring channels or tracks, so it is as deterministic as the rest of the scene.

use crate::math::{Mat4, Vec3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Projection {
    /// Vertical field of view in radians.
    Perspective { fov_y: f32 },
    /// World units shown from the bottom to the top of the target.
    Orthographic { height: f32 },
    /// A window of a larger perspective view, with the camera looking straight down -z (the
    /// stage camera, [`Camera::stage`]). `focal_px` is target pixels per world unit at view
    /// depth 1; `principal` is where the view axis meets the target, in target pixels from
    /// its top left; `size` is the target's size in pixels. The aspect passed to the
    /// methods is ignored.
    Window { focal_px: f32, principal: [f32; 2], size: [f32; 2] },
}

/// An orbit camera: it sits `distance` from `target`, at `yaw` around the world y axis and
/// `pitch` above the horizon (both radians).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub projection: Projection,
    pub near: f32,
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Camera {
        Camera {
            target: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            distance: 4.0,
            projection: Projection::Perspective { fov_y: 40f32.to_radians() },
            near: 0.1,
            far: 100.0,
        }
    }
}

impl Camera {
    pub fn orbit(target: Vec3, yaw: f32, pitch: f32, distance: f32) -> Camera {
        Camera { target, yaw, pitch, distance, ..Camera::default() }
    }

    /// The stage camera (`vo_scene::stage`) for an item, in world units of logical pixels
    /// about the item's centre, y up and z toward the viewer. The eye is `perspective` in
    /// front of the stage origin, so a point at z = 0 lands on the same pixel as the 2D
    /// layout puts it. `origin` and `centre` are in target pixels (y down), `scale` is
    /// target pixels per logical pixel, and `rect` is the target's place on the surface:
    /// `x, y, w, h` in the same pixels.
    pub fn stage(perspective: f32, origin: [f32; 2], centre: [f32; 2], scale: f32, rect: [f32; 4]) -> Camera {
        let eye_x = (origin[0] - centre[0]) / scale;
        let eye_y = -(origin[1] - centre[1]) / scale;
        Camera {
            target: Vec3::new(eye_x, eye_y, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            distance: perspective,
            projection: Projection::Window {
                focal_px: perspective * scale,
                principal: [origin[0] - rect[0], origin[1] - rect[1]],
                size: [rect[2], rect[3]],
            },
            near: 0.05 * perspective,
            far: 4.0 * perspective,
        }
    }

    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.target + Vec3::new(sy * cp, sp, cy * cp) * self.distance
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at(self.eye(), self.target, Vec3::Y)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        match self.projection {
            Projection::Perspective { fov_y } => Mat4::perspective(fov_y, aspect, self.near, self.far),
            Projection::Orthographic { height } => {
                let (h, w) = (0.5 * height, 0.5 * height * aspect);
                Mat4::orthographic(-w, w, -h, h, self.near, self.far)
            }
            Projection::Window { focal_px: f, principal: [px, py], size: [w, h] } => {
                // Pixel (u, v) = principal + f * (x, -y) / -z, then to the target's NDC.
                let r = self.far / (self.near - self.far);
                Mat4 {
                    cols: [
                        [2.0 * f / w, 0.0, 0.0, 0.0],
                        [0.0, 2.0 * f / h, 0.0, 0.0],
                        [1.0 - 2.0 * px / w, 2.0 * py / h - 1.0, r, -1.0],
                        [0.0, 0.0, r * self.near, 0.0],
                    ],
                }
            }
        }
    }

    /// Projection times view.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.view().then(&self.proj(aspect))
    }

    /// The ray through normalised device coordinates as `(o0, ox, oy, d0, dx, dy)`: the ray
    /// at `(x, y)` (each -1 to 1, y up) starts at `o0 + x * ox + y * oy` and runs along
    /// `d0 + x * dx + y * dy`. Linear in `x` and `y` for both projections.
    pub fn ray_basis(&self, aspect: f32) -> [Vec3; 6] {
        let eye = self.eye();
        let fwd = (self.target - eye).normalize();
        let right = fwd.cross(Vec3::Y).normalize();
        let up = right.cross(fwd);
        match self.projection {
            Projection::Perspective { fov_y } => {
                let ty = (0.5 * fov_y).tan();
                [eye, Vec3::ZERO, Vec3::ZERO, fwd, right * (ty * aspect), up * ty]
            }
            Projection::Orthographic { height } => {
                let (h, w) = (0.5 * height, 0.5 * height * aspect);
                [eye, right * w, up * h, fwd, Vec3::ZERO, Vec3::ZERO]
            }
            Projection::Window { focal_px: f, principal: [px, py], size: [w, h] } => {
                let off = right * ((0.5 * w - px) / f) + up * ((py - 0.5 * h) / f);
                [eye, Vec3::ZERO, Vec3::ZERO, fwd + off, right * (0.5 * w / f), up * (0.5 * h / f)]
            }
        }
    }

    /// Pixels per world unit at view depth 1 for a target `height_px` tall. A point sprite of
    /// world size `s` at view depth `z` is `s * focal_px / z` pixels across.
    pub fn focal_px(&self, height_px: f32) -> f32 {
        match self.projection {
            Projection::Perspective { fov_y } => 0.5 * height_px / (0.5 * fov_y).tan(),
            Projection::Orthographic { height } => height_px / height,
            Projection::Window { focal_px, .. } => focal_px,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Target pixels of world point `p` through `cam` on a target `size` big.
    fn pixel(cam: &Camera, p: Vec3, size: [f32; 2]) -> [f32; 2] {
        let c = cam.view_proj(size[0] / size[1]).transform_point(p);
        [(c[0] / c[3] + 1.0) * 0.5 * size[0], (1.0 - c[1] / c[3]) * 0.5 * size[1]]
    }

    #[test]
    fn stage_plane_lands_on_the_layout() {
        // An item centred at (450, 200) on a 600x300 view at scale 2, drawn into a window
        // of the surface at (700, 250), 400x300 pixels.
        let (scale, origin, centre) = (2.0, [600.0, 300.0], [900.0, 400.0]);
        let rect = [700.0, 250.0, 400.0, 300.0];
        let cam = Camera::stage(1000.0, origin, centre, scale, rect);
        for (x, y) in [(0.0, 0.0), (40.0, 25.0), (-60.0, -10.0)] {
            let [u, v] = pixel(&cam, Vec3::new(x, y, 0.0), [rect[2], rect[3]]);
            let want = [centre[0] + x * scale - rect[0], centre[1] - y * scale - rect[1]];
            assert!((u - want[0]).abs() < 0.01 && (v - want[1]).abs() < 0.01, "{x},{y}: {u},{v} vs {want:?}");
        }
        // Pushed back by the perspective distance: half way to the origin.
        let [u, v] = pixel(&cam, Vec3::new(0.0, 0.0, -1000.0), [rect[2], rect[3]]);
        let want = [(origin[0] + centre[0]) / 2.0 - rect[0], (origin[1] + centre[1]) / 2.0 - rect[1]];
        assert!((u - want[0]).abs() < 0.01 && (v - want[1]).abs() < 0.01, "{u},{v} vs {want:?}");
        assert_eq!(cam.focal_px(123.0), 2000.0);
    }

    #[test]
    fn stage_rays_go_through_their_pixels() {
        let rect = [0.0, 0.0, 600.0, 300.0];
        let cam = Camera::stage(800.0, [300.0, 150.0], [420.0, 90.0], 1.0, rect);
        let [o0, ox, oy, d0, dx, dy] = cam.ray_basis(2.0);
        for (x, y) in [(0.0, 0.0), (0.5, -0.25), (-1.0, 1.0)] {
            let o = o0 + ox * x + oy * y;
            let d = d0 + dx * x + dy * y;
            // Where the ray meets z = 0 projects back to the same NDC.
            let p = o + d * (-o.z / d.z);
            let [u, v] = pixel(&cam, p, [600.0, 300.0]);
            let want = [(x + 1.0) * 300.0, (1.0 - y) * 150.0];
            assert!((u - want[0]).abs() < 0.01 && (v - want[1]).abs() < 0.01, "{x},{y}: {u},{v} vs {want:?}");
        }
    }
}
