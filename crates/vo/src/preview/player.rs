//! Seek: the scene at any `t`, exactly as a forward run or an export shows it.
//!
//! Nodes that finish leaving are dropped and channels only know the events applied so far, so
//! a scene cannot be sampled before its last event. [`Player`] keeps one driver and moves it:
//!
//! - forward: applies the events up to the new `t`, as export does;
//! - back, with no event between the new `t` and the last one applied: samples as it is,
//!   because channels keep their whole history and nothing else changed since;
//! - back across an event: starts a fresh app and replays the scenario from 0.
//!
//! Every path applies the same events in the same order as the offline render, so the display
//! list at `t` is the one an export samples at `t`.

use std::time::Instant;

use vo_scene::display::DisplayList;

use crate::app::{App, Driver, Tokens};
use crate::offline::{advance, start, Scenario};
use crate::Options;

/// What the last seek did, for the stats overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SeekStats {
    /// Seconds spent in the last [`Player::seek`].
    pub secs: f64,
    /// Events applied by the last seek.
    pub events: usize,
    /// The last seek started the app again from 0.
    pub replayed: bool,
    /// Replays since the player was made.
    pub replays: u64,
}

/// A scene that can be sampled at any time, in any order.
pub struct Player<A: App> {
    make: Box<dyn Fn() -> A>,
    options: Box<dyn Fn() -> Options<A::Msg>>,
    scenario: Scenario<A::Msg>,
    /// Replaces the options' tokens, for live reload.
    tokens: Option<Tokens>,
    driver: Driver<A>,
    /// Index of the next scenario input to apply.
    next: usize,
    /// Every event at or before this time has been applied.
    pos: f64,
    /// Time of the last applied event, or minus infinity.
    last_event: f64,
    stats: SeekStats,
}

impl<A: App> Player<A> {
    /// `make` builds the app in its starting state and `options` its fonts and tokens; both run
    /// again on every replay.
    pub fn new(
        make: impl Fn() -> A + 'static,
        options: impl Fn() -> Options<A::Msg> + 'static,
        scenario: Scenario<A::Msg>,
    ) -> Player<A> {
        let mut opts = options();
        let driver = start(make(), &scenario, &mut opts);
        Player {
            make: Box::new(make),
            options: Box::new(options),
            scenario,
            tokens: None,
            driver,
            next: 0,
            pos: 0.0,
            last_event: f64::NEG_INFINITY,
            stats: SeekStats::default(),
        }
    }

    pub fn scenario(&self) -> &Scenario<A::Msg> {
        &self.scenario
    }

    pub fn stats(&self) -> SeekStats {
        self.stats
    }

    /// The scene as the last [`Player::seek`] left it.
    pub fn scene(&self) -> &vo_scene::Scene<A::Msg> {
        &self.driver.scene
    }

    /// The options a run starts with: the app's own, with reloaded tokens if any.
    pub fn options(&self) -> Options<A::Msg> {
        let mut opts = (self.options)();
        if let Some(t) = &self.tokens {
            opts.tokens = t.clone();
        }
        opts
    }

    /// New tokens: the app replays from 0 with them, so every view sees them at its own time.
    pub fn set_tokens(&mut self, tokens: Tokens) {
        self.tokens = Some(tokens);
        self.restart();
    }

    fn restart(&mut self) {
        let mut opts = self.options();
        self.driver = start((self.make)(), &self.scenario, &mut opts);
        self.next = 0;
        self.pos = 0.0;
        self.last_event = f64::NEG_INFINITY;
        self.stats.replays += 1;
    }

    /// Moves the scene so that sampling it at `t` gives the frame a forward run gives at `t`.
    pub fn seek(&mut self, t: f64) {
        let started = Instant::now();
        let replayed = t < self.last_event;
        if replayed {
            self.restart();
        }
        let mut events = 0;
        if t > self.pos {
            let (n, last) = advance(&mut self.driver, &self.scenario.inputs, &mut self.next, t);
            events = n;
            if let Some(at) = last {
                self.last_event = at;
            }
            self.pos = t;
        }
        self.stats = SeekStats { secs: started.elapsed().as_secs_f64(), events, replayed, replays: self.stats.replays };
    }

    /// Seeks to `t` and samples the scene there.
    pub fn sample_into(&mut self, t: f64, out: &mut DisplayList) {
        self.seek(t);
        self.driver.scene.sample_into(t, out);
    }

    /// The times of every event up to `until` (scenario inputs and the app's timers), from a
    /// run of its own. For the event ticks on the timeline.
    pub fn event_times(&self, until: f64) -> Vec<f64> {
        let mut opts = self.options();
        let mut driver = start((self.make)(), &self.scenario, &mut opts);
        let mut next = 0;
        let mut times = Vec::new();
        loop {
            let input_t = self.scenario.inputs.get(next).map(|(t, _)| *t);
            let Some(at) = [input_t, driver.timers.next_at()].into_iter().flatten().min_by(f64::total_cmp) else {
                break;
            };
            if at > until {
                break;
            }
            // Everything at exactly `at`, so each event keeps its own time.
            let (n, _) = advance(&mut driver, &self.scenario.inputs, &mut next, at);
            if n == 0 {
                break;
            }
            times.extend(std::iter::repeat_n(at, n));
        }
        times
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Cx;
    use crate::Input;
    use vo_core::hash;
    use vo_core::time::FrameTime;
    use vo_scene::element::{Element, Transition};
    use vo_scene::geom::{Color, Point, Size};
    use vo_scene::input::{Button, PointerKind};

    /// Items come and go on timers and leave with an exit motion; a click on the first one
    /// removes it.
    struct Blink {
        shown: Vec<bool>,
    }

    #[derive(Clone, Debug)]
    enum Msg {
        Toggle(usize),
        Drop,
    }

    impl App for Blink {
        type Msg = Msg;

        fn init(&mut self, cx: &mut Cx<Msg>) {
            for i in 0..6 {
                cx.after(0.3 + i as f64 * 0.45, Msg::Toggle(i % 3));
            }
        }

        fn update(&mut self, msg: Msg, cx: &mut Cx<Msg>) {
            match msg {
                Msg::Toggle(i) => {
                    self.shown[i] = !self.shown[i];
                    if i == 2 {
                        cx.after(0.2, Msg::Toggle(1));
                    }
                }
                Msg::Drop => self.shown[0] = false,
            }
        }

        fn view(&self, tokens: &Tokens) -> Element<Msg> {
            let w = tokens.num("w", 60.0);
            let items = (0..3).filter(|i| self.shown[*i]).map(|i| {
                Element::panel(format!("i{i}"))
                    .size(w, 30.0)
                    .fill(Color::WHITE)
                    .presence(Transition::fade().offset(0.0, 20.0), Transition::fade().scale(0.5))
                    .on_click(Msg::Drop)
            });
            Element::row("root").gap(8.0).children(items)
        }
    }

    fn player() -> Player<Blink> {
        let at = Point::new(20.0, 10.0);
        let scenario = Scenario {
            size: Size::new(300.0, 80.0),
            scale: 1.0,
            duration: 3.0,
            inputs: vec![
                (1.1, Input::Pointer { pos: at, kind: PointerKind::Enter }),
                (1.2, Input::Pointer { pos: at, kind: PointerKind::Down(Button::Primary) }),
                (1.25, Input::Pointer { pos: at, kind: PointerKind::Up(Button::Primary) }),
                (2.0, Input::Msg(Msg::Toggle(0))),
            ],
        };
        Player::new(|| Blink { shown: vec![true, false, true] }, Options::default, scenario)
    }

    fn sample<A: App>(p: &mut Player<A>, t: f64) -> DisplayList {
        let mut l = DisplayList::default();
        p.sample_into(t, &mut l);
        l
    }

    #[test]
    fn any_seek_order_gives_the_forward_frames() {
        let fps = 60.0;
        let n = (3.0 * fps) as u64 + 1;
        let t = |i: u64| FrameTime::at_rate(i, fps).t;
        let mut fwd = player();
        let reference: Vec<DisplayList> = (0..n).map(|i| sample(&mut fwd, t(i))).collect();
        let mut p = player();
        for i in (0..n).rev() {
            assert_eq!(sample(&mut p, t(i)), reference[i as usize], "backwards, frame {i}");
        }
        for k in 0..400u32 {
            let i = (hash::rand_f64(7, k, 0, 0) * n as f64) as u64 % n;
            assert_eq!(sample(&mut p, t(i)), reference[i as usize], "random, frame {i}");
        }
        assert!(p.stats().replays > 0);
    }

    #[test]
    fn a_seek_back_inside_one_event_gap_does_not_replay() {
        let mut p = player();
        p.seek(2.9);
        let replays = p.stats().replays;
        // The last event before 2.9 is at 2.75 (a timer).
        p.seek(2.8);
        assert_eq!(p.stats().replays, replays);
        p.seek(0.1);
        assert_eq!(p.stats().replays, replays + 1);
    }

    #[test]
    fn new_tokens_replay_to_the_same_time() {
        let mut p = player();
        let before = sample(&mut p, 1.0);
        let mut tokens = Tokens::default();
        tokens.numbers.insert("w".into(), 90.0);
        p.set_tokens(tokens);
        let after = sample(&mut p, 1.0);
        assert_ne!(before, after);
    }

    #[test]
    fn event_times_list_inputs_and_timers() {
        let times = player().event_times(3.0);
        assert!(times.contains(&0.3) && times.contains(&2.0) && times.contains(&1.2), "{times:?}");
        assert!(times.windows(2).all(|w| w[0] <= w[1]));
    }
}
