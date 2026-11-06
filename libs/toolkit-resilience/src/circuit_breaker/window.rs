use std::time::Duration;

use tokio::time::Instant;

/// Buckets the window is split into. Memory is fixed at this many counters
/// whatever the call rate; a bucket's worth of the oldest calls leaves the
/// window at once.
const BUCKETS: usize = 10;

#[derive(Clone, Copy, Default)]
struct Bucket {
    tick: u64,
    total: u32,
    failures: u32,
}

/// Call outcomes over the last `window`, in `BUCKETS` time slices.
pub(super) struct Window {
    origin: Instant,
    bucket_nanos: u128,
    buckets: [Bucket; BUCKETS],
}

impl Window {
    pub(super) fn new(window: Duration, origin: Instant) -> Self {
        let bucket_nanos = window
            .as_nanos()
            .checked_div(BUCKETS as u128)
            .unwrap_or(1)
            .max(1);
        Self {
            origin,
            bucket_nanos,
            buckets: [Bucket::default(); BUCKETS],
        }
    }

    fn tick(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.origin).as_nanos();
        let tick = elapsed.checked_div(self.bucket_nanos).unwrap_or(0);
        u64::try_from(tick).unwrap_or(u64::MAX)
    }

    fn slot(tick: u64) -> usize {
        // `BUCKETS` fits in u64 and the remainder fits back in usize.
        usize::try_from(tick % BUCKETS as u64).unwrap_or(0)
    }

    pub(super) fn record(&mut self, now: Instant, failure: bool) {
        let tick = self.tick(now);
        let bucket = &mut self.buckets[Self::slot(tick)];
        if bucket.tick != tick || bucket.total == 0 {
            *bucket = Bucket {
                tick,
                total: 0,
                failures: 0,
            };
        }
        bucket.total = bucket.total.saturating_add(1);
        if failure {
            bucket.failures = bucket.failures.saturating_add(1);
        }
    }

    /// `(calls, failures)` recorded in the window ending at `now`.
    pub(super) fn totals(&self, now: Instant) -> (u32, u32) {
        let tick = self.tick(now);
        self.buckets
            .iter()
            .filter(|b| b.total > 0 && b.tick <= tick && tick - b.tick < BUCKETS as u64)
            .fold((0u32, 0u32), |(t, f), b| {
                (t.saturating_add(b.total), f.saturating_add(b.failures))
            })
    }

    pub(super) fn clear(&mut self) {
        self.buckets = [Bucket::default(); BUCKETS];
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn calls_older_than_the_window_drop_out() {
        let origin = Instant::now();
        let mut w = Window::new(Duration::from_secs(10), origin);
        w.record(origin, true);
        w.record(origin + Duration::from_secs(5), false);

        assert_eq!(w.totals(origin + Duration::from_secs(9)), (2, 1));
        assert_eq!(w.totals(origin + Duration::from_secs(10)), (1, 0));
        assert_eq!(w.totals(origin + Duration::from_secs(16)), (0, 0));
    }

    #[test]
    fn a_reused_slot_starts_from_zero() {
        let origin = Instant::now();
        let mut w = Window::new(Duration::from_secs(10), origin);
        w.record(origin, true);
        w.record(origin + Duration::from_secs(10), false);

        assert_eq!(w.totals(origin + Duration::from_secs(10)), (1, 0));
    }
}
