//! Cameras. A camera is a plain value the app builds each frame from `t`, usually from
//! spring channels or tracks, so it is as deterministic as the rest of the scene.

use crate::math::{Mat4, Vec3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Projection {
    /// Vertical field of view in radians.
    Perspective { fov_y: f32 },
    /// World units shown from the bottom to the top of the target.
    Orthographic { height: f32 },
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
        }
    }

    /// Projection times view.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.view().then(&self.proj(aspect))
    }

    /// Pixels per world unit at view depth 1 for a target `height_px` tall. A point sprite of
    /// world size `s` at view depth `z` is `s * focal_px / z` pixels across.
    pub fn focal_px(&self, height_px: f32) -> f32 {
        match self.projection {
            Projection::Perspective { fov_y } => 0.5 * height_px / (0.5 * fov_y).tan(),
            Projection::Orthographic { height } => height_px / height,
        }
    }
}
