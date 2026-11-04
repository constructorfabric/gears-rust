//! Writers: one fence key each, never more than one append in flight, paced
//! closed or open loop.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, Fence, FenceKey, FenceSeq, Flush, Head, KeyedEvent,
};
use event_broker_sdk::{GtsInstanceId, Sequence};
use tokio::time::Instant;
use uuid::Uuid;

use crate::stats::{Histogram, nanos, period_nanos};
use crate::workload::{Clock, EventFactory, KeyPools};

/// The timer fires on a millisecond tick at best: sleeping closer to a send
/// than this would make the send late by up to a tick.
const TIMER_SLACK: Duration = Duration::from_millis(2);

/// Attempts at one fence before its events are counted lost.
const MAX_ATTEMPTS: u32 = 5;
const RESEND_BACKOFF: Duration = Duration::from_millis(50);

/// How writers schedule their appends.
#[derive(Debug, Clone, Copy)]
pub enum Pacing {
    /// Each writer appends again as soon as the previous append returned.
    Closed,
    /// Appends are scheduled at a fixed rate across all writers. A writer
    /// that falls behind sends at once and skips nothing, so latency measured
    /// from the schedule includes the time it spent behind.
    Open {
        /// Target rate across all writers.
        events_per_sec: f64,
    },
}

/// When writers stop: at the deadline or when the shared event budget runs
/// out, whichever comes first. Running out ends the measured window there.
#[derive(Debug, Clone)]
pub struct Until {
    /// No append is scheduled at or after this instant.
    pub deadline: Instant,
    /// Events the writers may still append between them.
    pub budget: Arc<AtomicU64>,
    /// Once this little is left, warmup is over even if its time is not.
    pub warm_below: u64,
}

impl Until {
    /// Warmup may use a fifth of `events`; the rest is for measurement.
    pub fn new(deadline: Instant, events: u64) -> Self {
        Self {
            deadline,
            budget: Arc::new(AtomicU64::new(events)),
            warm_below: events - events.div_ceil(5),
        }
    }
}

/// How a phase's writers append.
#[derive(Debug, Clone)]
pub struct WritePlan {
    /// Writers, each with its own fence key.
    pub writers: usize,
    /// Events each append carries.
    pub events_per_append: usize,
    /// Whether appends wait for their events to be readable.
    pub flush: Flush,
    /// Closed or open loop.
    pub pacing: Pacing,
    /// When the writers stop.
    pub until: Until,
}

/// What the writers of one phase did.
#[derive(Debug, Default)]
pub struct WriteTally {
    /// Appends made, whatever their answer.
    pub appends: u64,
    /// Events of appends answered `Heads` or `Accepted`, or `Duplicate` on a
    /// resend.
    pub acked: u64,
    /// Acked events whose append returned inside the measured window.
    pub acked_in_window: u64,
    /// `Duplicate` to a fence's first send: nothing was resent, so nothing
    /// should be a duplicate.
    pub unexpected_duplicates: u64,
    /// Resends after a failed attempt.
    pub resends: u64,
    /// Failed append attempts.
    pub errors: u64,
    /// Events of fences that failed every attempt.
    pub lost: u64,
    /// The highest head an append answer reported, per partition.
    pub reported_heads: BTreeMap<u32, Sequence>,
    /// Acked appends whose answer reported no head (`Accepted`, or
    /// `Duplicate` on a resend).
    pub unreported: u64,
    /// From each measured append's scheduled time to its answer, once per
    /// event.
    pub ack: Histogram,
}

impl WriteTally {
    fn absorb(&mut self, other: &Self) {
        self.appends += other.appends;
        self.acked += other.acked;
        self.acked_in_window += other.acked_in_window;
        self.unexpected_duplicates += other.unexpected_duplicates;
        self.resends += other.resends;
        self.errors += other.errors;
        self.lost += other.lost;
        for (partition, head) in &other.reported_heads {
            self.raise(*partition, *head);
        }
        self.unreported += other.unreported;
        self.ack.merge(&other.ack);
    }

    fn raise(&mut self, partition: u32, head: Sequence) {
        let reported = self.reported_heads.entry(partition).or_default();
        *reported = (*reported).max(head);
    }
}

/// Everything the writers of one phase share.
#[derive(Clone)]
pub struct WriteContext {
    /// The backend under load.
    pub backend: Arc<dyn Backend>,
    /// The phase's topic.
    pub topic: GtsInstanceId,
    /// The phase's time base.
    pub clock: Clock,
    /// Payload padding every event carries.
    pub padding: Arc<str>,
    /// Partition keys by partition.
    pub pools: Arc<KeyPools>,
}

/// Runs `plan`'s writers to the end and adds up what they did.
#[allow(unknown_lints, de1301_no_print_macros)] // writer failure reported on the bench's stderr
pub async fn write(context: &WriteContext, plan: &WritePlan) -> WriteTally {
    let period = match plan.pacing {
        Pacing::Closed => None,
        Pacing::Open { events_per_sec } => Some(period_nanos(
            plan.events_per_append,
            plan.writers,
            events_per_sec,
        )),
    };
    let tasks: Vec<_> = (0..plan.writers)
        .map(|index| {
            let writer = Writer {
                id: u32::try_from(index).unwrap_or(u32::MAX),
                key: FenceKey::new(Uuid::new_v4().as_u128()),
                context: context.clone(),
                plan: plan.clone(),
                period,
                events: EventFactory::new(Arc::clone(&context.padding), Arc::clone(&context.pools)),
            };
            tokio::spawn(writer.run())
        })
        .collect();
    let mut tally = WriteTally::default();
    for task in tasks {
        match task.await {
            Ok(one) => tally.absorb(&one),
            Err(err) => {
                eprintln!("a writer task failed: {err}");
                tally.errors += 1;
            }
        }
    }
    tally
}

struct Writer {
    id: u32,
    key: FenceKey,
    context: WriteContext,
    plan: WritePlan,
    /// Nanoseconds between this writer's scheduled appends; `None` closed loop.
    period: Option<u64>,
    events: EventFactory,
}

enum Answer {
    /// Stored, with the heads the answer reported - none for `Accepted` or a
    /// resend's `Duplicate`.
    Acked(Vec<Head>),
    UnexpectedDuplicate,
    Lost,
}

#[allow(unknown_lints, de1301_no_print_macros)] // append failures reported on the bench's stderr
impl Writer {
    async fn run(mut self) -> WriteTally {
        let mut tally = WriteTally::default();
        let clock = self.context.clock.clone();
        let (mut seq, mut n) = (0_i64, 0_u64);
        for k in 0_u64.. {
            let scheduled = match self.period {
                None => Instant::now(),
                Some(period) => clock.origin + Duration::from_nanos(period.saturating_mul(k)),
            };
            let Some(count) = self.claim(scheduled) else {
                break;
            };
            wait_until(scheduled).await;
            let stamp = clock.stamp(scheduled);
            // One partition per append, as ingest batches arrive: each
            // writer starts at its own partition and moves to the next one
            // every append, so even one writer covers them all.
            let pool = self.id as usize + usize::try_from(k).unwrap_or(0);
            let batch: Vec<KeyedEvent> = (0..count)
                .map(|_| {
                    n += 1;
                    self.events.build(self.id, n, stamp, pool)
                })
                .collect();
            seq += 1;
            let answer = self
                .append(
                    Fence {
                        key: self.key,
                        seq: FenceSeq::new(seq),
                    },
                    &batch,
                    &mut tally,
                )
                .await;
            let answered = Instant::now();
            // A backend whose append completes without suspending would
            // otherwise keep this task on its worker and starve consumers;
            // the cooperative budget yields every so many calls, not each one.
            tokio::task::consume_budget().await;
            tally.appends += 1;
            let events = batch.len() as u64;
            match answer {
                Answer::Acked(heads) => {
                    if heads.is_empty() {
                        tally.unreported += 1;
                    }
                    for head in heads {
                        tally.raise(head.at.partition.value(), head.sequence);
                    }
                    tally.acked += events;
                    if clock.measures(answered) {
                        tally.acked_in_window += events;
                    }
                    if clock.measures(scheduled) {
                        tally.ack.record_n(nanos(answered - scheduled), events);
                    }
                }
                Answer::UnexpectedDuplicate => tally.unexpected_duplicates += events,
                Answer::Lost => tally.lost += events,
            }
        }
        tally
    }

    /// How many events the append scheduled at `scheduled` carries, or `None`
    /// once the writer is done.
    fn claim(&self, scheduled: Instant) -> Option<usize> {
        if scheduled >= self.plan.until.deadline {
            return None;
        }
        let wanted = self.plan.events_per_append as u64;
        let claimed =
            self.plan
                .until
                .budget
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    (left > 0).then(|| left.saturating_sub(wanted))
                });
        if let Ok(before) = claimed {
            if before <= self.plan.until.warm_below {
                self.context.clock.start_by(Instant::now());
            }
            usize::try_from(before.min(wanted)).ok()
        } else {
            self.context.clock.end_by(Instant::now());
            None
        }
    }

    /// Sends `fence` until it is answered, resending the same fence after an
    /// error as ingest does.
    async fn append(&self, fence: Fence, events: &[KeyedEvent], tally: &mut WriteTally) -> Answer {
        let request = AppendRequest {
            fence,
            topic: &self.context.topic,
            events,
            flush: self.plan.flush,
        };
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                tally.resends += 1;
                tokio::time::sleep(RESEND_BACKOFF).await;
            }
            match self.context.backend.append(&request).await {
                Ok(Appended::Heads(heads)) => return Answer::Acked(heads),
                Ok(Appended::Accepted) => return Answer::Acked(Vec::new()),
                // A resend answered `Duplicate` was stored by an earlier attempt.
                Ok(Appended::Duplicate) if attempt > 0 => return Answer::Acked(Vec::new()),
                Ok(Appended::Duplicate) => return Answer::UnexpectedDuplicate,
                Err(err) => {
                    if tally.errors == 0 {
                        eprintln!("writer {}: append failed: {err}", self.id);
                    }
                    tally.errors += 1;
                }
            }
        }
        Answer::Lost
    }
}

/// Returns at `at`, not a timer tick after it: sleeps to within
/// [`TIMER_SLACK`], then yields until the instant passes. An instant already
/// past returns at once.
async fn wait_until(at: Instant) {
    if let Some(early) = at.checked_sub(TIMER_SLACK)
        && early > Instant::now()
    {
        tokio::time::sleep_until(early).await;
    }
    while Instant::now() < at {
        tokio::task::yield_now().await;
    }
}
