//! A bar-strip waveform over the audio level history (`Element::bars`): one round-capped bar
//! per time slot, newest on the right, scrolling left.
//!
//! The level is smoothed by an attack and release follower (60 ms and 200 ms time constants
//! by default), so bars rise fast and fall slowly. When recording stops, `settle` brings every
//! bar down to a dot over 400 ms, which leaves a flat dotted line. A `playhead` splits the
//! strip into an accent part on the left and a grey part on the right, inside a bar if it falls
//! there.
//!
//! Like `Wave`, it is a pure function of `t` and its fields. Bars sit on a grid of slot times
//! counted from 0, so a bar keeps its height while it scrolls. The follower runs over the
//! whole level history each frame and starts from the oldest level, so keep about a second of
//! history beyond what the strip shows.

use std::sync::Arc;

use crate::display::{Paint, ShapeInstance};
use crate::geom::{Color, Point, RRect, Rect};

/// See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Bars {
    /// Levels from 0 to 1, oldest first, evenly spaced in time.
    pub levels: Arc<[f32]>,
    /// Scene time of the newest level. The strip scrolls until then and stays still after.
    pub newest_at: f64,
    /// Levels per second.
    pub rate: f32,
    /// Bars across the box.
    pub count: u32,
    /// Seconds of history per bar.
    pub slot: f32,
    /// Bar width as a fraction of the bar pitch.
    pub width: f32,
    /// Follower time constants in seconds: rising and falling.
    pub attack: f32,
    pub release: f32,
    /// From this time on, heights fall to dots over `settle_secs`.
    pub settle: Option<f64>,
    pub settle_secs: f32,
    /// Where the colour changes, 0 to 1 across the box: `accent` on the left, `rest` on the
    /// right. `None` draws every bar in `accent`.
    pub playhead: Option<f32>,
    pub accent: Color,
    pub rest: Color,
}

impl Default for Bars {
    fn default() -> Bars {
        Bars {
            levels: Arc::from([]),
            newest_at: 0.0,
            rate: 100.0,
            count: 32,
            slot: 0.06,
            width: 0.55,
            attack: 0.060,
            release: 0.200,
            settle: None,
            settle_secs: 0.4,
            playhead: None,
            accent: Color::WHITE,
            rest: Color::hex(0x6b7280, 1.0),
        }
    }
}

impl Bars {
    /// Whether the strip still changes at `t`: while it scrolls or settles.
    pub fn moving(&self, t: f64) -> bool {
        (self.levels.len() > 1 && t < self.newest_at) || self.settle.is_some_and(|s| t < s + self.settle_secs as f64)
    }

    /// The height multiplier at `t`: 1, falling smoothly to 0 after `settle`.
    pub fn envelope(&self, t: f64) -> f32 {
        let Some(s) = self.settle else { return 1.0 };
        let u = ((t - s) / self.settle_secs.max(1e-3) as f64).clamp(0.0, 1.0) as f32;
        1.0 - u * u * (3.0 - 2.0 * u)
    }

    /// The followed level at every input level.
    pub fn followed(&self) -> Vec<f32> {
        let dt = 1.0 / self.rate.max(1e-3);
        let up = 1.0 - (-dt / self.attack.max(1e-4)).exp();
        let down = 1.0 - (-dt / self.release.max(1e-4)).exp();
        let mut y = self.levels.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
        self.levels
            .iter()
            .map(|&x| {
                let x = x.clamp(0.0, 1.0);
                y += (x - y) * if x > y { up } else { down };
                y
            })
            .collect()
    }

    /// The bars for `rect` at `t`, with `alpha` multiplied in.
    pub fn shapes(&self, rect: Rect, t: f64, alpha: f32, out: &mut Vec<ShapeInstance>) {
        let n = self.levels.len();
        if self.count == 0 || self.slot <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let pitch = rect.w / self.count as f32;
        let bar_w = (pitch * self.width).clamp(0.5, rect.h);
        let r = 0.5 * bar_w;
        let mid = rect.y + 0.5 * rect.h;
        let max_half = 0.5 * rect.h;
        let env = self.envelope(t);
        let followed = self.followed();
        let first_at = self.newest_at - n.saturating_sub(1) as f64 / self.rate as f64;
        let level_at = |at: f64| -> f32 {
            if n == 0 || at < first_at - 1e-9 {
                return 0.0;
            }
            let f = ((at - first_at) * self.rate as f64).min((n - 1) as f64);
            let i = f.floor() as usize;
            let k = (f - i as f64) as f32;
            let a = followed[i];
            let b = followed[(i + 1).min(n - 1)];
            a + (b - a) * k
        };
        let edge = t.min(self.newest_at);
        let slot = self.slot as f64;
        let px_per_s = pitch as f64 / slot;
        let newest_slot = (edge / slot).floor();
        let split = self.playhead.map(|p| rect.x + p.clamp(0.0, 1.0) * rect.w);
        // One extra slot on the left, fading out as it scrolls past the edge.
        for j in 0..=self.count as i64 {
            let at = (newest_slot - j as f64) * slot;
            let cx = rect.right() as f64 - 0.5 * pitch as f64 - (edge - at) * px_per_s;
            let cx = cx as f32;
            let fade = ((cx - rect.x) / (0.5 * pitch)).clamp(0.0, 1.0);
            if fade <= 0.0 {
                continue;
            }
            let half = (env * level_at(at) * max_half).max(r);
            let (x0, x1) = (cx - r, cx + r);
            let paint = match split {
                Some(s) if x1 <= s => Paint::solid(self.accent.fade(alpha * fade)),
                Some(s) if x0 >= s => Paint::solid(self.rest.fade(alpha * fade)),
                Some(s) => Paint::linear(
                    Point::new(s - 0.5, mid),
                    self.accent.fade(alpha * fade),
                    Point::new(s + 0.5, mid),
                    self.rest.fade(alpha * fade),
                ),
                None => Paint::solid(self.accent.fade(alpha * fade)),
            };
            out.push(ShapeInstance::rrect(RRect::new(Rect::new(x0, mid - half, bar_w, 2.0 * half), r), paint));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(levels: &[f32], newest_at: f64) -> Bars {
        Bars { levels: Arc::from(levels), newest_at, count: 10, slot: 0.1, width: 0.5, ..Bars::default() }
    }

    fn shapes(b: &Bars, t: f64) -> Vec<ShapeInstance> {
        let mut out = Vec::new();
        b.shapes(Rect::new(0.0, 0.0, 100.0, 40.0), t, 1.0, &mut out);
        out
    }

    #[test]
    fn the_follower_rises_fast_and_falls_slowly() {
        // A one-second step from 0 to 1 and back, at 100 levels a second.
        let mut levels = vec![0.0f32; 100];
        levels.extend([1.0; 100]);
        levels.extend([0.0; 100]);
        let b = Bars { levels: Arc::from(levels.as_slice()), ..Bars::default() };
        let y = b.followed();
        // One time constant after each edge: 63% of the way.
        assert!((y[100 + 5] - (1.0 - (-1.0f32).exp())).abs() < 0.03, "attack {}", y[105]);
        assert!((y[200 + 19] - (-1.0f32).exp()).abs() < 0.03, "release {}", y[219]);
        assert!(y[180] > 0.99);
    }

    #[test]
    fn bars_keep_their_height_while_they_scroll() {
        let levels: Vec<f32> = (0..400).map(|i| (i as f32 * 0.37).sin() * 0.5 + 0.5).collect();
        let b = bars(&levels, 4.0);
        // A whole slot later every bar has moved one pitch left with the same height.
        let a = shapes(&b, 3.0);
        let c = shapes(&b, 3.1);
        for (x, y) in a.iter().zip(c.iter().skip(1)) {
            assert!((x.geom[0] - 10.0 - y.geom[0]).abs() < 1e-3, "{:?} {:?}", x.geom, y.geom);
            assert!((x.geom[3] - y.geom[3]).abs() < 1e-4);
        }
        assert_eq!(shapes(&b, 4.0), shapes(&b, 9.0), "still after the newest level");
        assert!(b.moving(3.9) && !b.moving(4.0));
    }

    #[test]
    fn settle_brings_every_bar_down_to_a_dot() {
        let b = Bars { settle: Some(1.0), ..bars(&[0.8; 300], 1.0) };
        assert!(b.moving(1.2) && !b.moving(1.41));
        assert_eq!(b.envelope(0.9), 1.0);
        for s in shapes(&b, 1.41) {
            // Five units wide, so a dot is five units tall.
            assert!((s.geom[3] - 5.0).abs() < 1e-4, "{:?}", s.geom);
        }
        assert!(shapes(&b, 1.1).iter().any(|s| s.geom[3] > 10.0));
    }

    #[test]
    fn the_playhead_splits_the_colour() {
        let accent = Color::hex(0x22c55e, 1.0);
        let b = Bars { playhead: Some(0.4), accent, ..bars(&[0.5; 300], 2.95) };
        let out = shapes(&b, 2.95);
        let split = 40.0;
        let mut seen_split = false;
        for s in out.iter().filter(|s| s.fill.c0.a > 0.99) {
            let (x0, x1) = (s.geom[0], s.geom[0] + s.geom[2]);
            if x1 <= split {
                assert_eq!(s.fill, Paint::solid(accent));
            } else if x0 >= split {
                assert_eq!(s.fill, Paint::solid(b.rest));
            } else {
                seen_split = true;
                assert_ne!(s.fill, Paint::solid(accent));
            }
        }
        assert!(seen_split, "40% falls inside a bar");
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;
    use crate::element::Element;
    use crate::scene::Scene;

    #[test]
    fn bars_keep_the_scene_awake_until_they_settle() {
        let mut scene: Scene = Scene::new();
        let b =
            Bars { levels: Arc::from([0.5f32; 300].as_slice()), newest_at: 3.0, settle: Some(3.0), ..Bars::default() };
        scene.apply(Element::column("root").child(Element::bars("b", b).size(160.0, 30.0)), 1.0);
        assert_eq!(scene.next_wake(2.0), Some(2.0));
        assert_eq!(scene.next_wake(3.2), Some(3.2));
        assert_eq!(scene.next_wake(3.5), None);
        assert!(!scene.sample(2.0).shapes.is_empty());
        assert_ne!(scene.sample(3.1), scene.sample(3.3));
    }
}
