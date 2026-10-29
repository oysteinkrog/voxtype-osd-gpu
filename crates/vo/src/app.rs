//! The app model shared by the live overlay and offline rendering: an Elm-shaped [`App`], its
//! update context [`Cx`] with timers, and the [`Driver`] that feeds messages into the scene.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use vo_host::{Placement, PointerEvent, Waker};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

use crate::surfaces::{Extras, MakeScene, SurfaceView};

/// Design tokens handed to `view`. A plain name to value map until `#[derive(Tokens)]` and
/// TOML loading land (plan decision 4).
#[derive(Clone, Debug, Default)]
pub struct Tokens {
    pub numbers: HashMap<String, f32>,
    pub colors: HashMap<String, Color>,
}

impl Tokens {
    pub fn num(&self, name: &str, default: f32) -> f32 {
        self.numbers.get(name).copied().unwrap_or(default)
    }

    pub fn color(&self, name: &str, default: Color) -> Color {
        self.colors.get(name).copied().unwrap_or(default)
    }
}

/// An overlay app. `update` changes state in answer to a message; `view` describes the state.
/// `view` is pure and runs once after each message, at the message's time.
pub trait App {
    type Msg: Clone + 'static;

    /// Called once at the start, before the first view. Start timers here.
    fn init(&mut self, cx: &mut Cx<Self::Msg>) {
        let _ = cx;
    }

    fn update(&mut self, msg: Self::Msg, cx: &mut Cx<Self::Msg>);

    fn view(&self, tokens: &Tokens) -> Element<Self::Msg>;

    /// Extra surfaces next to the main one, such as a ticker bar or a fullscreen scrim
    /// (`vo::SurfaceView`). Runs after every update, with `view`. A key that appears opens a
    /// surface; a key that goes away closes it once its view has left.
    fn surfaces(&self, tokens: &Tokens) -> Vec<SurfaceView<Self::Msg>> {
        let _ = tokens;
        Vec::new()
    }

    /// The main surface's size and place, read after every update. `None` keeps what the
    /// overlay opened with. Live, a change resizes and moves the open surface in the same
    /// run, so the clock, the timers and the app's state carry on; the last frame stays on
    /// screen until the first frame at the new size replaces it. Offline it is ignored.
    fn main_surface(&self) -> Option<(Size, Placement)> {
        None
    }

    /// The most frames a second the main surface needs now, read after every update. `None`
    /// (the default) draws at every vblank while anything moves. With `Some(fps)` a live
    /// overlay spaces its main surface's frames at least `1 / fps` apart: a message that
    /// changes the view, or a scene that moves, waits for the next paced slot, and that frame
    /// shows the newest state. For a quiet stretch the app knows of, such as a popup on room
    /// tone (MOTION-PLAN rule 8: silence is idle). Return `None` again as soon as the quiet
    /// ends: the next change then draws at once. Extra surfaces and offline runs ignore it.
    fn frame_pace(&self) -> Option<f64> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimerId(u64);

pub(crate) struct Timers<M> {
    next: u64,
    /// `(at, id, msg)`, unsorted; there are only ever a few.
    pending: Vec<(f64, TimerId, M)>,
}

impl<M> Default for Timers<M> {
    fn default() -> Self {
        Timers { next: 0, pending: Vec::new() }
    }
}

impl<M> Timers<M> {
    pub(crate) fn next_at(&self) -> Option<f64> {
        self.pending.iter().map(|p| p.0).min_by(f64::total_cmp)
    }

    /// Removes and returns the earliest timer due at or before `t`.
    fn pop_due(&mut self, t: f64) -> Option<(f64, M)> {
        let i = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, p)| p.0 <= t)
            .min_by(|a, b| a.1 .0.total_cmp(&b.1 .0).then(a.1 .1 .0.cmp(&b.1 .1 .0)))?
            .0;
        let (at, _, msg) = self.pending.remove(i);
        Some((at, msg))
    }
}

/// What `update` can do besides changing state.
pub struct Cx<'a, M> {
    t: f64,
    timers: &'a mut Timers<M>,
    exit: &'a mut bool,
}

impl<M> Cx<'_, M> {
    /// The time of the message being handled, on the host clock (or the scenario clock offline).
    pub fn t(&self) -> f64 {
        self.t
    }

    /// Sends `msg` to `update` `secs` from now.
    pub fn after(&mut self, secs: f64, msg: M) -> TimerId {
        let id = TimerId(self.timers.next);
        self.timers.next += 1;
        self.timers.pending.push((self.t + secs, id, msg));
        id
    }

    pub fn cancel(&mut self, id: TimerId) {
        self.timers.pending.retain(|p| p.1 != id);
    }

    /// Ends the run once everything has settled, so exit motion plays out first.
    pub fn exit(&mut self) {
        *self.exit = true;
    }
}

/// Sends messages to a running overlay from any thread. Made with [`channel`].
pub struct Proxy<M> {
    tx: mpsc::Sender<M>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl<M> Clone for Proxy<M> {
    fn clone(&self) -> Self {
        Proxy { tx: self.tx.clone(), waker: self.waker.clone() }
    }
}

impl<M> Proxy<M> {
    /// Queues `msg` and wakes the overlay's loop. Returns false once the overlay has ended.
    pub fn send(&self, msg: M) -> bool {
        if self.tx.send(msg).is_err() {
            return false;
        }
        if let Some(w) = self.waker.lock().unwrap().as_ref() {
            w.wake();
        }
        true
    }
}

/// The receiving end of [`channel`], handed to the overlay through `Options::inbox`.
pub struct Inbox<M> {
    rx: mpsc::Receiver<M>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl<M> Inbox<M> {
    pub(crate) fn connect(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
    }

    pub(crate) fn drain(&self) -> impl Iterator<Item = M> + '_ {
        self.rx.try_iter()
    }
}

/// A channel into a running overlay: messages sent before the overlay starts wait for it.
pub fn channel<M>() -> (Proxy<M>, Inbox<M>) {
    let (tx, rx) = mpsc::channel();
    let waker = Arc::new(Mutex::new(None));
    (Proxy { tx, waker: waker.clone() }, Inbox { rx, waker })
}

/// Seconds the exit motion may run after the app asks to exit.
pub(crate) const EXIT_MOTION_MAX: f64 = 3.0;

/// Feeds messages, timers and pointer events into the app and the scene, in time order.
pub(crate) struct Driver<A: App> {
    pub(crate) app: A,
    pub(crate) scene: Scene<A::Msg>,
    tokens: Tokens,
    pub(crate) timers: Timers<A::Msg>,
    exit: bool,
    /// When the app asked to exit.
    exit_at: Option<f64>,
    last_t: f64,
    /// The scene changed since the last frame, so the host needs a new one.
    pub(crate) dirty: bool,
    /// Transparent room around the app's view for ink outside its box (`Options::margin`).
    margin: Option<vo_scene::Insets>,
    /// The app's extra surfaces, each with its own scene (`App::surfaces`).
    pub(crate) extras: Extras<A::Msg>,
    /// Build the view at the next frame, not at each message, while the app has a frame
    /// pace (`App::frame_pace`). A live overlay sets it: room tone at 50 audio batches a
    /// second then costs one view per drawn frame, not one per message.
    pub(crate) lazy: bool,
    /// The time of the newest message whose view `lazy` put off (`flush` builds it).
    stale: Option<f64>,
}

impl<A: App> Driver<A> {
    /// A driver whose extra surfaces get plain scenes (estimated text).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(app: A, scene: Scene<A::Msg>, tokens: Tokens, t: f64) -> Driver<A> {
        Driver::with_surfaces(app, scene, tokens, t, Box::new(Scene::new))
    }

    /// A driver whose extra surfaces get their scenes from `make`.
    pub(crate) fn with_surfaces(
        mut app: A,
        scene: Scene<A::Msg>,
        tokens: Tokens,
        t: f64,
        make: MakeScene<A::Msg>,
    ) -> Driver<A> {
        let mut timers = Timers::default();
        let mut exit = false;
        app.init(&mut Cx { t, timers: &mut timers, exit: &mut exit });
        let extras = Extras::new(make);
        let exit_at = exit.then_some(t);
        let mut d = Driver {
            app,
            scene,
            tokens,
            timers,
            exit,
            exit_at,
            last_t: t,
            dirty: true,
            margin: None,
            extras,
            lazy: false,
            stale: None,
        };
        let view = d.view();
        d.scene.apply(view, t);
        d.extras.apply(d.app.surfaces(&d.tokens), t);
        d
    }

    /// The app's view, inside the margin when there is one.
    fn view(&self) -> Element<A::Msg> {
        let view = self.app.view(&self.tokens);
        match self.margin {
            Some(m) => Element::column("vo-margin").pad_edges(m.top, m.right, m.bottom, m.left).child(view),
            None => view,
        }
    }

    /// Puts the view inside a transparent margin of `m` from `t` on.
    pub(crate) fn set_margin(&mut self, m: vo_scene::Insets, t: f64) {
        self.margin = Some(m);
        self.stale = None;
        let view = self.view();
        self.dirty |= self.scene.apply(view, t);
    }

    /// Runs `update` and the view at `t`. Times before the last event are moved up to it: the
    /// scene takes events in time order only.
    pub(crate) fn dispatch(&mut self, msg: A::Msg, t: f64) {
        let t = t.max(self.last_t);
        self.last_t = t;
        self.app.update(msg, &mut Cx { t, timers: &mut self.timers, exit: &mut self.exit });
        if self.exit && self.exit_at.is_none() {
            self.exit_at = Some(t);
        }
        // Under a frame pace the frame waits for its slot anyway, so the view waits with it.
        // Extra surfaces and the exit build at once: their frames are not paced.
        if self.lazy && !self.exit && self.extras.list.is_empty() && self.app.frame_pace().is_some() {
            self.stale = Some(t);
            return;
        }
        self.stale = None;
        self.rebuild(t);
    }

    /// Builds the view and applies it to the scenes at `t`.
    fn rebuild(&mut self, t: f64) {
        let view = self.view();
        // A message that changes nothing drawn, such as a hover that only stops a timer,
        // costs no frame.
        self.dirty |= self.scene.apply(view, t);
        self.extras.apply(self.app.surfaces(&self.tokens), t);
    }

    /// Whether a message's view waits for the next frame (`lazy`).
    pub(crate) fn stale(&self) -> bool {
        self.stale.is_some()
    }

    /// Builds the view put off by `lazy`, at the time of the newest message it shows. Runs
    /// before anything reads the scene: a frame, a pointer event, a new viewport.
    pub(crate) fn flush(&mut self) {
        if let Some(t) = self.stale.take() {
            self.rebuild(t);
        }
    }

    /// Fires the earliest timer if it is due at or before `t`. Returns whether one fired.
    pub(crate) fn fire_timer(&mut self, t: f64) -> bool {
        match self.timers.pop_due(t) {
            Some((at, msg)) => {
                self.dispatch(msg, at);
                true
            }
            None => false,
        }
    }

    /// Fires every timer due at or before `t`, each at its own time.
    pub(crate) fn fire_timers(&mut self, t: f64) {
        while let Some((at, msg)) = self.timers.pop_due(t) {
            self.dispatch(msg, at);
        }
    }

    pub(crate) fn pointer(&mut self, ev: &PointerEvent) {
        self.pointer_on(None, ev);
    }

    /// A pointer event on the main surface (`None`) or on extra surface `extra`.
    pub(crate) fn pointer_on(&mut self, extra: Option<usize>, ev: &PointerEvent) {
        self.fire_timers(ev.t);
        self.flush();
        let mut ev = *ev;
        ev.t = ev.t.max(self.last_t);
        self.last_t = ev.t;
        let msgs = match extra {
            None => self.scene.pointer(&ev),
            // A timer above may have closed it.
            Some(i) => match self.extras.list.get_mut(i) {
                Some(e) => e.scene.pointer(&ev),
                None => Vec::new(),
            },
        };
        for msg in msgs {
            self.dispatch(msg, ev.t);
        }
    }

    /// The main surface's scene moves at `t`.
    pub(crate) fn moving(&self, t: f64) -> bool {
        self.scene.next_wake(t).is_some()
    }

    /// The app asked to exit and nothing moves any more at `t`, on any surface, or the exit
    /// motion has run for `EXIT_MOTION_MAX`.
    pub(crate) fn done(&self, t: f64) -> bool {
        self.exit && (self.overdue(t) || (!self.moving(t) && !self.extras.moving(t)))
    }

    /// The app asked to exit `EXIT_MOTION_MAX` or more before `t`. A motion that loops for
    /// ever would otherwise keep the run, and a resident overlay's next show, waiting.
    pub(crate) fn overdue(&self, t: f64) -> bool {
        self.exit_deadline().is_some_and(|d| t >= d)
    }

    /// When the app asked to exit.
    pub(crate) fn exit_at(&self) -> Option<f64> {
        self.exit_at
    }

    /// When the run ends even if something still moves.
    pub(crate) fn exit_deadline(&self) -> Option<f64> {
        self.exit_at.map(|e| e + EXIT_MOTION_MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<(u32, f64)>);

    impl App for Log {
        type Msg = u32;

        fn init(&mut self, cx: &mut Cx<u32>) {
            cx.after(2.0, 2);
            let gone = cx.after(1.5, 99);
            cx.after(1.0, 1);
            cx.cancel(gone);
        }

        fn update(&mut self, msg: u32, cx: &mut Cx<u32>) {
            self.0.push((msg, cx.t()));
            if msg == 2 {
                cx.exit();
            }
        }

        fn view(&self, _: &Tokens) -> Element<u32> {
            Element::panel("root")
        }
    }

    #[test]
    fn timers_fire_in_order_at_their_own_time() {
        let mut d = Driver::new(Log::default(), Scene::new(), Tokens::default(), 0.5);
        assert_eq!(d.timers.next_at(), Some(1.5));
        d.fire_timers(3.0);
        assert_eq!(d.app.0, vec![(1, 1.5), (2, 2.5)]);
        assert!(d.done(3.0));
    }

    /// A toast body that holds its dismiss timer while hovered and draws nothing different.
    struct Hold {
        timer: Option<TimerId>,
    }

    impl App for Hold {
        type Msg = bool;

        fn init(&mut self, cx: &mut Cx<bool>) {
            self.timer = Some(cx.after(5.0, false));
        }

        fn update(&mut self, hovered: bool, cx: &mut Cx<bool>) {
            if let Some(id) = self.timer.take() {
                cx.cancel(id);
            }
            if !hovered {
                self.timer = Some(cx.after(5.0, false));
            }
        }

        fn view(&self, _: &Tokens) -> Element<bool> {
            Element::panel("root").size(100.0, 40.0).on_hover(true, false)
        }
    }

    #[test]
    fn a_hover_that_only_stops_a_timer_costs_no_frame() {
        use vo_host::{PointerEvent, PointerKind};
        use vo_scene::geom::Point;

        let mut d = Driver::new(Hold { timer: None }, Scene::new(), Tokens::default(), 0.0);
        // The first frame went out and the scene has settled.
        d.dirty = false;
        assert!(!d.moving(1.0));
        let ev = PointerEvent { surface: 0, t: 1.0, pos: Point::new(10.0, 10.0), kind: PointerKind::Enter };
        d.pointer(&ev);
        assert_eq!(d.timers.next_at(), None, "the hover stopped the timer");
        assert!(!d.dirty && !d.moving(1.0), "but asked for no frame");
    }

    /// An app with a dial that spins for ever.
    struct Spinner;

    impl App for Spinner {
        type Msg = ();

        fn update(&mut self, _: (), cx: &mut Cx<()>) {
            cx.exit();
        }

        fn view(&self, _: &Tokens) -> Element<()> {
            let dial = Element::shape("dial", vo_scene::display::ShapeKind::Circle)
                .size(40.0, 40.0)
                .ambient(vo_scene::LoopProp::Rotation, vo_scene::Loop::spin(1.0));
            Element::column("root").size(100.0, 100.0).child(dial)
        }
    }

    #[test]
    fn a_motion_that_never_stops_ends_the_run_after_exit_motion_max() {
        let mut d = Driver::new(Spinner, Scene::new(), Tokens::default(), 0.0);
        assert!(d.moving(1.0) && !d.done(1.0));
        d.dispatch((), 1.0);
        assert_eq!(d.exit_deadline(), Some(1.0 + EXIT_MOTION_MAX));
        assert!(d.moving(2.0) && !d.done(2.0), "the exit motion plays");
        assert!(d.moving(1.0 + EXIT_MOTION_MAX) && d.done(1.0 + EXIT_MOTION_MAX), "until the cap");
    }

    /// An app with an idle loop that drops it from the view once it asks to exit, as the
    /// idle-motion rule asks (MOTION-PLAN rule 8).
    struct Breather {
        exiting: bool,
    }

    impl App for Breather {
        type Msg = ();

        fn update(&mut self, _: (), cx: &mut Cx<()>) {
            self.exiting = true;
            cx.exit();
        }

        fn view(&self, _: &Tokens) -> Element<()> {
            let dot = Element::shape("dot", vo_scene::display::ShapeKind::Circle).size(20.0, 20.0);
            let dot = if self.exiting {
                dot
            } else {
                dot.ambient(vo_scene::LoopProp::Scale, vo_scene::Loop::pulse(0.2, 1.6))
                    .ambient(vo_scene::LoopProp::Rotation, vo_scene::Loop::spin(2.0))
            };
            Element::column("root").size(100.0, 100.0).child(dot)
        }
    }

    #[test]
    fn an_idle_loop_dropped_at_exit_settles_before_the_cap() {
        let mut d = Driver::new(Breather { exiting: false }, Scene::new(), Tokens::default(), 0.0);
        assert!(d.moving(5.0) && !d.done(5.0), "the idle loop runs while open");
        let exit = 5.3;
        d.dispatch((), exit);
        let deadline = d.exit_deadline().unwrap();
        // Step at the ambient rate until nothing moves.
        let mut t = exit;
        while d.moving(t) && t < deadline + 1.0 {
            t += 1.0 / 30.0;
        }
        assert!(t < deadline, "still moving at the cap: settled at {t}, cap {deadline}");
        assert!(d.done(t) && !d.overdue(t), "the run ends because it settled, not by the cap");
        assert!(t - exit < 1.5, "settles well inside the cap: took {:.2} s", t - exit);
    }

    #[test]
    fn late_messages_move_up_to_the_last_event() {
        let mut d = Driver::new(Log::default(), Scene::new(), Tokens::default(), 0.0);
        d.dispatch(7, 2.0);
        d.dispatch(8, 1.0);
        assert_eq!(d.app.0, vec![(7, 2.0), (8, 2.0)]);
    }
}
