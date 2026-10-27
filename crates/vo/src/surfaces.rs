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

use vo_host::OverlaySpec;
use vo_scene::element::Element;
use vo_scene::taffy::style::Dimension;
use vo_scene::Scene;

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

/// One extra surface of an app: what it is and what it shows.
pub struct SurfaceView<M> {
    /// Stays the same for the life of the surface. Two views with one key are one surface.
    pub key: String,
    /// Read when the surface opens. Later changes are ignored; use a new key to reopen.
    pub spec: OverlaySpec,
    /// The surface's view. Its root fills the surface.
    pub view: Element<M>,
}

impl<M> SurfaceView<M> {
    pub fn new(key: impl Into<String>, spec: OverlaySpec, view: Element<M>) -> SurfaceView<M> {
        SurfaceView { key: key.into(), spec, view }
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
}

/// Makes the scene for a new extra surface, with the app's text layout and motion settings.
pub(crate) type MakeScene<M> = Box<dyn Fn() -> Scene<M>>;

/// Every extra surface of an app, in the order the app first returned them.
pub(crate) struct Extras<M> {
    pub(crate) list: Vec<Extra<M>>,
    make: MakeScene<M>,
}

impl<M: Clone + 'static> Extras<M> {
    pub(crate) fn new(make: MakeScene<M>) -> Extras<M> {
        Extras { list: Vec::new(), make }
    }

    /// Applies the app's surface views at `t`: new keys get a scene, gone keys start closing.
    pub(crate) fn apply(&mut self, views: Vec<SurfaceView<M>>, t: f64) {
        for e in &mut self.list {
            e.closing = true;
        }
        for v in views {
            let root = root().child(v.view.grow(1.0));
            match self.list.iter_mut().find(|e| e.key == v.key) {
                Some(e) => {
                    e.closing = false;
                    e.dirty |= e.scene.apply(root, t);
                }
                None => {
                    let mut scene = (self.make)();
                    scene.set_viewport(t, v.spec.size, 1.0);
                    scene.apply(root, t);
                    self.list.push(Extra { key: v.key, spec: v.spec, scene, dirty: true, closing: false });
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

    /// Removes the closing surfaces that have settled at `t` and returns their keys.
    pub(crate) fn take_closed(&mut self, t: f64) -> Vec<String> {
        let mut gone = Vec::new();
        self.list.retain(|e| {
            let done = e.closing && e.scene.next_wake(t).is_none();
            if done {
                gone.push(e.key.clone());
            }
            !done
        });
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vo_scene::element::Transition;
    use vo_scene::geom::{Color, Size};

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
