//! The app model shared by the live overlay and offline rendering: an Elm-shaped [`App`], its
//! update context [`Cx`] with timers, and the [`Driver`] that feeds messages into the scene.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use vo_host::{PointerEvent, Waker};
use vo_scene::element::Element;
use vo_scene::geom::Color;
use vo_scene::Scene;

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

/// Feeds messages, timers and pointer events into the app and the scene, in time order.
pub(crate) struct Driver<A: App> {
    pub(crate) app: A,
    pub(crate) scene: Scene<A::Msg>,
    tokens: Tokens,
    pub(crate) timers: Timers<A::Msg>,
    exit: bool,
    last_t: f64,
    /// The scene changed since the last frame.
    pub(crate) dirty: bool,
}

impl<A: App> Driver<A> {
    pub(crate) fn new(mut app: A, scene: Scene<A::Msg>, tokens: Tokens, t: f64) -> Driver<A> {
        let mut timers = Timers::default();
        let mut exit = false;
        app.init(&mut Cx { t, timers: &mut timers, exit: &mut exit });
        let mut d = Driver { app, scene, tokens, timers, exit, last_t: t, dirty: true };
        let view = d.app.view(&d.tokens);
        d.scene.apply(view, t);
        d
    }

    /// Runs `update` and the view at `t`. Times before the last event are moved up to it: the
    /// scene takes events in time order only.
    pub(crate) fn dispatch(&mut self, msg: A::Msg, t: f64) {
        let t = t.max(self.last_t);
        self.last_t = t;
        self.app.update(msg, &mut Cx { t, timers: &mut self.timers, exit: &mut self.exit });
        let view = self.app.view(&self.tokens);
        self.scene.apply(view, t);
        self.dirty = true;
    }

    /// Fires every timer due at or before `t`, each at its own time.
    pub(crate) fn fire_timers(&mut self, t: f64) {
        while let Some((at, msg)) = self.timers.pop_due(t) {
            self.dispatch(msg, at);
        }
    }

    pub(crate) fn pointer(&mut self, ev: &PointerEvent) {
        self.fire_timers(ev.t);
        let mut ev = *ev;
        ev.t = ev.t.max(self.last_t);
        self.last_t = ev.t;
        for msg in self.scene.pointer(&ev) {
            self.dispatch(msg, ev.t);
        }
    }

    pub(crate) fn moving(&self, t: f64) -> bool {
        self.scene.next_wake(t).is_some()
    }

    /// The app asked to exit and nothing moves any more at `t`.
    pub(crate) fn done(&self, t: f64) -> bool {
        self.exit && !self.moving(t)
    }
}
