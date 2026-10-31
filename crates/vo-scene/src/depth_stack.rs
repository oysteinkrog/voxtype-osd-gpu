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
//! A line that has just left the reading place can be held flat for a moment ([`Hold`])
//! before it moves back: on the row it was read on when the box grew by a line, or gliding
//! one row up, like a plain scroll, when it did not. Without the hold, in a box that grows
//! the line lands back almost where it was read, so a reader (and the envelope check, MO-08)
//! takes that turned, pushed-back spot for where the line came to rest; in a box that keeps
//! its size, the line turns while it still sits on the new line's row.
//!
//! While a held line glides up out of the new line's row, the new reading line waits, then
//! shows whole in one frame ([`DepthStack::newest_shows_at`]): text never draws over text,
//! and the reading line is readable in the frame it arrives. The host views the stack again
//! by then.
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

/// A line that has just left the reading place, held flat for a moment before it moves back.
#[derive(Clone, Debug, PartialEq)]
pub struct Hold {
    /// The line's key, as given to [`DepthStack::view_at`].
    pub key: Key,
    /// When it left the reading place, on the scene clock.
    pub since: f64,
    /// How long it is held, in seconds.
    pub secs: f64,
    /// How far it is held above the newest sheet's box, in logical pixels: one line pitch.
    pub rise: f32,
    /// False when the box grew by `rise` as the new line arrived: the line jumps with its box
    /// and so stays where it was read. True when the box kept its size: the line glides up
    /// one row on the newest sheet's spring, as a plain scroll would, also under reduced
    /// motion.
    pub glide: bool,
}

impl Hold {
    /// Whether the line is held at time `now`: from `since` for `secs`.
    pub fn holds(&self, now: f64) -> bool {
        now >= self.since && now < self.until()
    }

    /// When the line moves back. Show the stack again at this time.
    pub fn until(&self) -> f64 {
        self.since + self.secs
    }
}

/// Share of a row a gliding held line has risen when the new reading line shows: its text
/// is then clear of the new line's.
const GLIDE_CLEAR: f64 = 0.9;

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

    /// Where a held line sits: as the reading line does, `rise` above the newest sheet's box.
    pub fn held_place(&self, hold: &Hold) -> Place {
        Place { offset: (0.0, -hold.rise), ..self.place(0) }
    }

    /// When the newest sheet shows with `hold` on the line behind it: once a gliding line has
    /// risen out of its row (`GLIDE_CLEAR` of the way on the newest sheet's spring), or at
    /// once when the box grew and the held line stays where it was read.
    pub fn newest_shows_at(&self, hold: &Hold) -> f64 {
        if !hold.glide {
            return hold.since;
        }
        let mut ch = vo_core::channel::Channel::new(0.0, 0.0, self.spring(0));
        ch.retarget(0.0, 1.0);
        let mut t = 0.0;
        while t < 1.0 && ch.value(t) < GLIDE_CLEAR {
            t += 1.0 / 480.0;
        }
        hold.since + t
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
        self.view_at(key, size, sheet, lines, content, reduced, None, 0.0)
    }

    /// [`DepthStack::view`] at time `now`, with `hold` keeping the line just behind the
    /// newest flat while [`Hold::holds`]. Its content is built for place 1
    /// with the held [`Place`]. A hold on any other line does nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn view_at<L, M>(
        &self,
        key: impl Into<Key>,
        size: Size,
        sheet: Size,
        lines: &[(Key, L)],
        content: impl Fn(&L, usize, &Place) -> Element<M>,
        reduced: bool,
        hold: Option<&Hold>,
        now: f64,
    ) -> Element<M> {
        let n = lines.len();
        let keep = self.depth + 2;
        // A hold on the line just behind the newest: when the newest sheet shows.
        let behind = n.checked_sub(2).map(|i| &lines[i].0);
        let shows = hold.filter(|h| behind == Some(&h.key)).map(|h| self.newest_shows_at(h));
        let mut root = Element::layer(key).size(size.w, size.h);
        for (i, (k, line)) in lines.iter().enumerate() {
            let d = n - 1 - i;
            if d >= keep {
                continue;
            }
            let held = hold.filter(|h| d == 1 && h.key == *k && h.holds(now));
            let place = held.map_or_else(|| self.place(d), |h| self.held_place(h));
            let body = content(line, d, &place);
            let waits = shows.filter(|_| d == 0).map(|at| now < at);
            root = root.child(self.sheet_view(k.clone(), d, size, sheet, body, reduced, held, waits));
        }
        root
    }

    #[allow(clippy::too_many_arguments)]
    fn sheet_view<M>(
        &self,
        key: Key,
        d: usize,
        size: Size,
        sheet: Size,
        content: Element<M>,
        reduced: bool,
        held: Option<&Hold>,
        waits: Option<bool>,
    ) -> Element<M> {
        let p = held.map_or_else(|| self.place(d), |h| self.held_place(h));
        // The newest sheet while a line glides out of its row: clear, then whole in one step.
        let ink = if waits == Some(true) { 0.0 } else { p.ink };
        let motion = match held {
            // It jumps with its box, so it stays where it was read.
            Some(h) if !h.glide => Motion::Snap,
            Some(_) => Motion::Spring(self.spring(0)),
            None => self.motion(d, reduced),
        };
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
            .opacity(ink)
            .motion(Prop::Tilt, motion)
            .motion(Prop::Offset, motion)
            .motion(Prop::Opacity, if waits.is_some() { Motion::Snap } else { motion })
            .child(pane);
        let layer = if held.is_some() { layer.motion(Prop::Position, motion) } else { layer };
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

    fn hold(key: &'static str, since: f64, secs: f64, glide: bool) -> Hold {
        Hold { key: Key::from(key), since, secs, rise: 40.0, glide }
    }

    /// The stack `rows` lines of 40 px tall at `now`.
    fn view_held(s: &DepthStack, keys: &[&'static str], rows: f32, hold: Option<&Hold>, now: f64) -> Element<()> {
        let content = |k: &&str, _: usize, _: &Place| Element::column(format!("{k}-text")).size(200.0, 30.0);
        let size = Size::new(400.0, 40.0 * rows);
        s.view_at("stack", size, Size::new(300.0, 40.0), &lines(keys), content, false, hold, now)
    }

    /// Where sheet `k` is drawn at `t`, its ink, and how many sheets are turned.
    fn at(scene: &Scene<()>, k: &str, t: f64) -> ((f32, f32), f32, usize) {
        let warped = scene.sample(t).layers.iter().filter(|l| l.warp.is_some()).count();
        let n = scene.sampled(scene.find(&["stack", k]).unwrap(), t).unwrap();
        ((n.transform.e, n.transform.f), n.opacity, warped)
    }

    #[test]
    fn a_hold_is_pure_in_time() {
        let h = hold("a", 1.0, 0.15, false);
        assert!(!h.holds(0.99));
        assert!(h.holds(1.0) && h.holds(1.149));
        assert!(!h.holds(1.15) && !h.holds(2.0));
        assert_eq!(h.until(), 1.15);
        let p = DepthStack::default().held_place(&h);
        assert_eq!((p.z, p.offset, p.turn, p.ink, p.blur), (0.0, (0.0, -40.0), 0.0, 1.0, 0.0));
    }

    #[test]
    fn a_held_line_stays_where_it_was_read_while_the_box_grows() {
        let s = DepthStack::default();
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
        scene.apply(view_held(&s, &["a"], 1.0, None, 0.0), 0.0);
        let (read, _, _) = at(&scene, "a", 0.5);
        // The box grows by a line as "b" arrives; "a" is held where it was read.
        let h = hold("a", 1.0, 0.15, false);
        scene.apply(view_held(&s, &["a", "b"], 2.0, Some(&h), 1.0), 1.0);
        for t in [1.0, 1.05, 1.1, 1.149] {
            let (pos, ink, warped) = at(&scene, "a", t);
            assert!((pos.0 - read.0).abs() < 1e-3 && (pos.1 - read.1).abs() < 1e-3, "{t}: {pos:?} {read:?}");
            assert!((ink - 1.0).abs() < 1e-4 && warped == 0, "{t}: ink {ink}, {warped} turned");
        }
        // Then it moves back.
        scene.apply(view_held(&s, &["a", "b"], 2.0, Some(&h), h.until()), h.until());
        let (_, ink, warped) = at(&scene, "a", 3.0);
        assert!((ink - s.place(1).ink).abs() < 1e-3 && warped == 1, "ink {ink}, {warped} turned");
    }

    #[test]
    fn a_held_line_glides_up_a_row_flat_when_the_box_keeps_its_size() {
        let s = DepthStack::default();
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
        scene.apply(view_held(&s, &["a"], 2.0, None, 0.0), 0.0);
        let (read, _, _) = at(&scene, "a", 0.5);
        let h = hold("a", 1.0, 0.6, true);
        scene.apply(view_held(&s, &["a", "b"], 2.0, Some(&h), 1.0), 1.0);
        let mut last = read.1;
        for t in [1.05, 1.1, 1.2, 1.3, 1.59] {
            let (pos, ink, warped) = at(&scene, "a", t);
            assert!(pos.1 < last && (pos.0 - read.0).abs() < 1e-3, "{t}: rises straight up, {pos:?}");
            assert!((ink - 1.0).abs() < 1e-4 && warped == 0, "{t}: flat, ink {ink}, {warped} turned");
            last = pos.1;
        }
        assert!((last - (read.1 - 40.0)).abs() < 0.5, "a row up: {last} from {}", read.1);
    }

    #[test]
    fn the_new_line_waits_while_the_held_line_glides_out_of_its_row() {
        // A full box: "a" glides up a row as "b" arrives on its row. "b" shows once "a" is
        // nearly a row up, whole in one step, and never while the two rows overlap.
        let s = DepthStack::default();
        let mut scene: Scene<()> = Scene::new();
        scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
        scene.apply(view_held(&s, &["a"], 2.0, None, 0.0), 0.0);
        let (read, _, _) = at(&scene, "a", 0.5);
        let h = hold("a", 1.0, 0.05, true);
        let shows = s.newest_shows_at(&h);
        assert!(shows > 1.05 && shows < 1.25, "shows at {shows}");
        let mut t = 1.0;
        while t < 1.6 {
            scene.apply(view_held(&s, &["a", "b"], 2.0, Some(&h), t), t);
            let (b, ink, _) = at(&scene, "b", t);
            let (a, _, _) = at(&scene, "a", t);
            if t < shows {
                assert_eq!(ink, 0.0, "at {t} the new line waits");
            } else {
                assert!((ink - 1.0).abs() < 1e-4, "at {t} the new line is whole: {ink}");
                assert!(b.1 - a.1 >= 0.9 * 40.0 - 0.5, "at {t} a is {} above b", b.1 - a.1);
            }
            assert!((b.1 - read.1).abs() < 3.5, "at {t} the reading line stays: {b:?}");
            t += 1.0 / 60.0;
        }
        // The box grew: the held line stays where it was read and the new one shows at once.
        assert_eq!(s.newest_shows_at(&hold("a", 1.0, 0.35, false)), 1.0);
    }

    #[test]
    fn a_hold_on_another_line_or_after_its_time_does_nothing() {
        let s = DepthStack::default();
        let plain = |h: Option<&Hold>, now: f64| {
            let mut scene: Scene<()> = Scene::new();
            scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
            scene.apply(view_held(&s, &["a", "b", "c"], 2.0, h, now), 0.0);
            ["a", "b", "c"].map(|k| at(&scene, k, 2.0))
        };
        let none = plain(None, 1.0);
        assert_eq!(plain(Some(&hold("a", 1.0, 0.15, false)), 1.0), none, "a is two places back");
        assert_eq!(plain(Some(&hold("b", 0.5, 0.15, false)), 1.0), none, "the hold is over");
        assert_ne!(plain(Some(&hold("b", 1.0, 0.15, false)), 1.0), none, "b is held");
    }
}
