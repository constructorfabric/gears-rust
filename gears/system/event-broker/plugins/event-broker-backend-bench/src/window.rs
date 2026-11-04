//! Per-second latency windows for the soak.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::Serialize;
use tokio::time::Instant;

use crate::stats::{Histogram, Percentiles};

/// One second of the soak, by when its events were read.
#[derive(Debug, Clone, Serialize)]
pub struct WindowStat {
    /// Seconds since measurement began.
    pub second: u64,
    /// Scheduled send to the consumer seeing a head that covers the event.
    pub head: Percentiles,
    /// Scheduled send to the consumer's read returning the event.
    pub read: Percentiles,
}

/// Histograms keyed by the second their samples were read in. Keyed by read
/// time rather than send time, a stall shows in the window the stalled reads
/// finish in. Each consumer fills its current second alone and adds it here
/// when it moves on, so the lock is taken about once a second per consumer.
pub struct Windows {
    /// When measurement began; set once the phase's clock starts.
    from: OnceLock<Instant>,
    seconds: Mutex<BTreeMap<u64, (Histogram, Histogram)>>,
}

impl std::fmt::Debug for Windows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Windows")
            .field("from", &self.from)
            .finish_non_exhaustive()
    }
}

impl Windows {
    /// Empty, with measurement not yet started.
    pub fn new() -> Self {
        Self {
            from: OnceLock::new(),
            seconds: Mutex::default(),
        }
    }

    /// Second 0 begins at `from`; later calls change nothing.
    pub fn start(&self, from: Instant) {
        self.from.get_or_init(|| from);
    }

    /// The second `at` falls in; `None` before measurement began.
    pub fn second(&self, at: Instant) -> Option<u64> {
        let from = *self.from.get()?;
        (at >= from).then(|| (at - from).as_secs())
    }

    /// Merges a consumer's histograms of `second` into its window.
    pub fn add(&self, second: u64, read: &Histogram, head: &Histogram) {
        if read.is_empty() && head.is_empty() {
            return;
        }
        let mut seconds = self.seconds.lock().unwrap_or_else(PoisonError::into_inner);
        let (r, h) = seconds.entry(second).or_default();
        r.merge(read);
        h.merge(head);
    }

    /// Every window merged: the whole soak's (read, head) latencies.
    pub fn merged(&self) -> (Histogram, Histogram) {
        let seconds = self.seconds.lock().unwrap_or_else(PoisonError::into_inner);
        let mut all = (Histogram::default(), Histogram::default());
        for (read, head) in seconds.values() {
            all.0.merge(read);
            all.1.merge(head);
        }
        all
    }

    /// Every window, oldest first.
    pub fn collect(&self) -> Vec<WindowStat> {
        self.seconds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(second, (read, head))| WindowStat {
                second: *second,
                head: head.percentiles(),
                read: read.percentiles(),
            })
            .collect()
    }
}
