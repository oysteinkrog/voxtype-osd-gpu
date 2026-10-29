//! Extra surfaces: one app driving several surfaces at once (FX-193), such as a card and a
//! ticker bar below it, or a popup over a fullscreen scrim (FX-133, FX-194).
//!
//! `App::surfaces` returns a [`SurfaceView`] per extra surface, after every update, next to
//! the main `view`. Each key gets its own retained scene, so each surface animates and draws
//! on its own: a settled fullscreen scrim costs no frames while a ticker beside it updates.
//! All surfaces share the app's one `update` and its timers, and pointer events on any of them
//! become messages to the same `update`.
//!
//! A surface opens when its key first appears. When its key goes away, its view's nodes leave
//! with their presence motion, and the surface closes once its scene has settled. Bring the
//! key back before that and the surface stays.
//!
//! A fly-out ([`SurfaceView::fly_out`], EG-08) is an extra surface for one big exit, such as
//! a card flying to the app or a long streak: a short-lived full-screen click-through
//! surface on the popup's output, with the view put where the main panel sits on screen. It
//! closes when its view settles or `EXIT_MOTION_MAX` after it opened, whichever is first,
//! and stays closed while the app keeps returning its key. Under reduced motion no fly-out
//! opens: the app keeps the element in its main view and lets it fade in place.

use vo_host::{InputMode, OverlaySpec, Placement, Stack};
use vo_scene::element::{Element, Motion, Prop};
use vo_scene::geom::{Point, Size};
use vo_scene::taffy::style::{Dimension, LengthPercentageAuto, Position};
use vo_scene::Scene;

use crate::app::EXIT_MOTION_MAX;

/// The internal root around every extra surface's view, so a surface whose key went away
/// can let the view leave (`presence` exit motion) and still have a root.
const ROOT: &str = "vo-surface";

/// The internal root: the whole viewport, whatever size the compositor gave.
fn root<M>() -> Element<M> {
    Element::column(ROOT).layout(|s| {
        s.size.width = Dimension::percent(1.0);
        s.size.height = Dimension::percent(1.0);
    })
}

/// The internal box that puts a fly-out's view at the main panel's place.
const FLY_OUT: &str = "vo-fly-out";

/// One extra surface of an app: what it is and what it shows.
pub struct SurfaceView<M> {
    /// Stays the same for the life of the surface. Two views with one key are one surface.
    pub key: String,
    /// Read when the surface opens. Later changes are ignored; use a new key to reopen.
    pub spec: OverlaySpec,
    /// The surface's view. Its root fills the surface, except on a fly-out.
    pub view: Element<M>,
    /// A fly-out: the view's top-left, from the main panel's top-left (`fly_out`).
    pub fly_out: Option<Point>,
}

impl<M> SurfaceView<M> {
    pub fn new(key: impl Into<String>, spec: OverlaySpec, view: Element<M>) -> SurfaceView<M> {
        SurfaceView { key: key.into(), spec, view, fly_out: None }
    }

    /// A fly-out for a big exit: `view` drawn on a full-screen click-through surface, with its
    /// top-left `at` from the main panel's top-left, so it starts where the element sat in
    /// the panel and can fly anywhere on the output. Return it from `App::surfaces` while the
    /// element flies, and take the element out of the main view at the same message. The
    /// surface closes once the view settles, or `EXIT_MOTION_MAX` (3 s) after it opened, so
    /// the view should end out of sight: faded out, or off the output.
    /// Under reduced motion it never opens: keep the element in the main view and let it fade.
    pub fn fly_out(key: impl Into<String>, at: Point, view: Element<M>) -> SurfaceView<M> {
        let spec = OverlaySpec {
            // A first guess; the compositor's configure brings the output's size.
            size: Size::new(1920.0, 1080.0),
            placement: Placement::Fullscreen,
            input: InputMode::None,
            layer: Stack::Overlay,
            app_id: "vo-fly-out".into(),
            ..OverlaySpec::default()
        };
        SurfaceView { key: key.into(), spec, view, fly_out: Some(at) }
    }
}

/// Where the main panel's top-left sits on its output of size `(W, H)`: at
/// `(fx W + dx, fy H + dy)`. Fractions, so a fly-out's layout follows the output's real size
/// once its surface is configured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PanelAnchor {
    pub(crate) fx: f32,
    pub(crate) fy: f32,
    pub(crate) dx: f32,
    pub(crate) dy: f32,
}

impl PanelAnchor {
    /// For a main surface of `size` at `placement` (both with the margin), whose panel sits
    /// `inset` (left, top) inside it. The host's keep-on-the-output clamp is not modelled.
    pub(crate) fn of(placement: Placement, size: Size, inset: Point) -> PanelAnchor {
        let centred = |len: f32, inset: f32| (0.5, -0.5 * len + inset);
        let axis = |near: bool, far: bool, m_near: i32, m_far: i32, len: f32, inset: f32| match (near, far) {
            (true, false) => (0.0, m_near as f32 + inset),
            (false, true) => (1.0, -(m_far as f32) - len + inset),
            _ => centred(len, inset),
        };
        let ((fx, dx), (fy, dy)) = match placement {
            Placement::Centered => (centred(size.w, inset.x), centred(size.h, inset.y)),
            Placement::Anchored { edges, margin } => (
                axis(edges.left, edges.right, margin[3], margin[1], size.w, inset.x),
                axis(edges.top, edges.bottom, margin[0], margin[2], size.h, inset.y),
            ),
            Placement::FromTop { fraction } => (centred(size.w, inset.x), (fraction.clamp(0.0, 1.0), inset.y)),
            Placement::LineAt { fraction, y } => (centred(size.w, inset.x), (fraction.clamp(0.0, 1.0), inset.y - y)),
            Placement::Fullscreen => ((0.0, inset.x), (0.0, inset.y)),
        };
        PanelAnchor { fx, fy, dx, dy }
    }

    /// The box that puts a fly-out's view at the panel's place plus `at`.
    fn place<M>(&self, at: Point, view: Element<M>) -> Element<M> {
        Element::column(FLY_OUT)
            .layout(|s| {
                s.position = Position::Absolute;
                s.inset.left = LengthPercentageAuto::percent(self.fx);
                s.inset.top = LengthPercentageAuto::percent(self.fy);
                s.margin.left = LengthPercentageAuto::length(self.dx + at.x);
                s.margin.top = LengthPercentageAuto::length(self.dy + at.y);
            })
            // A new anchor (the main surface moved) is a jump, not a slide.
            .motion(Prop::Position, Motion::Snap)
            .child(view)
    }
}

/// An extra surface's scene and state, kept by the driver.
pub(crate) struct Extra<M> {
    pub(crate) key: String,
    pub(crate) spec: OverlaySpec,
    pub(crate) scene: Scene<M>,
    /// The scene changed since this surface's last frame.
    pub(crate) dirty: bool,
    /// The app no longer returns this key: close once settled.
    pub(crate) closing: bool,
    /// A fly-out: when it opened. It closes once settled, or `EXIT_MOTION_MAX` after this.
    pub(crate) fly_out: Option<f64>,
}

/// Makes the scene for a new extra surface, with the app's text layout and motion settings.
pub(crate) type MakeScene<M> = Box<dyn Fn() -> Scene<M>>;

/// Every extra surface of an app, in the order the app first returned them.
pub(crate) struct Extras<M> {
    pub(crate) list: Vec<Extra<M>>,
    make: MakeScene<M>,
    /// Where the main panel sits on its output, for fly-outs. The live overlay keeps it up to
    /// date; offline it stays at the output's top-left.
    pub(crate) anchor: PanelAnchor,
    /// Reduced motion: fly-outs never open.
    pub(crate) reduced: bool,
    /// Fly-out keys that closed while the app still returned them: they stay closed until
    /// the app stops returning the key.
    spent: Vec<String>,
}

impl<M: Clone + 'static> Extras<M> {
    pub(crate) fn new(make: MakeScene<M>) -> Extras<M> {
        Extras { list: Vec::new(), make, anchor: PanelAnchor::default(), reduced: false, spent: Vec::new() }
    }

    /// Applies the app's surface views at `t`: new keys get a scene, gone keys start closing.
    pub(crate) fn apply(&mut self, views: Vec<SurfaceView<M>>, t: f64) {
        for e in &mut self.list {
            e.closing = true;
        }
        self.spent.retain(|k| views.iter().any(|v| v.fly_out.is_some() && v.key == *k));
        for v in views {
            if v.fly_out.is_some() && (self.reduced || self.spent.contains(&v.key)) {
                continue;
            }
            let root = match v.fly_out {
                Some(at) => root().child(self.anchor.place(at, v.view)),
                None => root().child(v.view.grow(1.0)),
            };
            match self.list.iter_mut().find(|e| e.key == v.key) {
                Some(e) => {
                    e.closing = false;
                    e.dirty |= e.scene.apply(root, t);
                }
                None => {
                    let mut scene = (self.make)();
                    scene.set_viewport(t, v.spec.size, 1.0);
                    scene.apply(root, t);
                    let fly_out = v.fly_out.map(|_| t);
                    self.list.push(Extra { key: v.key, spec: v.spec, scene, dirty: true, closing: false, fly_out });
                }
            }
        }
        for e in self.list.iter_mut().filter(|e| e.closing) {
            e.dirty |= e.scene.apply(root(), t);
        }
    }

    /// Any extra surface moves at `t`.
    pub(crate) fn moving(&self, t: f64) -> bool {
        self.list.iter().any(|e| e.scene.next_wake(t).is_some())
    }

    pub(crate) fn find(&self, key: &str) -> Option<usize> {
        self.list.iter().position(|e| e.key == key)
    }

    /// Removes the closing surfaces that have settled at `t`, and the fly-outs that have
    /// settled or run `EXIT_MOTION_MAX`, and returns their keys.
    pub(crate) fn take_closed(&mut self, t: f64) -> Vec<String> {
        let mut gone = Vec::new();
        let spent = &mut self.spent;
        self.list.retain(|e| {
            let settled = e.scene.next_wake(t).is_none();
            let flown = e.fly_out.is_some_and(|at| settled || t >= at + EXIT_MOTION_MAX);
            let done = (e.closing && settled) || flown;
            if done {
                gone.push(e.key.clone());
                if flown && !e.closing {
                    spent.push(e.key.clone());
                }
            }
            !done
        });
        gone
    }

    /// When the earliest open fly-out reaches its cap, for a wake-up.
    pub(crate) fn fly_out_deadline(&self) -> Option<f64> {
        self.list.iter().filter_map(|e| e.fly_out).map(|at| at + EXIT_MOTION_MAX).min_by(f64::total_cmp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::element::Transition;
    use vo_scene::geom::Color;

    fn spec() -> OverlaySpec {
        OverlaySpec { size: Size::new(200.0, 40.0), ..OverlaySpec::default() }
    }

    fn bar(text: &str) -> Element<()> {
        Element::panel("bar")
            .fill(Color::hex(0x202020, 1.0))
            .presence(Transition::fade(), Transition::fade())
            .child(Element::label("text", text))
    }

    #[test]
    fn opens_updates_and_closes_after_the_exit_motion() {
        let mut x: Extras<()> = Extras::new(Box::new(Scene::new));
        x.apply(vec![SurfaceView::new("ticker", spec(), bar("one"))], 0.0);
        assert_eq!(x.list.len(), 1);
        assert!(x.list[0].dirty);
        x.list[0].dirty = false;

        // A new view on the same key updates that surface's scene only.
        x.apply(vec![SurfaceView::new("ticker", spec(), bar("two"))], 2.0);
        assert_eq!(x.list.len(), 1);
        assert!(x.list[0].dirty);

        // The key goes away: the bar fades out first, then the surface closes.
        x.apply(Vec::new(), 3.0);
        assert!(x.list[0].closing);
        assert!(x.moving(3.01), "the bar is fading out");
        assert!(x.take_closed(3.01).is_empty());
        let settled = (3..200).map(|i| 3.0 + i as f64 * 0.05).find(|&t| !x.moving(t)).expect("settles");
        assert_eq!(x.take_closed(settled), vec!["ticker".to_string()]);
        assert!(x.list.is_empty());
    }

    #[test]
    fn a_key_that_comes_back_before_closing_stays_open() {
        let mut x: Extras<()> = Extras::new(Box::new(Scene::new));
        x.apply(vec![SurfaceView::new("scrim", spec(), bar("a"))], 0.0);
        x.apply(Vec::new(), 1.0);
        x.apply(vec![SurfaceView::new("scrim", spec(), bar("a"))], 1.05);
        assert!(!x.list[0].closing);
        assert!(x.take_closed(10.0).is_empty());
    }

    #[test]
    fn each_new_key_gets_a_scene_from_the_maker() {
        use std::cell::Cell;
        use std::rc::Rc;
        let made = Rc::new(Cell::new(0));
        let count = made.clone();
        let mut x: Extras<()> = Extras::new(Box::new(move || {
            count.set(count.get() + 1);
            Scene::new()
        }));
        x.apply(vec![SurfaceView::new("a", spec(), bar("1")), SurfaceView::new("b", spec(), bar("1"))], 0.0);
        x.apply(vec![SurfaceView::new("a", spec(), bar("2")), SurfaceView::new("b", spec(), bar("2"))], 1.0);
        assert_eq!(made.get(), 2, "one scene per key, made by the app's maker (fonts, motion)");
    }

    const SCREEN: Size = Size::new(1920.0, 1080.0);

    /// A popup 216 x 76 with a margin of 8, its reading line 30 px down at 80% of the output.
    fn popup_anchor() -> PanelAnchor {
        PanelAnchor::of(Placement::LineAt { fraction: 0.8, y: 38.0 }, Size::new(216.0, 76.0), Point::new(8.0, 8.0))
    }

    /// A card that flies 800 px up from where it sat in the panel, then fades.
    fn card(spring: Option<vo_core::spring::Spring>) -> Element<()> {
        let card = Element::panel("card")
            .size(200.0, 60.0)
            .fill(Color::WHITE)
            .offset(0.0, -800.0)
            .presence(Transition::NONE.offset(0.0, 0.0), Transition::fade());
        match spring {
            Some(s) => card.motion(Prop::Offset, s),
            None => card,
        }
    }

    fn fly(spring: Option<vo_core::spring::Spring>) -> SurfaceView<()> {
        SurfaceView::fly_out("fly", Point::new(4.0, 6.0), card(spring))
    }

    fn extras(reduced: bool) -> Extras<()> {
        let mut x: Extras<()> = Extras::new(Box::new(Scene::new));
        x.anchor = popup_anchor();
        x.reduced = reduced;
        x
    }

    #[test]
    fn a_fly_out_starts_at_the_panel_and_is_never_clipped() {
        let mut x = extras(false);
        x.apply(vec![fly(None)], 1.0);
        assert_eq!(x.list.len(), 1, "the fly-out opens");
        let e = &x.list[0];
        assert_eq!(e.spec.placement, Placement::Fullscreen);
        assert_eq!(e.spec.input, InputMode::None, "click-through");
        // The compositor gives the output's size.
        x.list[0].scene.set_viewport(1.0, SCREEN, 1.0);
        let scene = &x.list[0].scene;
        let id = scene.find(&[ROOT, FLY_OUT, "card"]).unwrap();
        let at = scene.sampled(id, 1.0).unwrap().transform.apply(Point::ZERO);
        // Centred across: (1920 - 216) / 2 + 8 + 4; down: 0.8 * 1080 - 38 + 8 + 6.
        assert!((at.x - 864.0).abs() < 0.5 && (at.y - 840.0).abs() < 0.5, "{at:?}");
        let mut t = 1.0;
        let mut top = at.y;
        while x.find("fly").is_some() {
            let scene = &x.list[0].scene;
            let clipped = scene.clipped(t, SCREEN);
            assert!(clipped.is_zero(), "clipped {clipped:?} at {t}");
            top = top.min(scene.sampled(id, t).unwrap().transform.apply(Point::ZERO).y);
            x.apply(vec![fly(None)], t);
            x.take_closed(t);
            t += 1.0 / 60.0;
        }
        assert!(top < at.y - 790.0, "it flew up: {top}");
        assert!(t - 1.0 <= EXIT_MOTION_MAX + 0.02, "closed {} s after it opened", t - 1.0);
        // The app still returns it: it stays closed until the key goes away.
        x.apply(vec![fly(None)], t + 1.0);
        assert!(x.list.is_empty());
        x.apply(Vec::new(), t + 2.0);
        x.apply(vec![fly(None)], t + 3.0);
        assert_eq!(x.list.len(), 1, "a new flight opens again");
    }

    #[test]
    fn a_fly_out_closes_at_the_exit_cap() {
        let mut x = extras(false);
        // A spring that takes ten seconds to settle.
        let slow = vo_core::spring::Spring::critical_to_98(10.0);
        x.apply(vec![fly(Some(slow))], 0.0);
        assert_eq!(x.fly_out_deadline(), Some(EXIT_MOTION_MAX));
        assert!(x.take_closed(EXIT_MOTION_MAX - 0.01).is_empty(), "still flying");
        assert!(x.moving(EXIT_MOTION_MAX - 0.01));
        assert_eq!(x.take_closed(EXIT_MOTION_MAX), vec!["fly".to_string()]);
    }

    #[test]
    fn under_reduced_motion_no_fly_out_opens() {
        let mut x = extras(true);
        x.apply(vec![fly(None), SurfaceView::new("ticker", spec(), bar("one"))], 0.0);
        assert_eq!(x.list.len(), 1);
        assert_eq!(x.list[0].key, "ticker", "other surfaces still open");
        assert_eq!(x.fly_out_deadline(), None);
    }

    #[test]
    fn the_anchor_follows_each_placement() {
        let size = Size::new(100.0, 50.0);
        let inset = Point::new(10.0, 5.0);
        let at = |a: PanelAnchor| (a.fx * SCREEN.w + a.dx, a.fy * SCREEN.h + a.dy);
        assert_eq!(at(PanelAnchor::of(Placement::Centered, size, inset)), (920.0, 520.0));
        let br = Placement::Anchored { edges: vo_host::Edges::BOTTOM_RIGHT, margin: [0, 24, 16, 0] };
        assert_eq!(at(PanelAnchor::of(br, size, inset)), (1920.0 - 24.0 - 100.0 + 10.0, 1080.0 - 16.0 - 50.0 + 5.0));
        assert_eq!(at(PanelAnchor::of(Placement::FromTop { fraction: 0.5 }, size, inset)), (920.0, 545.0));
        assert_eq!(at(PanelAnchor::of(Placement::Fullscreen, size, inset)), (10.0, 5.0));
    }

    #[test]
    fn the_view_fills_the_surface() {
        let mut x: Extras<()> = Extras::new(Box::new(Scene::new));
        let full = SurfaceView::new("scrim", spec(), Element::panel("dim").fill(Color::BLACK));
        x.apply(vec![full], 0.0);
        let scene = &x.list[0].scene;
        let id = scene.find(&[ROOT, "dim"]).unwrap();
        let s = scene.sampled(id, 1.0).unwrap();
        assert_eq!(s.size, Size::new(200.0, 40.0));
    }
}
