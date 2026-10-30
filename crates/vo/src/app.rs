//! The app model shared by the live overlay and offline rendering: an Elm-shaped [`App`], its
//! update context [`Cx`] with timers, and the [`Driver`] that feeds messages into the scene.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use vo_host::{FocusProxySpec, KeyEvent, KeyState, LogicalKey, Mods, Placement, PointerEvent, RepeatInfo, Waker};
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
    /// screen until the first frame at the new size replaces it. Offline the frames take the
    /// new size from the next frame on.
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

    /// A key went down, repeated or went up while the app held keyboard focus
    /// ([`Cx::take_keyboard_focus`]). Repeats come from the driver, on the host clock, at the
    /// compositor's rate. Return true when the key was used. The view is rebuilt after it
    /// either way.
    fn key(&mut self, ev: &KeyEvent, cx: &mut Cx<Self::Msg>) -> bool {
        let _ = (ev, cx);
        false
    }

    /// Keyboard focus came or went. `requested` is true for a loss the app asked for with
    /// [`Cx::release_keyboard_focus`]; false when the compositor took focus away (a click
    /// elsewhere, Alt+Tab, the lock screen).
    fn keyboard_focus(&mut self, gained: bool, requested: bool, cx: &mut Cx<Self::Msg>) {
        let _ = (gained, requested, cx);
    }
}

/// What the app asked for about keyboard focus during a callback.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FocusRequest {
    Take(FocusProxySpec),
    Release,
}

/// Keyboard state the driver keeps: modifiers, the repeat rate and the key repeating now.
#[derive(Clone, Debug, Default)]
pub(crate) struct Keys {
    info: Option<RepeatInfo>,
    mods: Mods,
    /// The held key that repeats, and how many repeats it sent.
    repeat: Option<(KeyEvent, u32)>,
}

impl Keys {
    /// When the held key repeats next.
    fn next_repeat(&self) -> Option<f64> {
        let (ev, n) = self.repeat.as_ref()?;
        self.info?.nth(ev.t, *n)
    }
}

/// Keys that repeat while held: everything but modifiers.
fn repeats(ev: &KeyEvent) -> bool {
    !matches!(ev.key, LogicalKey::Named(k) if k.is_modifier())
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
    focus: &'a mut Option<FocusRequest>,
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

    /// Asks for keyboard focus through a focus proxy (`HostCtl::open_focus_proxy`). Keys
    /// then come to [`App::key`], and [`App::keyboard_focus`] tells when focus arrives.
    /// Offline, focus comes from the scenario's inputs instead.
    pub fn take_keyboard_focus(&mut self, spec: FocusProxySpec) {
        *self.focus = Some(FocusRequest::Take(spec));
    }

    /// Gives keyboard focus back: the proxy closes, and `App::keyboard_focus(false, true)`
    /// follows at once.
    pub fn release_keyboard_focus(&mut self) {
        *self.focus = Some(FocusRequest::Release);
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
    /// How many views were applied to the main scene, so a live overlay checks the margin
    /// a new view needs once per view (EG-08).
    pub(crate) views: u64,
    pub(crate) keys: Keys,
    /// A focus change the app asked for, for the live overlay to carry out.
    pub(crate) focus: Option<FocusRequest>,
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
        let mut focus = None;
        app.init(&mut Cx { t, timers: &mut timers, exit: &mut exit, focus: &mut focus });
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
            views: 1,
            keys: Keys::default(),
            focus,
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
            // A margin that grows mid-show moves the view inside the surface while the surface
            // moves the other way on screen: a jump, so the panel stays still (EG-08).
            Some(m) => Element::column("vo-margin")
                .pad_edges(m.top, m.right, m.bottom, m.left)
                .motion(vo_scene::element::Prop::Size, vo_scene::element::Motion::Snap)
                .child(view.motion(vo_scene::element::Prop::Position, vo_scene::element::Motion::Snap)),
            None => view,
        }
    }

    /// Puts the view inside a transparent margin of `m` from `t` on. `t` is an event like any
    /// other: a timer due before it and fired after it moves up to it.
    pub(crate) fn set_margin(&mut self, m: vo_scene::Insets, t: f64) {
        let t = t.max(self.last_t);
        self.last_t = t;
        self.margin = Some(m);
        self.stale = None;
        self.views += 1;
        let view = self.view();
        self.dirty |= self.scene.apply(view, t);
    }

    /// Runs `update` and the view at `t`. Times before the last event are moved up to it: the
    /// scene takes events in time order only.
    pub(crate) fn dispatch(&mut self, msg: A::Msg, t: f64) {
        self.run(t, |app, cx| app.update(msg, cx));
    }

    /// Runs `f` on the app at `t` (moved up to the last event), then the view.
    fn run(&mut self, t: f64, f: impl FnOnce(&mut A, &mut Cx<A::Msg>)) {
        let t = t.max(self.last_t);
        self.last_t = t;
        f(&mut self.app, &mut Cx { t, timers: &mut self.timers, exit: &mut self.exit, focus: &mut self.focus });
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
        self.views += 1;
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

    /// When the next timer or key repeat is due.
    pub(crate) fn next_timer_at(&self) -> Option<f64> {
        [self.timers.next_at(), self.keys.next_repeat()].into_iter().flatten().min_by(f64::total_cmp)
    }

    /// Fires the earliest timer or key repeat if it is due at or before `t`. Returns whether
    /// one fired. A repeat due at the same time as a timer goes first.
    pub(crate) fn fire_timer(&mut self, t: f64) -> bool {
        let timer = self.timers.next_at().filter(|&at| at <= t);
        match self.keys.next_repeat().filter(|&at| at <= t) {
            Some(at) if timer.is_none_or(|tm| at <= tm) => {
                let mods = self.keys.mods;
                let (held, n) = self.keys.repeat.as_mut().expect("a repeat is due");
                *n += 1;
                let ev = KeyEvent { t: at, state: KeyState::Repeat, mods, ..held.clone() };
                self.deliver_key(&ev);
                true
            }
            _ => match self.timers.pop_due(t) {
                Some((at, msg)) => {
                    self.dispatch(msg, at);
                    true
                }
                None => false,
            },
        }
    }

    /// Fires every timer and key repeat due at or before `t`, each at its own time.
    pub(crate) fn fire_timers(&mut self, t: f64) {
        while self.fire_timer(t) {}
    }

    /// A key from the host. Due timers and repeats fire first. A press of a key that repeats
    /// starts its repeat (replacing any other), a press of a modifier stops it, and the
    /// release of the repeating key stops it.
    pub(crate) fn key(&mut self, ev: &KeyEvent) {
        self.fire_timers(ev.t);
        self.flush();
        let mut ev = ev.clone();
        ev.t = ev.t.max(self.last_t);
        match ev.state {
            KeyState::Down => {
                let on = repeats(&ev) && self.keys.info.is_some_and(|i| i.rate_hz > 0.0);
                self.keys.repeat = on.then(|| (ev.clone(), 0));
            }
            KeyState::Up => {
                if self.keys.repeat.as_ref().is_some_and(|(r, _)| r.code == ev.code) {
                    self.keys.repeat = None;
                }
            }
            KeyState::Repeat => {}
        }
        self.deliver_key(&ev);
    }

    /// Gives a key to the focused node (`Element::on_key`), or to `App::key` when no node
    /// is focusable.
    fn deliver_key(&mut self, ev: &KeyEvent) {
        match self.scene.key(ev) {
            Some(msg) => self.dispatch(msg, ev.t),
            None => self.run(ev.t, |app, cx| {
                app.key(ev, cx);
            }),
        }
    }

    /// Keyboard focus came or went. A loss stops any repeat.
    pub(crate) fn keyboard_focus(&mut self, t: f64, gained: bool, requested: bool) {
        self.fire_timers(t);
        self.flush();
        if !gained {
            self.keys.repeat = None;
        }
        self.run(t, |app, cx| app.keyboard_focus(gained, requested, cx));
    }

    /// The modifier state changed: any repeat stops.
    pub(crate) fn modifiers(&mut self, t: f64, mods: Mods) {
        self.fire_timers(t);
        if mods != self.keys.mods {
            self.keys.mods = mods;
            self.keys.repeat = None;
        }
    }

    /// The compositor's repeat rate and delay.
    pub(crate) fn repeat_info(&mut self, info: RepeatInfo) {
        self.keys.info = Some(info);
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

    /// A margin that comes at 1.2 (the surface's configure) and a timer due at 1.0 that a late
    /// wake-up fires after it: the timer's message moves up to 1.2, so the scene still takes
    /// its events in time order.
    #[test]
    fn a_timer_fired_after_a_margin_moves_up_to_it() {
        let mut d = Driver::new(Log::default(), Scene::new(), Tokens::default(), 0.0);
        d.set_margin(vo_scene::Insets::new(4.0, 4.0, 4.0, 4.0), 1.2);
        d.fire_timers(1.3);
        assert_eq!(d.app.0, vec![(1, 1.2)]);
    }

    #[test]
    fn late_messages_move_up_to_the_last_event() {
        let mut d = Driver::new(Log::default(), Scene::new(), Tokens::default(), 0.0);
        d.dispatch(7, 2.0);
        d.dispatch(8, 1.0);
        assert_eq!(d.app.0, vec![(7, 2.0), (8, 2.0)]);
    }

    /// Logs every key it gets, as (state, code, time).
    #[derive(Default)]
    struct Keyed(Vec<(KeyState, u32, f64)>);

    impl App for Keyed {
        type Msg = ();
        fn update(&mut self, _: (), _: &mut Cx<()>) {}
        fn view(&self, _: &Tokens) -> Element<()> {
            Element::panel("root")
        }
        fn key(&mut self, ev: &KeyEvent, _: &mut Cx<()>) -> bool {
            self.0.push((ev.state, ev.code, ev.t));
            true
        }
    }

    fn key_ev(state: KeyState, code: u32, t: f64) -> KeyEvent {
        let key = if code == 42 { LogicalKey::Named(vo_host::NamedKey::ShiftLeft) } else { LogicalKey::Char('a') };
        KeyEvent { surface: 1, t, state, key, code, text: None, mods: Mods::NONE }
    }

    /// Holds 'a' from 1.0 to `up`, stepping the driver at `hz` as a frame loop would, and
    /// returns what the app saw.
    fn hold_a(hz: f64, up: f64, extra: impl Fn(&mut Driver<Keyed>, f64)) -> Vec<(KeyState, u32, f64)> {
        let mut d = Driver::new(Keyed::default(), Scene::new(), Tokens::default(), 0.0);
        d.repeat_info(RepeatInfo { rate_hz: 25.0, delay_s: 0.5 });
        let mut t = 0.0;
        let mut pressed = false;
        while t < 3.0 {
            t += 1.0 / hz;
            if !pressed && t >= 1.0 {
                d.key(&key_ev(KeyState::Down, 30, 1.0));
                pressed = true;
            }
            if t >= up && d.keys.repeat.is_some() {
                d.key(&key_ev(KeyState::Up, 30, up));
            }
            extra(&mut d, t);
            d.fire_timers(t);
        }
        d.app.0
    }

    #[test]
    fn key_repeats_are_the_same_at_any_frame_rate() {
        let at60 = hold_a(60.0, 2.0, |_, _| {});
        let at144 = hold_a(144.0, 2.0, |_, _| {});
        assert_eq!(at60, at144);
        // Down at 1.0, repeats from 1.5 every 40 ms while held, then the release.
        let repeats: Vec<f64> = at60.iter().filter(|e| e.0 == KeyState::Repeat).map(|e| e.2).collect();
        assert_eq!(repeats.len(), 13, "{repeats:?}");
        assert!((repeats[0] - 1.5).abs() < 1e-9 && (repeats[12] - 1.98).abs() < 1e-9);
        assert_eq!(at60.first().map(|e| e.0), Some(KeyState::Down));
        assert_eq!(at60.last().map(|e| (e.0, e.2)), Some((KeyState::Up, 2.0)));
    }

    #[test]
    fn repeat_stops_on_focus_loss_and_modifier_change() {
        let lost = hold_a(60.0, 9.0, |d, t| {
            if (t - 1.7).abs() < 0.5 / 60.0 {
                d.keyboard_focus(1.7, false, false);
            }
        });
        assert!(lost.iter().all(|e| e.2 < 1.7 + 1e-9), "{lost:?}");
        let shifted = hold_a(60.0, 9.0, |d, t| {
            if (t - 1.7).abs() < 0.5 / 60.0 {
                d.modifiers(1.7, Mods::SHIFT);
            }
        });
        assert!(shifted.iter().all(|e| e.2 < 1.7 + 1e-9), "{shifted:?}");
        assert!(shifted.iter().any(|e| e.0 == KeyState::Repeat));
        // A modifier never repeats, and pressing one stops the key repeating.
        let mut d = Driver::new(Keyed::default(), Scene::new(), Tokens::default(), 0.0);
        d.repeat_info(RepeatInfo { rate_hz: 25.0, delay_s: 0.5 });
        d.key(&key_ev(KeyState::Down, 42, 1.0));
        assert_eq!(d.next_timer_at(), None);
        d.key(&key_ev(KeyState::Down, 30, 1.1));
        assert_eq!(d.next_timer_at(), Some(1.6));
        d.key(&key_ev(KeyState::Down, 42, 1.2));
        assert_eq!(d.next_timer_at(), None);
        // No repeat until the compositor's rate is known, and none at rate 0.
        let mut d = Driver::new(Keyed::default(), Scene::new(), Tokens::default(), 0.0);
        d.key(&key_ev(KeyState::Down, 30, 1.0));
        assert_eq!(d.next_timer_at(), None);
        d.repeat_info(RepeatInfo { rate_hz: 0.0, delay_s: 0.5 });
        d.key(&key_ev(KeyState::Down, 30, 2.0));
        assert_eq!(d.next_timer_at(), None);
    }

    /// Edits through a focusable node when `node` is set, else through `App::key`.
    struct Routed {
        node: bool,
        via_node: Vec<f64>,
        via_app: Vec<f64>,
    }

    impl App for Routed {
        type Msg = Option<KeyEvent>;
        fn update(&mut self, msg: Option<KeyEvent>, _: &mut Cx<Option<KeyEvent>>) {
            if let Some(ev) = msg {
                self.via_node.push(ev.t);
            }
        }
        fn view(&self, _: &Tokens) -> Element<Option<KeyEvent>> {
            let root = Element::column("root");
            if self.node {
                root.child(Element::panel("text").on_key(Some))
            } else {
                root
            }
        }
        fn key(&mut self, ev: &KeyEvent, _: &mut Cx<Option<KeyEvent>>) -> bool {
            self.via_app.push(ev.t);
            true
        }
    }

    #[test]
    fn keys_go_to_the_focused_node_before_app_key() {
        let app = Routed { node: true, via_node: Vec::new(), via_app: Vec::new() };
        let mut d = Driver::new(app, Scene::new(), Tokens::default(), 0.0);
        d.key(&key_ev(KeyState::Down, 30, 1.0));
        assert_eq!((d.app.via_node.as_slice(), d.app.via_app.as_slice()), (&[1.0][..], &[][..]));
        // The node goes away: keys reach App::key.
        d.app.node = false;
        d.dispatch(None, 2.0);
        d.key(&key_ev(KeyState::Down, 30, 3.0));
        assert_eq!(d.app.via_app, [3.0]);
    }
}
