//! Latency percentiles and the rate arithmetic the report prints.

use std::time::Duration;

use serde::Serialize;

/// Values below this are counted exactly; above it, each power of two is
/// split into [`HALF`] buckets.
const EXACT: u64 = 128;
const HALF: u64 = 64;
const HALF_BITS: u32 = 6;
/// `EXACT` exact buckets, then `HALF` for each power of two from 2^7 to 2^63.
const BUCKETS: usize = 128 + 57 * 64;

/// A constant-memory latency histogram in nanoseconds: log-linear buckets,
/// HDR-style, with a relative error under 1/64 (about 1.6 %), plus the exact
/// maximum. About 29 KiB whatever the number of samples.
#[derive(Clone)]
pub struct Histogram {
    counts: Box<[u64]>,
    samples: u64,
    max: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: vec![0; BUCKETS].into_boxed_slice(),
            samples: 0,
            max: 0,
        }
    }
}

impl std::fmt::Debug for Histogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Histogram")
            .field("samples", &self.samples)
            .field("max", &self.max)
            .finish_non_exhaustive()
    }
}

impl Histogram {
    /// Records one sample.
    pub fn record(&mut self, nanos: u64) {
        self.record_n(nanos, 1);
    }

    /// Records `nanos` `count` times: the events of one batch share a latency.
    pub fn record_n(&mut self, nanos: u64, count: u64) {
        if count == 0 {
            return;
        }
        self.counts[bucket(nanos)] += count;
        self.samples += count;
        self.max = self.max.max(nanos);
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples == 0
    }

    /// Adds `other`'s samples.
    pub fn merge(&mut self, other: &Self) {
        for (mine, theirs) in self.counts.iter_mut().zip(other.counts.iter()) {
            *mine += theirs;
        }
        self.samples += other.samples;
        self.max = self.max.max(other.max);
    }

    /// Nearest-rank percentiles: each is the highest value of the bucket
    /// holding the sample at that rank, never above the exact maximum.
    #[must_use]
    pub fn percentiles(&self) -> Percentiles {
        if self.samples == 0 {
            return Percentiles::default();
        }
        let at = |per_mille: u64| self.value_at_rank(nearest_rank(self.samples, per_mille));
        Percentiles {
            samples: self.samples,
            p50_ns: at(500),
            p90_ns: at(900),
            p99_ns: at(990),
            p99_9_ns: at(999),
            max_ns: self.max,
        }
    }

    fn value_at_rank(&self, rank: u64) -> u64 {
        let mut seen = 0;
        for (index, count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= rank {
                return highest_in(index).min(self.max);
            }
        }
        self.max
    }
}

fn bucket(nanos: u64) -> usize {
    if nanos < EXACT {
        return usize::try_from(nanos).unwrap_or(0);
    }
    // `nanos >> shift` keeps the top seven bits: in [HALF, EXACT).
    let shift = u64::from(nanos.ilog2() - HALF_BITS);
    let index = EXACT + (shift - 1) * HALF + ((nanos >> shift) - HALF);
    usize::try_from(index).unwrap_or(BUCKETS - 1)
}

fn highest_in(index: usize) -> u64 {
    let index = index as u64;
    if index < EXACT {
        return index;
    }
    let shift = ((index - EXACT) >> HALF_BITS) + 1;
    let top = ((index - EXACT) & (HALF - 1)) + HALF;
    // The top bucket's bound is 2^64, which wraps to 0: minus one is u64::MAX.
    ((top + 1) << shift).wrapping_sub(1)
}

/// Nearest-rank percentiles of a set of latencies, in nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Percentiles {
    pub samples: u64,
    pub p50_ns: u64,
    pub p90_ns: u64,
    pub p99_ns: u64,
    pub p99_9_ns: u64,
    pub max_ns: u64,
}

/// `ceil(len * q)` with `q = per_mille / 1000`, kept within `1..=len`.
fn nearest_rank(len: u64, per_mille: u64) -> u64 {
    len.saturating_mul(per_mille).div_ceil(1000).clamp(1, len)
}

/// A duration in nanoseconds, saturating.
pub fn nanos(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
}

/// `count` per second of `over`; 0 when `over` is zero.
#[allow(
    clippy::cast_precision_loss,
    reason = "a count turned into a rate; exact below 2^53"
)]
pub fn per_second(count: u64, over: Duration) -> f64 {
    let seconds = over.as_secs_f64();
    if seconds > 0.0 {
        count as f64 / seconds
    } else {
        0.0
    }
}

/// Bytes as MiB.
#[allow(clippy::cast_precision_loss, reason = "a byte count printed in MiB")]
pub fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

/// The nanoseconds between two scheduled sends of one writer.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a positive, finite period well inside u64 nanoseconds"
)]
pub fn period_nanos(events_per_append: usize, writers: usize, events_per_sec: f64) -> u64 {
    let appends_per_sec = events_per_sec / (events_per_append * writers) as f64;
    if appends_per_sec > 0.0 {
        (1e9 / appends_per_sec).round().max(1.0) as u64
    } else {
        u64::MAX
    }
}

/// Nanoseconds as milliseconds, three decimals.
#[allow(
    clippy::cast_precision_loss,
    reason = "a latency printed in milliseconds"
)]
pub fn ms(nanos: u64) -> String {
    format!("{:.3}", nanos as f64 / 1e6)
}
