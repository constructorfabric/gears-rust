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

/// A cell's check: empty `failures` is a pass.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Verdict {
    pub failures: Vec<String>,
}

impl Verdict {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

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
    Full,
    /// Stopped at a deadline; what was read is checked, not that it is all.
    Partial,
}

/// The head of every partition of `topic`, once they add up to `acked` or the
/// settle deadline passes. Sequences start at 1 with no gaps, so the heads'
/// sum is the number of events stored.
pub async fn settle_heads(
    backend: &dyn Backend,
    topic: &GtsInstanceId,
    acked: u64,
) -> Vec<Sequence> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
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
        if stored(&heads) >= acked || Instant::now() >= deadline {
            return heads;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub fn stored(heads: &[Sequence]) -> u64 {
    heads
        .iter()
        .map(|head| u64::try_from(head.as_i64()).unwrap_or(0))
        .sum()
}

/// Checks a cell's writes against what is stored and what was read.
pub fn judge(write: &WriteTally, read: &ReadTally, stored: u64, drain: Drain) -> Verdict {
    let mut verdict = Verdict::default();
    verdict.require(stored == write.acked, || {
        format!("stored {stored} != acked {}", write.acked)
    });
    match drain {
        Drain::Full => {
            verdict.require(read.consumed == write.acked, || {
                format!("read {} != acked {}", read.consumed, write.acked)
            });
            verdict.require(read.drained_at.is_some(), || {
                "consumers did not reach the heads".to_owned()
            });
        }
        Drain::Partial => verdict.require(read.consumed <= stored, || {
            format!("read {} > stored {stored}", read.consumed)
        }),
    }
    verdict.require(read.sequence_breaks == 0, || {
        format!("{} sequence gaps or repeats", read.sequence_breaks)
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
