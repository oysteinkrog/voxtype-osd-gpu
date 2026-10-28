//! Snapshot policy for [`crate::Timeline`].
//!
//! A snapshot is taken every `interval` ticks (default [`DEFAULT_INTERVAL_TICKS`], one second
//! at 120 Hz). The store keeps them sorted by tick. When the bytes held would go over the
//! memory cap (default [`DEFAULT_CAP_BYTES`]), every second snapshot is dropped and the
//! interval doubles, so the store still covers the whole run, just more coarsely. Memory
//! never goes over the cap, as long as one snapshot fits in it.
//!
//! Seek cost: a seek restores the nearest snapshot at or before the target and steps
//! forward, so it runs at most `interval` ticks (`interval * cap_doublings` after thinning).
//! The start state needs no snapshot: it comes from [`crate::Sim::reset`].

/// One second at the default 120 Hz tick.
pub const DEFAULT_INTERVAL_TICKS: u64 = 120;

/// 32 MiB. A 256x256 RGBA f32 grid (1 MiB) keeps 32 snapshots before thinning.
pub const DEFAULT_CAP_BYTES: usize = 32 << 20;

/// When to snapshot and how much memory snapshots may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotPolicy {
    /// Ticks between snapshots. At least 1.
    pub interval: u64,
    /// The most bytes the stored snapshots may hold together.
    pub cap_bytes: usize,
}

impl Default for SnapshotPolicy {
    fn default() -> SnapshotPolicy {
        SnapshotPolicy { interval: DEFAULT_INTERVAL_TICKS, cap_bytes: DEFAULT_CAP_BYTES }
    }
}

/// Snapshots sorted by tick, with their sizes.
pub struct SnapshotStore<T> {
    policy: SnapshotPolicy,
    interval: u64,
    items: Vec<(u64, usize, T)>,
    bytes: usize,
}

impl<T> SnapshotStore<T> {
    pub fn new(policy: SnapshotPolicy) -> SnapshotStore<T> {
        SnapshotStore { policy, interval: policy.interval.max(1), items: Vec::new(), bytes: 0 }
    }

    pub fn policy(&self) -> SnapshotPolicy {
        self.policy
    }

    /// The current interval: the policy's, doubled once per thinning.
    pub fn interval(&self) -> u64 {
        self.interval
    }

    /// Bytes the stored snapshots hold.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether a snapshot is due at `tick` and not stored yet. Tick 0 never is.
    pub fn wants(&self, tick: u64) -> bool {
        tick > 0 && tick.is_multiple_of(self.interval) && self.items.binary_search_by_key(&tick, |s| s.0).is_err()
    }

    /// Stores a snapshot of `bytes` bytes taken at `tick`, thinning first if it would go over
    /// the cap. A snapshot larger than the whole cap is not stored.
    pub fn insert(&mut self, tick: u64, bytes: usize, snap: T) {
        if bytes > self.policy.cap_bytes {
            return;
        }
        while self.bytes + bytes > self.policy.cap_bytes {
            self.thin();
        }
        if !tick.is_multiple_of(self.interval) {
            // Thinning moved the grid away from this tick.
            return;
        }
        let at = self.items.partition_point(|s| s.0 < tick);
        if self.items.get(at).is_some_and(|s| s.0 == tick) {
            return;
        }
        self.items.insert(at, (tick, bytes, snap));
        self.bytes += bytes;
    }

    /// The latest snapshot at or before `tick`.
    pub fn at_or_before(&self, tick: u64) -> Option<(u64, &T)> {
        let at = self.items.partition_point(|s| s.0 <= tick);
        at.checked_sub(1).map(|i| (self.items[i].0, &self.items[i].2))
    }

    /// Drops every snapshot at `tick` or later (an event changed what happens from there).
    pub fn invalidate_from(&mut self, tick: u64) {
        let at = self.items.partition_point(|s| s.0 < tick);
        for s in self.items.drain(at..) {
            self.bytes -= s.1;
        }
    }

    /// Doubles the interval and keeps only the snapshots on the new grid.
    fn thin(&mut self) {
        self.interval *= 2;
        let interval = self.interval;
        let mut bytes = 0;
        self.items.retain(|s| {
            let keep = s.0.is_multiple_of(interval);
            if keep {
                bytes += s.1;
            }
            keep
        });
        self.bytes = bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinning_keeps_memory_under_the_cap() {
        let mut s = SnapshotStore::new(SnapshotPolicy { interval: 10, cap_bytes: 100 });
        for tick in (10..=2000).step_by(10) {
            if s.wants(tick) {
                s.insert(tick, 30, ());
            }
            assert!(s.bytes() <= 100);
        }
        assert!(s.interval() > 10);
        assert!(s.len() >= 2);
        assert_eq!(s.at_or_before(5).map(|x| x.0), None);
        assert_eq!(s.at_or_before(2000).map(|x| x.0 % s.interval()), Some(0));
    }

    #[test]
    fn invalidate_drops_later_snapshots() {
        let mut s = SnapshotStore::new(SnapshotPolicy { interval: 10, cap_bytes: 1000 });
        for tick in [10, 20, 30] {
            s.insert(tick, 5, tick);
        }
        s.invalidate_from(20);
        assert_eq!(s.len(), 1);
        assert_eq!(s.bytes(), 5);
        assert_eq!(s.at_or_before(100).map(|x| x.0), Some(10));
        assert!(s.wants(20));
    }
}
