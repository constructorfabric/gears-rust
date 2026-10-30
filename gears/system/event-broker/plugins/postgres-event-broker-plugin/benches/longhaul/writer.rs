//! A fenced writer on a fixed schedule: one append in flight, resent under the
//! same fence until answered.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Fence, FenceKey, FenceSeq, Flush, KeyedEvent,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::args::Args;
use crate::tally::Tally;
use crate::topics::{Rng, Stamp, Topic, event};

const RESEND_BACKOFF: Duration = Duration::from_millis(200);

pub struct Writer {
    pub id: u32,
    pub topic: Topic,
    /// Events per second at the base rate.
    pub rate: f64,
    pub keys: Arc<[Vec<u32>]>,
    pub args: Arc<Args>,
    pub backend: Arc<dyn Backend>,
    pub tally: Arc<Tally>,
}

impl Writer {
    /// Writes until `stop`; an append still unanswered then is counted as
    /// unresolved.
    pub async fn run(self, stop: CancellationToken) {
        let fence_key = FenceKey::new(Uuid::new_v4().as_u128());
        let pad = "x".repeat(self.args.payload_bytes);
        let mut rng = Rng::seeded();
        let mut counts: HashMap<u32, u64> = HashMap::new();
        let mut seq = 0_i64;
        let mut partition = self.id % self.topic.partitions;
        let mut due = Instant::now();
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep_until(due) => {}
            }
            let scheduled = due;
            due += self.period(due);

            let keys = loop {
                partition = (partition + 1) % self.topic.partitions;
                if !self.keys[partition as usize].is_empty() {
                    break &self.keys[partition as usize];
                }
            };
            let events: Vec<KeyedEvent> = (0..self.args.batch)
                .map(|_| {
                    let k = keys[usize::try_from(rng.below(keys.len() as u64)).unwrap_or(0)];
                    let n = counts.entry(k).or_insert(0);
                    *n += 1;
                    event(
                        Stamp {
                            w: self.id,
                            k,
                            n: *n,
                        },
                        &pad,
                    )
                })
                .collect();
            seq += 1;
            if !self.append(fence_key, seq, &events, scheduled, &stop).await {
                return;
            }
        }
    }

    /// Time to the next send: the batch at the current rate, bursts included.
    fn period(&self, now: Instant) -> Duration {
        let factor = self
            .args
            .burst_factor_at(now.saturating_duration_since(self.tally.started));
        Duration::from_secs_f64(self.args.batch as f64 / (self.rate * factor).max(f64::EPSILON))
    }

    /// Sends until answered; false if `stop` came first.
    async fn append(
        &self,
        key: FenceKey,
        seq: i64,
        events: &[KeyedEvent],
        scheduled: Instant,
        stop: &CancellationToken,
    ) -> bool {
        let request = AppendRequest {
            fence: Fence {
                key,
                seq: FenceSeq::new(seq),
            },
            topic: &self.topic.id,
            events,
            flush: Flush::Sync,
        };
        let count = events.len() as u64;
        let sent = Instant::now();
        let mut first = true;
        loop {
            match self.backend.append(&request).await {
                Ok(answer) => {
                    self.tally.up();
                    if first && answer == Appended::Duplicate {
                        self.tally.violation(format!(
                            "writer {} on {}: first send of fence seq {seq} answered Duplicate",
                            self.id,
                            self.topic.kind.name()
                        ));
                    }
                    self.tally.acked(
                        self.topic.kind,
                        count,
                        sent.saturating_duration_since(scheduled),
                        sent.elapsed(),
                    );
                    return true;
                }
                Err(err) => {
                    self.tally.append_errors.fetch_add(1, Ordering::Relaxed);
                    if matches!(err, BackendError::Unavailable { .. }) {
                        self.tally.down(&err.to_string());
                    } else {
                        self.tally.log(&format!(
                            "writer {} on {}: append failed: {err}",
                            self.id,
                            self.topic.kind.name()
                        ));
                    }
                }
            }
            first = false;
            tokio::select! {
                () = stop.cancelled() => {
                    self.tally.unresolved[self.topic.kind.index()].fetch_add(count, Ordering::Relaxed);
                    return false;
                }
                () = tokio::time::sleep(RESEND_BACKOFF) => {}
            }
        }
    }
}
