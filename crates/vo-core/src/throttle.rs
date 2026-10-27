//! A rate limit for jittery values (T21): a telemetry number that arrives at 20 Hz should
//! change on screen at most once every 500 ms, or its digits never stop rolling.
//!
//! [`Throttle`] decides from event times only. The first value shows at once. A value that
//! arrives sooner than `interval` after the last one shown waits; a later value replaces it.
//! The waiting value shows at [`Throttle::due`], when the app's timer calls
//! [`Throttle::flush`], so the last reading always reaches the screen and nothing waits more
//! than `interval`.

/// Shows a value at most once per `interval` seconds of event time.
#[derive(Clone, Debug, PartialEq)]
pub struct Throttle<T> {
    interval: f64,
    shown: Option<(f64, T)>,
    pending: Option<T>,
}

impl<T: Clone> Throttle<T> {
    pub fn new(interval: f64) -> Throttle<T> {
        Throttle { interval: interval.max(0.0), shown: None, pending: None }
    }

    /// A new reading at `t`. Returns the value to show now, or `None` when it must wait for
    /// [`Throttle::due`].
    pub fn push(&mut self, t: f64, value: T) -> Option<T> {
        match &self.shown {
            Some((at, _)) if t < at + self.interval => {
                self.pending = Some(value);
                None
            }
            _ => {
                self.pending = None;
                self.shown = Some((t, value.clone()));
                Some(value)
            }
        }
    }

    /// When the waiting value may show, if one waits.
    pub fn due(&self) -> Option<f64> {
        self.pending.as_ref()?;
        self.shown.as_ref().map(|(at, _)| at + self.interval)
    }

    /// Shows the waiting value if it is due at `t`. Returns it when it did.
    pub fn flush(&mut self, t: f64) -> Option<T> {
        if self.due().is_some_and(|d| t >= d) {
            let v = self.pending.take().expect("due means pending");
            self.shown = Some((t, v.clone()));
            return Some(v);
        }
        None
    }

    /// The value on screen.
    pub fn shown(&self) -> Option<&T> {
        self.shown.as_ref().map(|(_, v)| v)
    }

    /// When the value on screen was shown.
    pub fn shown_at(&self) -> Option<f64> {
        self.shown.as_ref().map(|(t, _)| *t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_value_shows_and_bursts_wait() {
        let mut th = Throttle::new(0.5);
        assert_eq!(th.push(1.0, 186), Some(186));
        assert_eq!(th.push(1.05, 172), None);
        assert_eq!(th.push(1.10, 175), None);
        assert_eq!(th.due(), Some(1.5));
        assert_eq!(th.flush(1.4), None);
        assert_eq!(th.flush(1.5), Some(175), "the latest reading wins");
        assert_eq!(th.due(), None);
        assert_eq!(th.shown(), Some(&175));
        // A reading after a quiet spell shows at once.
        assert_eq!(th.push(3.0, 180), Some(180));
        assert_eq!(th.shown_at(), Some(3.0));
    }

    #[test]
    fn a_20_hz_stream_changes_at_most_twice_a_second() {
        let mut th = Throttle::new(0.5);
        let mut shown = Vec::new();
        for i in 0..200 {
            let t = i as f64 / 20.0;
            if let Some(v) = th.flush(t) {
                shown.push((t, v));
            }
            if let Some(v) = th.push(t, i) {
                shown.push((t, v));
            }
        }
        assert!(shown.windows(2).all(|w| w[1].0 - w[0].0 >= 0.5 - 1e-9), "{shown:?}");
        assert_eq!(shown.len(), 20);
        // Every shown value was the newest reading at the time it showed.
        assert!(shown.iter().all(|&(t, v)| v as f64 <= t * 20.0 + 1e-9));
    }
}
