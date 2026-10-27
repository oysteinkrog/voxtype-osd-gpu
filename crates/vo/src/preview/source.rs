//! What the preview shows: an [`App`] through a [`Player`], or a custom draw function for
//! scenes that are not a display list yet (the 3D stack). The window, contact sheets and the
//! seek bench only see this trait.

use vo_render::{Renderer, Target};
use vo_scene::display::DisplayList;
use vo_scene::geom::{Affine, Point, Rect, Size};
use vo_scene::FontRegistry;

use super::chrome::NodeBox;
use super::{Player, SeekStats};
use crate::{App, Tokens};

/// What the overlays ask for: whether to list moving channels, the stage scale and its rect.
pub(crate) struct OverlayAsk {
    pub springs: bool,
    pub fit: f32,
    pub stage: Rect,
}

pub(crate) trait Source {
    /// Logical size of the scene.
    fn size(&self) -> Size;
    fn duration(&self) -> f64;
    /// The font table the scene's glyphs index; the chrome adds its fonts to it.
    fn fonts(&self) -> FontRegistry;
    /// Times of every event, for the ticks on the track.
    fn event_times(&self) -> Vec<f64>;
    fn set_tokens(&mut self, tokens: Tokens) -> Result<(), String>;
    /// Moves to `t` without drawing; for the seek bench.
    fn seek(&mut self, t: f64);
    fn seek_stats(&self) -> Option<SeekStats>;
    /// Sets up a renderer made for this source (`Options::configure` for an app).
    fn configure(&self, r: &mut Renderer) {
        let _ = r;
    }
    /// Draws the frame at `t` into `target`, `scale` physical pixels per logical unit.
    fn draw(&mut self, t: f64, scale: f32, r: &mut Renderer, target: &mut Target);
    /// Node boxes and moving-channel lines at the last drawn `t`. Empty without a scene.
    fn overlays(&self, t: f64, ask: &OverlayAsk) -> (Vec<NodeBox>, Vec<String>) {
        let _ = (t, ask);
        (Vec::new(), Vec::new())
    }
    /// Lines for the stats overlay.
    fn stats(&self) -> Vec<String> {
        Vec::new()
    }
    /// Glows, shadows and glyph effects at the last drawn `t`, in scene units.
    fn shader_effects(&self) -> Vec<super::effects::FxBox> {
        Vec::new()
    }
}

/// An app and its scenario.
pub(crate) struct AppSource<A: App> {
    pub player: Player<A>,
    list: DisplayList,
}

impl<A: App> AppSource<A> {
    pub fn new(player: Player<A>) -> AppSource<A> {
        AppSource { player, list: DisplayList::default() }
    }
}

impl<A: App> Source for AppSource<A> {
    fn size(&self) -> Size {
        self.player.scenario().size
    }

    fn duration(&self) -> f64 {
        self.player.scenario().duration
    }

    fn fonts(&self) -> FontRegistry {
        self.player.options().fonts.clone()
    }

    fn event_times(&self) -> Vec<f64> {
        self.player.event_times(self.duration())
    }

    fn set_tokens(&mut self, tokens: Tokens) -> Result<(), String> {
        self.player.set_tokens(tokens);
        Ok(())
    }

    fn seek(&mut self, t: f64) {
        self.player.sample_into(t, &mut self.list);
    }

    fn seek_stats(&self) -> Option<SeekStats> {
        Some(self.player.stats())
    }

    fn configure(&self, r: &mut Renderer) {
        self.player.options().configure(r);
    }

    fn draw(&mut self, t: f64, scale: f32, r: &mut Renderer, target: &mut Target) {
        self.player.sample_into(t, &mut self.list);
        r.render_at(&self.list, scale, target);
    }

    fn overlays(&self, t: f64, ask: &OverlayAsk) -> (Vec<NodeBox>, Vec<String>) {
        let scene = self.player.scene();
        let (fit, stage) = (ask.fit, ask.stage);
        let map = |m: Affine, w: f32, h: f32| {
            let pts =
                [Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(0.0, h), Point::new(w, h)].map(|p| m.apply(p));
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for p in pts {
                x0 = x0.min(p.x);
                y0 = y0.min(p.y);
                x1 = x1.max(p.x);
                y1 = y1.max(p.y);
            }
            Rect::new(stage.x + x0 * fit, stage.y + y0 * fit, (x1 - x0) * fit, (y1 - y0) * fit)
        };
        let mut boxes = Vec::new();
        let mut lines = Vec::new();
        let mut more = 0;
        for id in scene.node_ids() {
            let Some(s) = scene.sampled(id, t) else { continue };
            let states = scene.channel_states(id, t);
            let moving: Vec<_> = states.iter().filter(|c| c.settles_at > t).collect();
            let target = (ask.springs && !moving.is_empty())
                .then(|| scene.target_box(id, t))
                .flatten()
                .map(|(m, size)| map(m, size.w, size.h));
            boxes.push(NodeBox { rect: map(s.transform, s.size.w, s.size.h), target, depth: scene.depth(id) });
            if ask.springs {
                // Channels still inside their settle threshold but visibly at rest add noise.
                for c in moving.into_iter().filter(|c| (c.value - c.target).abs() > 1e-3) {
                    if lines.len() < 16 {
                        lines.push(format!(
                            "{} {} {:.2} -> {:.2}  {:.2} s",
                            scene.path(id),
                            c.name,
                            c.value,
                            c.target,
                            c.settles_at - t
                        ));
                    } else {
                        more += 1;
                    }
                }
            }
        }
        if more > 0 {
            lines.push(format!("and {more} more moving channels"));
        }
        (boxes, lines)
    }

    fn shader_effects(&self) -> Vec<super::effects::FxBox> {
        super::effects::shader_effects(&self.list)
    }

    fn stats(&self) -> Vec<String> {
        let mut out = Vec::new();
        // Count every arena through serde, so arenas added later show up without a change here.
        if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(&self.list) {
            let counts: Vec<String> =
                map.iter().filter_map(|(k, v)| v.as_array().map(|a| format!("{k} {}", a.len()))).collect();
            for chunk in counts.chunks(4) {
                out.push(chunk.join("  "));
            }
        }
        let s = self.player.stats();
        out.push(format!(
            "nodes {}  seek {:.2} ms  events {}  {}replays {}",
            self.player.scene().len(),
            s.secs * 1e3,
            s.events,
            if s.replayed { "replayed  " } else { "" },
            s.replays
        ));
        out
    }
}

/// Draws frame `t` into the target it is given, at the target's size.
pub type DrawFn = Box<dyn FnMut(f64, &Target)>;
/// Makes the draw function once the window or sheet has a GPU device.
pub type MakeDraw = Box<dyn Fn(&wgpu::Device, &wgpu::Queue) -> DrawFn>;

/// A scene drawn by a function of `t`, for scenes that are not a display list.
pub(crate) struct CustomSource {
    pub size: Size,
    pub duration: f64,
    pub make: MakeDraw,
    draw: Option<DrawFn>,
}

impl CustomSource {
    pub fn new(size: Size, duration: f64, make: MakeDraw) -> CustomSource {
        CustomSource { size, duration, make, draw: None }
    }
}

impl Source for CustomSource {
    fn size(&self) -> Size {
        self.size
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn fonts(&self) -> FontRegistry {
        FontRegistry::new()
    }

    fn event_times(&self) -> Vec<f64> {
        Vec::new()
    }

    fn set_tokens(&mut self, _: Tokens) -> Result<(), String> {
        Err("a custom scene takes no tokens".into())
    }

    fn seek(&mut self, _: f64) {}

    fn seek_stats(&self) -> Option<SeekStats> {
        None
    }

    fn draw(&mut self, t: f64, _: f32, r: &mut Renderer, target: &mut Target) {
        let make = &self.make;
        let draw = self.draw.get_or_insert_with(|| make(r.device(), r.queue()));
        draw(t, target);
    }
}
