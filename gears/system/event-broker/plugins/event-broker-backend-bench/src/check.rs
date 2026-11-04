//! The correctness check every cell ends with.

use std::time::Duration;

use event_broker_sdk::backend::Backend;
use event_broker_sdk::{GtsInstanceId, Position, Sequence};
use serde::Serialize;
use tokio::time::Instant;

use crate::consume::ReadTally;
use crate::produce::WriteTally;
use crate::workload::{PARTITIONS, partition};

/// How long heads may take to account for every acked event: `Accepted`
/// events become visible later.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long heads must hold still to count as settled when some acked append
/// reported no head to wait for.
const SETTLE_QUIET: Duration = Duration::from_secs(1);

/// A cell's check: empty `failures` is a pass.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Verdict {
    /// Why the check failed, one line per broken expectation.
    pub failures: Vec<String>,
}

impl Verdict {
    /// Whether no expectation broke.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    /// Records a broken expectation.
    pub fn fail(&mut self, failure: String) {
        self.failures.push(failure);
    }

    fn require(&mut self, holds: bool, failure: impl FnOnce() -> String) {
        if !holds {
            self.failures.push(failure());
        }
    }
}

/// Whether the consumers were expected to read everything stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drain {
    /// Every stored event should have been read.
    Full,
    /// Stopped at a deadline; what was read is checked, not that it is all.
    Partial,
}

/// The head of every partition of `topic`, once each reaches the highest head
/// an append answer reported there - and, when some acked append reported
/// none, the heads have held still for [`SETTLE_QUIET`] - or the settle
/// deadline passes. Sequences may have gaps, so heads say nothing about how
/// many events are stored; the consumers count those.
pub async fn settle_heads(
    backend: &dyn Backend,
    topic: &GtsInstanceId,
    write: &WriteTally,
) -> Vec<Sequence> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut previous = Vec::new();
    let mut still_since = Instant::now();
    loop {
        let mut heads = Vec::with_capacity(PARTITIONS as usize);
        for p in 0..PARTITIONS {
            heads.push(
                backend
                    .resolve(&partition(topic, p), Position::Latest)
                    .await
                    .unwrap_or(Sequence::NONE),
            );
        }
        let now = Instant::now();
        if heads != previous {
            still_since = now;
        }
        let reached = write
            .reported_heads
            .iter()
            .all(|(p, reported)| heads.get(*p as usize).is_some_and(|head| head >= reported));
        let quiet = write.unreported == 0 || now - still_since >= SETTLE_QUIET;
        if (reached && quiet) || now >= deadline {
            return heads;
        }
        previous = heads;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Checks a cell's writes against what was read.
pub fn judge(write: &WriteTally, read: &ReadTally, drain: Drain) -> Verdict {
    let mut verdict = Verdict::default();
    match drain {
        Drain::Full => {
            verdict.require(read.consumed == write.acked, || {
                format!("read {} != acked {}", read.consumed, write.acked)
            });
            verdict.require(read.drained_at.is_some(), || {
                "consumers did not reach the heads".to_owned()
            });
        }
        Drain::Partial => verdict.require(read.consumed <= write.acked, || {
            format!("read {} > acked {}", read.consumed, write.acked)
        }),
    }
    verdict.require(read.sequence_breaks == 0, || {
        format!(
            "{} sequences not above the one read before",
            read.sequence_breaks
        )
    });
    verdict.require(read.order_breaks == 0, || {
        format!("{} events out of key order", read.order_breaks)
    });
    verdict.require(read.unstamped == 0, || {
        format!("{} events without a bench stamp", read.unstamped)
    });
    verdict.require(write.unexpected_duplicates == 0, || {
        format!(
            "{} events answered Duplicate on a first send",
            write.unexpected_duplicates
        )
    });
    verdict.require(write.lost == 0, || {
        format!("{} events of fences that failed every attempt", write.lost)
    });
    verdict
}
