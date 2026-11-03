//! Screen reader access for a live overlay (plan stage F8), through AccessKit on AT-SPI.
//!
//! An app that returns `Some` from `App::accessible` gets an AccessKit adapter
//! (accesskit_unix) the first time it does. The adapter watches the session bus for an
//! assistive technology and stays inactive while there is none: then the overlay does not even
//! ask the app for its description again, so it costs nothing while no screen reader runs.
//! When one starts, the adapter's activation wakes the loop, and the next change sends the
//! full tree (`vo_a11y::tree`). From then on every change that matters sends it again: the
//! edit field at once, the shown text as `vo_a11y::Announcer` lets it.
//!
//! The edit field takes the focus and set-selection actions: the adapter's thread queues them
//! and wakes the loop, which maps each against the tree the screen reader saw
//! (`vo_a11y::request`) and hands it to the app (`App::accessibility`).
//!
//! Built only with the `a11y` feature; without it [`A11y`] does nothing. `VO_A11Y=0` turns it
//! off at run time.

use vo_a11y::Accessible;
use vo_host::Waker;

#[cfg(feature = "a11y")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use vo_a11y::accesskit::{ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate};
    use vo_a11y::{Accessible, Announcer, Request};
    use vo_host::Waker;

    struct Activate(Arc<AtomicBool>, Waker);

    impl ActivationHandler for Activate {
        fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
            // The tree comes with the next change on the overlay's own thread.
            self.0.store(true, Ordering::SeqCst);
            self.1.wake();
            None
        }
    }

    struct Deactivate(Arc<AtomicBool>);

    impl DeactivationHandler for Deactivate {
        fn deactivate_accessibility(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }

    /// Queues the screen reader's actions for the overlay's own thread and wakes it.
    struct Actions(Arc<Mutex<Vec<ActionRequest>>>, Waker);

    impl ActionHandler for Actions {
        fn do_action(&mut self, request: ActionRequest) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(request);
            self.1.wake();
        }
    }

    pub(super) struct Live {
        adapter: accesskit_unix::Adapter,
        active: Arc<AtomicBool>,
        /// Whether the tree went out since the last activation.
        sent: bool,
        /// What the adapter was last told about keyboard focus.
        focused: bool,
        announcer: Announcer,
        last: Option<Accessible>,
        actions: Arc<Mutex<Vec<ActionRequest>>>,
    }

    impl Live {
        pub(super) fn start(waker: Waker) -> Live {
            let active = Arc::new(AtomicBool::new(false));
            let actions = Arc::new(Mutex::new(Vec::new()));
            let adapter = accesskit_unix::Adapter::new(
                Activate(active.clone(), waker.clone()),
                Actions(actions.clone(), waker),
                Deactivate(active.clone()),
            );
            Live { adapter, active, sent: false, focused: false, announcer: Announcer::default(), last: None, actions }
        }

        /// The actions that came since the last call, read against the tree last sent: the
        /// one the screen reader saw. Ones that tree does not offer are dropped.
        pub(super) fn requests(&mut self) -> Vec<Request> {
            let queued = std::mem::take(&mut *self.actions.lock().unwrap_or_else(|e| e.into_inner()));
            let Some(last) = self.last.as_ref() else { return Vec::new() };
            queued.iter().filter_map(|r| vo_a11y::request(last, r)).collect()
        }

        /// The focused node is focused for a screen reader only while the overlay has
        /// keyboard focus.
        pub(super) fn focus(&mut self, focused: bool) {
            if focused != self.focused {
                self.focused = focused;
                self.adapter.update_window_focus_state(focused);
            }
        }

        pub(super) fn active(&self) -> bool {
            self.active.load(Ordering::SeqCst)
        }

        /// No screen reader now: the next activation gets the whole tree.
        pub(super) fn idle(&mut self) {
            self.sent = false;
        }

        pub(super) fn follow(&mut self, t: f64, a: Accessible) {
            let label = self.announcer.take(t, &a.text, a.done);
            let other = self.last.as_ref().is_none_or(|l| l.name != a.name || l.edit != a.edit);
            if self.sent && !label && !other {
                return;
            }
            let update = vo_a11y::tree(&a, self.announcer.shown());
            self.adapter.update_if_active(|| update);
            self.sent = true;
            self.last = Some(a);
        }
    }
}

/// The live overlay's screen reader adapter, made the first time the app describes itself.
#[derive(Default)]
pub(crate) struct A11y {
    /// The app was asked once whether it has a description.
    probed: bool,
    #[cfg(feature = "a11y")]
    live: Option<imp::Live>,
}

impl A11y {
    /// After a change on the overlay at `t`, with keyboard focus (`focused`, its focus proxy)
    /// or not: `describe` asks the app (`App::accessible`).
    pub(crate) fn follow(
        &mut self,
        t: f64,
        focused: bool,
        waker: impl FnOnce() -> Waker,
        describe: impl FnOnce() -> Option<Accessible>,
    ) {
        #[cfg(feature = "a11y")]
        {
            if !self.probed {
                self.probed = true;
                let off = std::env::var("VO_A11Y").is_ok_and(|v| v.trim() == "0");
                if !off && describe().is_some() {
                    self.live = Some(imp::Live::start(waker()));
                }
                return;
            }
            if let Some(live) = self.live.as_mut() {
                live.focus(focused);
                if !live.active() {
                    live.idle();
                } else if let Some(a) = describe() {
                    live.follow(t, a);
                }
            }
        }
        #[cfg(not(feature = "a11y"))]
        let _ = (t, focused, waker, describe, self.probed);
    }

    /// The screen reader's requests since the last call (`App::accessibility`). None without
    /// the `a11y` feature or a screen reader.
    pub(crate) fn requests(&mut self) -> Vec<vo_a11y::Request> {
        #[cfg(feature = "a11y")]
        if let Some(live) = self.live.as_mut() {
            return live.requests();
        }
        Vec::new()
    }
}
