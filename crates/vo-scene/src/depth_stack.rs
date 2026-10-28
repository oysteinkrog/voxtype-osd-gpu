//! Depth stack history (MO-86): older lines recede in true depth behind the newest line,
//! stacked like glass sheets in the stage camera (`crate::stage`), with parallax as the
//! stack advances.
//!
//! Each line is a sheet. The newest sheet (depth 0) sits flat and sharp at the front: no
//! turn, no push, full ink, so the reading line keeps its envelope (rule 1). Every older
//! sheet is turned about its vertical centre line, pushed back in z and lifted, and its ink
//! falls with depth. When a new line arrives every sheet moves one place back. Each place
//! further back moves less than the one in front (`DepthStack::parallax`) and on a slower
//! spring, so the sheets slide past each other like glass panes at different distances.
//!
//! Sheets are keyed by the caller's line keys, so a line keeps its node, and its springs,
//! as it moves back. A sheet past `DepthStack::depth` fades out and leaves.
//!
//! Under reduced motion every sheet jumps to its place: the same stack, still.

use vo_core::spring::Spring;

use crate::element::{Element, Key, Motion, Prop, Transition};
use crate::geom::{Color, Size};
use crate::taffy;
use crate::taffy::prelude::{auto, length};

/// How the sheets of a depth stack are placed.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthStack {
    /// Sheets shown behind the newest one.
    pub depth: usize,
    /// How far each place back moves, as a share of the first step: the parallax ratio of
    /// the mock-up. Place `d` (1 is just behind the newest) moves `parallax[d - 1]` of a
    /// full step from place `d - 1`; places past the list keep its last value.
    pub parallax: Vec<f32>,
    /// One full step back: logical pixels pushed away from the viewer.
    pub step_z: f32,
    /// One full step back: logical pixels lifted (positive moves up).
    pub step_rise: f32,
    /// One full step back: logical pixels moved across (positive moves right).
    pub step_x: f32,
    /// The turn of every older sheet about its vertical centre line, in radians. Negative
    /// takes the left edge away, so older lines read as going into the distance on the left.
    pub turn: f32,
    /// Ink of each place back, as a share of full; places past the list keep its last value.
    pub ink: Vec<f32>,
    /// Text blur per place back, in logical units (0 for none), for the content to apply.
    pub blur: f32,
    /// The spring of the front sheet. A place further back takes this much longer per place
    /// (`slowdown`), so deeper sheets lag.
    pub spring_secs: f64,
    pub slowdown: f64,
    /// The glass of a sheet, or `None` for text alone.
    pub sheet: Option<Sheet>,
    /// Frost (EG-04, `Element::frost`) of each sheet, in logical units: the sheets and lines
    /// behind a sheet show blurred through it. 0 for none (the default).
    pub frost: f32,
}

/// The glass pane under a line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sheet {
    pub fill: Color,
    pub edge: Color,
    pub edge_width: f32,
    pub radius: f32,
    /// Room between the pane's edge and its line, across and down.
    pub pad: (f32, f32),
}

impl Default for DepthStack {
    /// The mock-up's numbers: four sheets, parallax 1.0, 0.72, 0.48, 0.28, 220 ms steps.
    fn default() -> DepthStack {
        DepthStack {
            depth: 3,
            parallax: vec![1.0, 0.72, 0.48, 0.28],
            step_z: 110.0,
            step_rise: 46.0,
            step_x: 16.0,
            turn: -0.7,
            ink: vec![0.62, 0.42, 0.26],
            blur: 0.5,
            spring_secs: 0.22,
            slowdown: 0.05,
            sheet: None,
            frost: 0.0,
        }
    }
}

/// Where a sheet sits at a place in the stack.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// Pushed back in z (negative is away from the viewer).
    pub z: f32,
    /// Offset from the newest sheet's box, y down.
    pub offset: (f32, f32),
    pub turn: f32,
    pub ink: f32,
    pub blur: f32,
}

impl DepthStack {
    /// The sum of the parallax steps up to place `d`.
    fn reach(&self, d: usize) -> f32 {
        (1..=d).map(|k| self.ratio(&self.parallax, k)).sum()
    }

    fn ratio(&self, list: &[f32], d: usize) -> f32 {
        match list {
            [] => 1.0,
            l => l[(d - 1).min(l.len() - 1)],
        }
    }

    /// Where a sheet `d` places behind the newest sits. Place 0 is the reading line: flat,
    /// unturned, full ink.
    pub fn place(&self, d: usize) -> Place {
        if d == 0 {
            return Place { z: 0.0, offset: (0.0, 0.0), turn: 0.0, ink: 1.0, blur: 0.0 };
        }
        let r = self.reach(d);
        let ink = if d > self.depth { 0.0 } else { self.ratio(&self.ink, d) };
        Place {
            z: -self.step_z * r,
            offset: (self.step_x * r, -self.step_rise * r),
            turn: self.turn,
            ink,
            blur: self.blur * d as f32,
        }
    }

    /// The spring that moves a sheet to place `d`.
    pub fn spring(&self, d: usize) -> Spring {
        Spring::critical_to_98(self.spring_secs + self.slowdown * d as f64)
    }

    /// How a sheet moves to place `d`: its spring, or a jump under reduced motion. Give the
    /// same motion to a text blur the content sets from [`Place::blur`] (`Prop::TextFx`).
    pub fn motion(&self, d: usize, reduced: bool) -> Motion {
        if reduced {
            Motion::Snap
        } else {
            Motion::Spring(self.spring(d))
        }
    }

    /// The stack in a box `size` big, the newest sheet `sheet` big at its bottom left.
    /// `lines` are `(key, line)`, oldest first; the last is the reading line. `content`
    /// builds a line's sheet content for its place, so it can blur older text by
    /// [`Place::blur`]. Only the newest `depth + 1` lines are kept, plus one more that fades
    /// out as it leaves.
    pub fn view<L, M>(
        &self,
        key: impl Into<Key>,
        size: Size,
        sheet: Size,
        lines: &[(Key, L)],
        content: impl Fn(&L, usize, &Place) -> Element<M>,
        reduced: bool,
    ) -> Element<M> {
        let n = lines.len();
        let keep = self.depth + 2;
        let mut root = Element::layer(key).size(size.w, size.h);
        for (i, (k, line)) in lines.iter().enumerate() {
            let d = n - 1 - i;
            if d >= keep {
                continue;
            }
            let body = content(line, d, &self.place(d));
            root = root.child(self.sheet_view(k.clone(), d, size, sheet, body, reduced));
        }
        root
    }

    fn sheet_view<M>(
        &self,
        key: Key,
        d: usize,
        size: Size,
        sheet: Size,
        content: Element<M>,
        reduced: bool,
    ) -> Element<M> {
        let p = self.place(d);
        let motion = self.motion(d, reduced);
        let pane = match self.sheet {
            Some(s) => Element::panel(format!("{}-pane", key.0))
                .size(sheet.w, sheet.h)
                .radius(s.radius)
                .fill(s.fill)
                .border(s.edge, s.edge_width)
                .pad_xy(s.pad.0, s.pad.1),
            None => Element::column(format!("{}-pane", key.0)).size(sheet.w, sheet.h),
        };
        let pane = pane.child(content);
        let mut layer = Element::layer(key);
        if self.frost > 0.0 {
            // Cut to the pane's rounded box.
            layer = layer.frost(self.frost).radius(self.sheet.map_or(0.0, |s| s.radius));
        }
        let layer = layer
            .size(sheet.w, sheet.h)
            .layout(|s| {
                s.position = taffy::Position::Absolute;
                s.inset =
                    taffy::Rect { left: length(0.0), top: length(size.h - sheet.h), right: auto(), bottom: auto() };
            })
            .translate_z(p.z)
            .rotate_y(p.turn)
            .offset(p.offset.0, p.offset.1)
            .opacity(p.ink)
            .motion(Prop::Tilt, motion)
            .motion(Prop::Offset, motion)
            .motion(Prop::Opacity, motion)
            .child(pane);
        // A new reading line is readable in the frame it arrives: it rises at most 3 px
        // and starts at 70 % ink (rule 1).
        let enter = if reduced { Transition::NONE } else { Transition::fade().opacity(0.7).offset(0.0, 3.0) };
        layer.presence(enter, Transition::fade())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scene;

    fn lines(keys: &[&'static str]) -> Vec<(Key, &'static str)> {
        keys.iter().map(|&k| (Key::from(k), k)).collect()
    }

    fn view(s: &DepthStack, keys: &[&'static str], reduced: bool) -> Element<()> {
        let content = |k: &&str, _: usize, _: &Place| Element::column(format!("{k}-text")).size(200.0, 30.0);
        s.view("stack", Size::new(400.0, 200.0), Size::new(300.0, 40.0), &lines(keys), content, reduced)
    }

    #[test]
    fn the_newest_sheet_is_flat_and_full_ink() {
        let s = DepthStack::default();
        let p = s.place(0);
        assert_eq!((p.z, p.offset, p.turn, p.ink, p.blur), (0.0, (0.0, 0.0), 0.0, 1.0, 0.0));
    }

    #[test]
    fn each_place_back_moves_less_than_the_one_in_front() {
        let s = DepthStack::default();
        let steps: Vec<f32> = (1..=4).map(|d| s.place(d - 1).z - s.place(d).z).collect();
        assert!(steps.windows(2).all(|w| w[0] > w[1]), "{steps:?}");
        assert!((steps[0] / steps[1] - 1.0 / 0.72).abs() < 1e-4);
        let inks: Vec<f32> = (0..=3).map(|d| s.place(d).ink).collect();
        assert!(inks.windows(2).all(|w| w[0] > w[1]), "{inks:?}");
        assert_eq!(s.place(4).ink, 0.0, "past the depth a sheet fades out");
        assert!(s.spring(3).omega() < s.spring(0).omega(), "deeper sheets lag");
    }

    #[test]
    fn a_new_line_moves_every_sheet_one_place_back() {
        let s = DepthStack::default();
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
        scene.apply(view(&s, &["a", "b"], false), 0.0);
        scene.sample(0.0);
        scene.apply(view(&s, &["a", "b", "c"], false), 1.0);
        let list = scene.sample(3.0);
        // The newest sheet has no warp; the two behind it are turned and pushed back.
        let warped = list.layers.iter().filter(|l| l.warp.is_some()).count();
        assert_eq!(warped, 2, "{:?}", list.layers.iter().map(|l| l.warp.is_some()).collect::<Vec<_>>());
        let ink = |k: &str| scene.sampled(scene.find(&["stack", k]).unwrap(), 3.0).unwrap().opacity;
        assert!((ink("c") - 1.0).abs() < 1e-3);
        assert!((ink("b") - s.place(1).ink).abs() < 1e-3);
        assert!((ink("a") - s.place(2).ink).abs() < 1e-3);
    }

    #[test]
    fn reduced_motion_jumps_to_place() {
        let s = DepthStack::default();
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
        scene.apply(view(&s, &["a"], true), 0.0);
        scene.sample(0.0);
        scene.apply(view(&s, &["a", "b"], true), 1.0);
        scene.sample(1.0);
        let ink = scene.sampled(scene.find(&["stack", "a"]).unwrap(), 1.0).unwrap().opacity;
        assert!((ink - s.place(1).ink).abs() < 1e-4, "{ink}");
    }
}
