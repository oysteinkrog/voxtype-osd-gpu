//! Timeline combinators (plan decision 6): `all`, `chain`, `sequence`, `stagger`, `delay` and
//! named markers.
//!
//! A [`Timeline`] is a tree of named clips. [`Timeline::schedule`] lays it out once into a
//! [`Schedule`], a flat table of `(name, start, end)` on the timeline's own local time. Every
//! query on a schedule is a closed-form function of `t`: there is no `step(dt)` and no state,
//! so sampling frames in any order, at any rate, gives the same values.
//!
//! The timeline only says when things happen. What moves is up to the caller: a progress or
//! an eased value for an expression, a closed-form spring from a clip's start, a [`Track`]
//! sampled at the clip's local time, or the [`Schedule::cues`] fed to the app as timed
//! messages so the scene's own springs do the motion.
//!
//! [`Track`]: crate::track::Track

use std::collections::HashMap;

use crate::hash::stagger_fit;
use crate::spring::Spring;
use crate::track::CubicBezier;

/// The authoring tree. Build it with [`clip`], [`wait`], [`marker`] and the combinators.
#[derive(Clone, Debug, PartialEq)]
pub enum Timeline {
    /// A named stretch of time.
    Clip { name: String, dur: f64 },
    /// A named point in time. Takes no time.
    Marker(String),
    /// Unnamed time, for holds and gaps.
    Wait(f64),
    /// Children start together. Lasts as long as the longest.
    All(Vec<Timeline>),
    /// Children run back to back.
    Chain(Vec<Timeline>),
    /// Child `i` starts `i * gap` after the first. Ends when the last child ends.
    Sequence { gap: f64, items: Vec<Timeline> },
    /// Like `Sequence`, with the gap shrunk so the last child starts by `cap` (see
    /// [`stagger_fit`]).
    Stagger { gap: f64, cap: f64, items: Vec<Timeline> },
    /// The child starts `by` seconds late.
    Delay { by: f64, item: Box<Timeline> },
    /// Child `i` starts at its spoken time, scaled (MO-05): see [`spoken`].
    Spoken { times: Vec<Option<f64>>, fit: SpokenFit, items: Vec<Timeline> },
}

/// How spoken times become start times (MO-05): the rhythm is replayed at `scale` of its
/// speed, and the whole run is stretched or squeezed to last between `min` and `max`
/// seconds from the first start to the last.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpokenFit {
    pub scale: f64,
    pub min: f64,
    pub max: f64,
}

impl Default for SpokenFit {
    /// A quarter of the spoken time, 200 to 600 ms in all.
    fn default() -> SpokenFit {
        SpokenFit { scale: 0.25, min: 0.2, max: 0.6 }
    }
}

/// Start offsets for items said at `times` (seconds, any origin), first at 0 (MO-05). A
/// missing time sits between its neighbours' times in proportion, or after the last known
/// one at the mean gap; with fewer than two known times the items are evenly spaced over
/// `fit.min`. Times never run backwards. Under reduced motion, callers start everything at
/// once instead.
pub fn spoken_offsets(times: &[Option<f64>], fit: SpokenFit) -> Vec<f64> {
    let n = times.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let known: Vec<(usize, f64)> = times.iter().enumerate().filter_map(|(i, t)| t.map(|t| (i, t))).collect();
    let even = |total: f64| (0..n).map(|i| total * i as f64 / (n - 1) as f64).collect::<Vec<f64>>();
    if known.len() < 2 {
        return even(fit.min.max(0.0));
    }
    // Fill the gaps, then keep the order.
    let (first, last) = (known[0], known[known.len() - 1]);
    let mean = (last.1 - first.1) / (last.0 - first.0) as f64;
    let mut raw = vec![0.0; n];
    for (i, r) in raw.iter_mut().enumerate() {
        *r = match known.iter().position(|k| k.0 >= i) {
            Some(j) if known[j].0 == i => known[j].1,
            Some(0) => first.1 - mean * (first.0 - i) as f64,
            Some(j) => {
                let (a, b) = (known[j - 1], known[j]);
                a.1 + (b.1 - a.1) * (i - a.0) as f64 / (b.0 - a.0) as f64
            }
            None => last.1 + mean * (i - last.0) as f64,
        };
    }
    for i in 1..n {
        raw[i] = raw[i].max(raw[i - 1]);
    }
    let span = raw[n - 1] - raw[0];
    if span <= 1e-9 {
        return even(fit.min.max(0.0));
    }
    let total = (span * fit.scale).clamp(fit.min, fit.max.max(fit.min));
    raw.iter().map(|r| (r - raw[0]) * total / span).collect()
}

/// Starts item `i` at its spoken time `times[i]`, replayed by `fit` (MO-05). Groups of words
/// move in spoken order at a scaled copy of the spoken rhythm, not a fixed stagger.
pub fn spoken(times: Vec<Option<f64>>, fit: SpokenFit, items: impl IntoIterator<Item = Timeline>) -> Timeline {
    let items: Vec<Timeline> = items.into_iter().collect();
    let mut times = times;
    times.resize(items.len(), None);
    Timeline::Spoken { times, fit, items }
}

/// A named clip of `dur` seconds.
pub fn clip(name: impl Into<String>, dur: f64) -> Timeline {
    Timeline::Clip { name: name.into(), dur: dur.max(0.0) }
}

/// A named point in time.
pub fn marker(name: impl Into<String>) -> Timeline {
    Timeline::Marker(name.into())
}

/// `secs` of unnamed time.
pub fn wait(secs: f64) -> Timeline {
    Timeline::Wait(secs.max(0.0))
}

/// Starts every item at once.
pub fn all(items: impl IntoIterator<Item = Timeline>) -> Timeline {
    Timeline::All(items.into_iter().collect())
}

/// Runs the items one after another.
pub fn chain(items: impl IntoIterator<Item = Timeline>) -> Timeline {
    Timeline::Chain(items.into_iter().collect())
}

/// Starts item `i` at `i * gap`. Items may overlap.
pub fn sequence(gap: f64, items: impl IntoIterator<Item = Timeline>) -> Timeline {
    Timeline::Sequence { gap: gap.max(0.0), items: items.into_iter().collect() }
}

/// Starts item `i` of `n` at `stagger_fit(i, n, gap, cap)`: an even rhythm that never starts
/// the last item later than `cap`.
pub fn stagger(gap: f64, cap: f64, items: impl IntoIterator<Item = Timeline>) -> Timeline {
    Timeline::Stagger { gap: gap.max(0.0), cap: cap.max(0.0), items: items.into_iter().collect() }
}

/// Starts `item` `by` seconds late.
pub fn delay(by: f64, item: Timeline) -> Timeline {
    Timeline::Delay { by: by.max(0.0), item: Box::new(item) }
}

impl Timeline {
    /// Length in seconds.
    pub fn duration(&self) -> f64 {
        match self {
            Timeline::Clip { dur, .. } | Timeline::Wait(dur) => *dur,
            Timeline::Marker(_) => 0.0,
            Timeline::All(items) => items.iter().map(Timeline::duration).fold(0.0, f64::max),
            Timeline::Chain(items) => items.iter().map(Timeline::duration).sum(),
            Timeline::Sequence { items, .. } | Timeline::Stagger { items, .. } | Timeline::Spoken { items, .. } => {
                items.iter().zip(self.offsets()).map(|(c, off)| off + c.duration()).fold(0.0, f64::max)
            }
            Timeline::Delay { by, item } => by + item.duration(),
        }
    }

    fn items(&self) -> &[Timeline] {
        match self {
            Timeline::All(items)
            | Timeline::Chain(items)
            | Timeline::Sequence { items, .. }
            | Timeline::Stagger { items, .. }
            | Timeline::Spoken { items, .. } => items,
            Timeline::Delay { item, .. } => std::slice::from_ref(item),
            _ => &[],
        }
    }

    /// Start of each child relative to this node's start.
    fn offsets(&self) -> Vec<f64> {
        match self {
            Timeline::All(items) => vec![0.0; items.len()],
            Timeline::Chain(items) => {
                let mut at = 0.0;
                items
                    .iter()
                    .map(|c| {
                        let s = at;
                        at += c.duration();
                        s
                    })
                    .collect()
            }
            Timeline::Sequence { gap, items } => (0..items.len()).map(|i| i as f64 * gap).collect(),
            Timeline::Stagger { gap, cap, items } => {
                (0..items.len()).map(|i| stagger_fit(i, items.len(), *gap, *cap)).collect()
            }
            Timeline::Delay { by, .. } => vec![*by],
            Timeline::Spoken { times, fit, .. } => spoken_offsets(times, *fit),
            _ => Vec::new(),
        }
    }

    /// Lays the tree out on local time, starting at 0. Panics when two clips or two markers
    /// share a name, which is an authoring error.
    pub fn schedule(&self) -> Schedule {
        let mut s = Schedule { spans: Vec::new(), markers: Vec::new(), index: HashMap::new(), duration: 0.0 };
        self.lay_out(0.0, &mut s);
        s.duration = self.duration();
        // Stable, so clips that start together keep their authoring order.
        s.spans.sort_by(|a, b| a.start.total_cmp(&b.start));
        s.markers.sort_by(|a, b| a.1.total_cmp(&b.1));
        for (i, sp) in s.spans.iter().enumerate() {
            assert!(s.index.insert(sp.name.clone(), i).is_none(), "two clips named {:?}", sp.name);
        }
        for (i, m) in s.markers.iter().enumerate() {
            assert!(!s.markers[..i].iter().any(|o| o.0 == m.0), "two markers named {:?}", m.0);
        }
        s
    }

    fn lay_out(&self, at: f64, s: &mut Schedule) {
        match self {
            Timeline::Clip { name, dur } => s.spans.push(Span { name: name.clone(), start: at, end: at + dur }),
            Timeline::Marker(name) => s.markers.push((name.clone(), at)),
            Timeline::Wait(_) => {}
            _ => {
                for (child, off) in self.items().iter().zip(self.offsets()) {
                    child.lay_out(at + off, s);
                }
            }
        }
    }
}

/// One clip on the schedule, in seconds of local time.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub name: String,
    pub start: f64,
    pub end: f64,
}

impl Span {
    pub fn dur(&self) -> f64 {
        self.end - self.start
    }
}

/// Whether a cue marks a clip's start, its end, or a marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
    Marker,
}

/// A point on the schedule where something begins, ends or is marked.
#[derive(Clone, Debug, PartialEq)]
pub struct Cue<'a> {
    pub t: f64,
    pub name: &'a str,
    pub edge: Edge,
}

/// A laid-out timeline. All queries take local time `t` and are pure.
#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    /// Sorted by start, then authoring order.
    spans: Vec<Span>,
    /// Sorted by time.
    markers: Vec<(String, f64)>,
    index: HashMap<String, usize>,
    duration: f64,
}

impl Schedule {
    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// Every clip, by start time.
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub fn span(&self, name: &str) -> Option<&Span> {
        self.index.get(name).map(|&i| &self.spans[i])
    }

    /// Time of a named marker.
    pub fn marker(&self, name: &str) -> Option<f64> {
        self.markers.iter().find(|m| m.0 == name).map(|m| m.1)
    }

    fn get(&self, name: &str) -> &Span {
        self.span(name).unwrap_or_else(|| panic!("no clip named {name:?}"))
    }

    /// Seconds since the clip started; negative before it.
    pub fn local(&self, name: &str, t: f64) -> f64 {
        t - self.get(name).start
    }

    /// True from the clip's start up to, not including, its end.
    pub fn active(&self, name: &str, t: f64) -> bool {
        let s = self.get(name);
        s.start <= t && t < s.end
    }

    /// Linear progress through the clip: 0 before it, 1 after it. A zero-length clip steps
    /// from 0 to 1 at its start.
    pub fn progress(&self, name: &str, t: f64) -> f64 {
        let s = self.get(name);
        if t < s.start {
            0.0
        } else if t >= s.end {
            1.0
        } else {
            (t - s.start) / s.dur()
        }
    }

    /// [`Schedule::progress`] through an easing curve.
    pub fn eased(&self, name: &str, t: f64, ease: CubicBezier) -> f64 {
        ease.ease(self.progress(name, t))
    }

    /// A spring that rests at `from` until the clip starts, then moves to `to` from rest. Only
    /// the start matters, so a clip's length is free to mean "how long to hold" for the caller.
    pub fn spring(&self, name: &str, t: f64, spring: Spring, from: f64, to: f64) -> f64 {
        let lt = self.local(name, t);
        if lt <= 0.0 {
            return from;
        }
        to + spring.eval(from - to, 0.0, lt).0
    }

    /// Clip starts and ends and markers, in time order, shifted by `start` (the time the whole
    /// timeline starts). At equal times, ends come before markers and markers before starts, so
    /// a chain hands over cleanly. Feed these to an app as timed messages.
    pub fn cues(&self, start: f64) -> Vec<Cue<'_>> {
        let mut out: Vec<Cue> = Vec::with_capacity(self.spans.len() * 2 + self.markers.len());
        for s in &self.spans {
            out.push(Cue { t: start + s.start, name: &s.name, edge: Edge::Start });
            out.push(Cue { t: start + s.end, name: &s.name, edge: Edge::End });
        }
        for (name, t) in &self.markers {
            out.push(Cue { t: start + t, name, edge: Edge::Marker });
        }
        let rank = |e: Edge| match e {
            Edge::End => 0,
            Edge::Marker => 1,
            Edge::Start => 2,
        };
        out.sort_by(|a, b| a.t.total_cmp(&b.t).then(rank(a.edge).cmp(&rank(b.edge))));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::rand_f64;

    fn card() -> Timeline {
        chain([
            marker("in"),
            all([clip("bg", 0.4), delay(0.1, stagger(0.08, 0.2, (0..6).map(|i| clip(format!("word.{i}"), 0.3))))]),
            clip("hold", 2.0),
            marker("out"),
            sequence(0.05, [clip("exit.a", 0.3), clip("exit.b", 0.3), clip("exit.c", 0.3)]),
        ])
    }

    #[test]
    fn combinators_place_clips() {
        let s = card().schedule();
        // Stagger fits 6 items into a 0.2 s cap: gap 0.04, not 0.08.
        for i in 0..6 {
            let sp = s.span(&format!("word.{i}")).unwrap();
            assert!((sp.start - (0.1 + 0.04 * i as f64)).abs() < 1e-12, "{sp:?}");
            assert!((sp.dur() - 0.3).abs() < 1e-12);
        }
        // The `all` lasts as long as its longest child: 0.1 + 0.2 + 0.3.
        let hold = s.span("hold").unwrap();
        assert!((hold.start - 0.6).abs() < 1e-12);
        assert_eq!(s.marker("in"), Some(0.0));
        assert!((s.marker("out").unwrap() - 2.6).abs() < 1e-12);
        assert!((s.span("exit.c").unwrap().start - 2.7).abs() < 1e-12);
        assert!((s.duration() - 3.0).abs() < 1e-12);
        assert_eq!(s.spans().first().unwrap().name, "bg");
        assert!(s.spans().windows(2).all(|w| w[0].start <= w[1].start));
    }

    #[test]
    fn spoken_replays_the_rhythm_scaled_and_clamped() {
        let fit = SpokenFit::default();
        // 0, 0.4, 2.0 s spoken: a quarter is 0.5 s, inside 0.2 to 0.6.
        let o = spoken_offsets(&[Some(10.0), Some(10.4), Some(12.0)], fit);
        assert!((o[1] - 0.1).abs() < 1e-9 && (o[2] - 0.5).abs() < 1e-9, "{o:?}");
        // 8 s spoken squeezes to 0.6 s; 0.2 s spoken stretches to 0.2 s.
        assert!((spoken_offsets(&[Some(0.0), Some(8.0)], fit)[1] - 0.6).abs() < 1e-9);
        assert!((spoken_offsets(&[Some(0.0), Some(0.2)], fit)[1] - 0.2).abs() < 1e-9);
        // Missing times interpolate; no times spread evenly over the minimum.
        let o = spoken_offsets(&[Some(0.0), None, Some(2.0)], fit);
        assert!((o[1] - 0.25).abs() < 1e-9);
        assert_eq!(spoken_offsets(&[None, None, None], fit), vec![0.0, 0.1, 0.2]);
        // Never backwards.
        let o = spoken_offsets(&[Some(1.0), Some(0.5), Some(2.0)], fit);
        assert!(o[1] >= o[0] && o[2] >= o[1]);
        let s = spoken(vec![Some(0.0), Some(1.6)], fit, [clip("a", 0.1), clip("b", 0.1)]).schedule();
        assert!((s.span("b").unwrap().start - 0.4).abs() < 1e-9);
    }

    #[test]
    fn plain_stagger_and_sequence() {
        let s = sequence(0.5, [clip("a", 0.1), clip("b", 2.0), clip("c", 0.1)]).schedule();
        assert_eq!(s.span("c").unwrap().start, 1.0);
        // The longest-running item ends last, not the last-started one.
        assert_eq!(s.duration(), 2.5);
        let s = stagger(0.1, 1.0, [clip("a", 0.1), clip("b", 0.1)]).schedule();
        assert_eq!(s.span("b").unwrap().start, 0.1);
        let s = all(Vec::new()).schedule();
        assert_eq!(s.duration(), 0.0);
    }

    #[test]
    fn queries_are_closed_form() {
        let s = card().schedule();
        assert_eq!(s.progress("hold", 0.0), 0.0);
        assert!((s.progress("hold", 1.6) - 0.5).abs() < 1e-12);
        assert_eq!(s.progress("hold", 9.0), 1.0);
        assert!(s.active("hold", 0.6) && !s.active("hold", 2.6));
        assert_eq!(s.spring("word.0", 0.0, Spring::POP, 40.0, 0.0), 40.0);
        let mid = s.spring("word.0", 0.2, Spring::POP, 40.0, 0.0);
        assert!(mid < 40.0 && mid.abs() < 40.0);
        assert!(s.spring("word.0", 5.0, Spring::POP, 40.0, 0.0).abs() < 1e-6);
        assert!((s.eased("hold", 1.6, CubicBezier::LINEAR) - 0.5).abs() < 1e-12);
        let z = clip("z", 0.0);
        let s = delay(1.0, z).schedule();
        assert_eq!((s.progress("z", 0.999), s.progress("z", 1.0)), (0.0, 1.0));
    }

    /// Every value the card's schedule gives at `t`, as bits.
    fn sample(s: &Schedule, t: f64) -> Vec<u64> {
        s.spans()
            .iter()
            .flat_map(|sp| {
                [
                    s.progress(&sp.name, t),
                    s.eased(&sp.name, t, CubicBezier::OUT),
                    s.spring(&sp.name, t, Spring::POP, -30.0, 0.0),
                    s.spring(&sp.name, t, Spring::SLOW, 0.0, 1.0),
                ]
            })
            .map(f64::to_bits)
            .collect()
    }

    #[test]
    fn seek_in_any_order_gives_the_same_values() {
        let s = card().schedule();
        let frames: Vec<f64> = (0..240).map(|n| crate::time::FrameTime::at_rate(n, 60.0).t).collect();
        let forward: Vec<Vec<u64>> = frames.iter().map(|&t| sample(&s, t)).collect();
        // A shuffled order from the seeded hash, and backwards.
        let mut order: Vec<usize> = (0..frames.len()).collect();
        order.sort_by(|&a, &b| rand_f64(7, a as u32, 0, 0).total_cmp(&rand_f64(7, b as u32, 0, 0)));
        for i in order {
            assert_eq!(sample(&s, frames[i]), forward[i], "frame {i}");
        }
        for i in (0..frames.len()).rev() {
            assert_eq!(sample(&s, frames[i]), forward[i], "frame {i}");
        }
        // A second layout of the same tree is identical.
        assert_eq!(card().schedule(), s);
    }

    #[test]
    fn cues_are_in_time_order() {
        let s = chain([clip("a", 1.0), marker("m"), clip("b", 1.0)]).schedule();
        let cues: Vec<_> = s.cues(10.0).into_iter().map(|c| (c.t, c.name, c.edge)).collect();
        assert_eq!(
            cues,
            vec![
                (10.0, "a", Edge::Start),
                (11.0, "a", Edge::End),
                (11.0, "m", Edge::Marker),
                (11.0, "b", Edge::Start),
                (12.0, "b", Edge::End),
            ]
        );
        assert!(card().schedule().cues(0.0).windows(2).all(|w| w[0].t <= w[1].t));
    }

    #[test]
    #[should_panic(expected = "two clips named")]
    fn duplicate_names_panic() {
        all([clip("a", 1.0), clip("a", 2.0)]).schedule();
    }
}
