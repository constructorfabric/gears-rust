//! Consumers: each owns a fixed set of partitions and reads them from the
//! start, by polling or by following the backend's head watch, checking what
//! it reads as it goes.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::Sequence;
use event_broker_sdk::backend::support::poll_heads;
use event_broker_sdk::backend::{Backend, BackendError, Head, TopicPartition, Wait, Watch};
use event_broker_sdk::models::Event;
use futures_util::FutureExt;
use futures_util::stream::{BoxStream, StreamExt};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::matrix::ReadMode;
use crate::stats::{Histogram, nanos};
use crate::window::Windows;
use crate::workload::{Clock, PARTITIONS, Stamp, partition};

/// How long a polling consumer sleeps after a round that found nothing.
pub const POLL_BACKOFF: Duration = Duration::from_millis(1);
const RESUBSCRIBE_BACKOFF: Duration = Duration::from_millis(100);

/// Per-partition heads every consumer reads up to before it stops; unset
/// while writers are still running.
pub type Targets = watch::Receiver<Option<Arc<Vec<Sequence>>>>;

/// How a phase's consumers read.
#[derive(Debug, Clone)]
pub struct ReadPlan {
    /// Consumers, each owning a share of the partitions.
    pub consumers: usize,
    /// Most events one read call returns.
    pub max_events: usize,
    /// Whether consumers poll or follow the head watch.
    pub mode: ReadMode,
    /// Which latencies the consumers keep.
    pub samples: Samples,
}

/// Which latencies a consumer keeps.
#[derive(Debug, Clone)]
pub enum Samples {
    /// Publish-to-head-seen and publish-to-read per measured event.
    Publish,
    /// The same, per second of read time.
    PerSecond(Arc<Windows>),
    /// Only how long each read call took.
    ReadCalls,
}

/// What the consumers of one phase did and saw.
#[derive(Debug, Default)]
pub struct ReadTally {
    /// Events read in total.
    pub consumed: u64,
    /// Events read inside the measured window.
    pub consumed_in_window: u64,
    /// An event whose sequence is not above the previous one read in its
    /// partition (from 0), so a repeat or a step back; gaps are allowed.
    pub sequence_breaks: u64,
    /// An event of a writer and key whose count is not above the previous one
    /// read for that writer and key.
    pub order_breaks: u64,
    /// Events without a bench stamp.
    pub unstamped: u64,
    /// Failed read calls.
    pub read_errors: u64,
    /// Errors the head watch returned.
    pub watch_errors: u64,
    /// Scheduled send to the consumer seeing a head that covers the event.
    pub head: Histogram,
    /// Scheduled send to the consumer's read returning the event.
    pub read: Histogram,
    /// Duration of each read call that returned events.
    pub read_calls: Histogram,
    /// When the last consumer reached its targets; `None` if one never did.
    pub drained_at: Option<Instant>,
}

impl ReadTally {
    fn absorb(&mut self, other: &Self) {
        self.consumed += other.consumed;
        self.consumed_in_window += other.consumed_in_window;
        self.sequence_breaks += other.sequence_breaks;
        self.order_breaks += other.order_breaks;
        self.unstamped += other.unstamped;
        self.read_errors += other.read_errors;
        self.watch_errors += other.watch_errors;
        self.head.merge(&other.head);
        self.read.merge(&other.read);
        self.read_calls.merge(&other.read_calls);
        self.drained_at = match (self.drained_at, other.drained_at) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
    }
}

/// Starts `plan`'s consumers; consumer `i` owns the partitions `p` with
/// `p % consumers == i`.
pub fn spawn(
    backend: &Arc<dyn Backend>,
    topic: &event_broker_sdk::GtsInstanceId,
    clock: &Clock,
    plan: &ReadPlan,
    targets: &Targets,
    cancel: &CancellationToken,
) -> Vec<JoinHandle<ReadTally>> {
    (0..plan.consumers)
        .map(|index| {
            let lanes = (0..PARTITIONS)
                .filter(|p| *p as usize % plan.consumers == index)
                .map(|p| Lane::new(partition(topic, p)))
                .collect();
            let consumer = Consumer {
                backend: Arc::clone(backend),
                clock: clock.clone(),
                plan: plan.clone(),
                lanes,
                last_n: HashMap::new(),
                pending: Pending::default(),
                tally: ReadTally::default(),
                targets: targets.clone(),
            };
            tokio::spawn(consumer.run(cancel.clone()))
        })
        .collect()
}

/// Waits for `consumers` until `deadline`, then cancels the rest and adds up
/// what all of them saw.
#[allow(unknown_lints, de1301_no_print_macros)] // consumer failure reported on the bench's stderr
pub async fn join(
    consumers: Vec<JoinHandle<ReadTally>>,
    deadline: Instant,
    cancel: &CancellationToken,
) -> ReadTally {
    let canceller = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            cancel.cancel();
        })
    };
    let mut tally = ReadTally {
        drained_at: Some(Instant::now()),
        ..ReadTally::default()
    };
    for consumer in consumers {
        match consumer.await {
            Ok(one) => tally.absorb(&one),
            Err(err) => {
                eprintln!("a consumer task failed: {err}");
                tally.read_errors += 1;
                tally.drained_at = None;
            }
        }
    }
    canceller.abort();
    tally
}

/// One owned partition.
struct Lane {
    at: TopicPartition,
    /// The last sequence read.
    offset: Sequence,
    /// The highest head seen on the watch.
    known: Sequence,
    /// Heads seen and when, oldest first; consumed as reads pass them.
    seen: VecDeque<(Sequence, Instant)>,
}

impl Lane {
    fn new(at: TopicPartition) -> Self {
        Self {
            at,
            offset: Sequence::NONE,
            known: Sequence::NONE,
            seen: VecDeque::new(),
        }
    }

    /// When the first head at or above `sequence` was seen.
    fn head_seen(&mut self, sequence: Sequence) -> Option<Instant> {
        while self.seen.front().is_some_and(|(head, _)| *head < sequence) {
            self.seen.pop_front();
        }
        self.seen.front().map(|(_, at)| *at)
    }
}

/// The second of read time this consumer is filling, for
/// [`Samples::PerSecond`]; handed to the shared windows when the next begins.
#[derive(Default)]
struct Pending {
    second: u64,
    read: Histogram,
    head: Histogram,
}

struct Consumer {
    backend: Arc<dyn Backend>,
    clock: Clock,
    plan: ReadPlan,
    lanes: Vec<Lane>,
    /// The last count read per (writer, key).
    last_n: HashMap<(u32, u32), u64>,
    pending: Pending,
    tally: ReadTally,
    targets: Targets,
}

#[allow(unknown_lints, de1301_no_print_macros)] // watch and read failures reported on the bench's stderr
impl Consumer {
    async fn run(mut self, cancel: CancellationToken) -> ReadTally {
        match self.plan.mode {
            ReadMode::Sync => self.poll(&cancel).await,
            ReadMode::Async => self.follow(&cancel).await,
        }
        self.flush_second();
        self.tally.drained_at = self.drained().then(Instant::now);
        self.tally
    }

    fn drained(&mut self) -> bool {
        let targets = self.targets.borrow_and_update();
        let Some(targets) = targets.as_ref() else {
            return false;
        };
        self.lanes.iter().all(|lane| {
            targets
                .get(lane.at.partition.value() as usize)
                .is_none_or(|target| lane.offset >= *target)
        })
    }

    async fn poll(&mut self, cancel: &CancellationToken) {
        while !self.drained() && !cancel.is_cancelled() {
            let mut found = false;
            for lane in 0..self.lanes.len() {
                found |= self.read(lane).await > 0;
            }
            if !found {
                tokio::select! {
                    () = cancel.cancelled() => {}
                    () = tokio::time::sleep(POLL_BACKOFF) => {}
                }
            }
        }
    }

    async fn follow(&mut self, cancel: &CancellationToken) {
        let mut heads = self.subscribe();
        loop {
            let behind = self.read_to_heads().await;
            if self.drained() || cancel.is_cancelled() {
                return;
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                item = heads.next() => {
                    if !self.take(item) {
                        tokio::time::sleep(RESUBSCRIBE_BACKOFF).await;
                        heads = self.subscribe();
                    }
                }
                _ = self.targets.changed() => {}
                // A head promises its events are readable; if a read still
                // came back short, try again shortly rather than wait for
                // a head that may not move.
                () = tokio::time::sleep(POLL_BACKOFF), if behind => {}
            }
            while let Some(item) = heads.next().now_or_never() {
                if !self.take(item) {
                    heads = self.subscribe();
                    break;
                }
            }
        }
    }

    fn subscribe(&self) -> BoxStream<'static, Result<Head, BackendError>> {
        let partitions: Vec<TopicPartition> = self.lanes.iter().map(|l| l.at.clone()).collect();
        match self.backend.watch() {
            Watch::Native(watch) => watch.watch(&partitions),
            Watch::Poll(interval) => poll_heads(Arc::clone(&self.backend), &partitions, interval),
        }
    }

    /// Records a head from the watch; `false` when the watch has to be
    /// resubscribed.
    fn take(&mut self, item: Option<Result<Head, BackendError>>) -> bool {
        match item {
            Some(Ok(head)) => {
                let seen = Instant::now();
                if let Some(lane) = self.lanes.iter_mut().find(|lane| lane.at == head.at)
                    && head.sequence > lane.known
                {
                    lane.known = head.sequence;
                    lane.seen.push_back((head.sequence, seen));
                }
                true
            }
            Some(Err(err)) => {
                if self.tally.watch_errors == 0 {
                    eprintln!("watch failed: {err}");
                }
                self.tally.watch_errors += 1;
                false
            }
            None => false,
        }
    }

    /// Reads every lane up to its known head; `true` when a lane is still
    /// below it.
    async fn read_to_heads(&mut self) -> bool {
        let mut behind = false;
        for lane in 0..self.lanes.len() {
            while self.lanes[lane].offset < self.lanes[lane].known {
                if self.read(lane).await == 0 {
                    behind = true;
                    break;
                }
            }
        }
        behind
    }

    /// One `read(Wait::No)` of `lane`; how many events it returned.
    async fn read(&mut self, lane: usize) -> usize {
        let started = Instant::now();
        let result = self
            .backend
            .read(
                &self.lanes[lane].at,
                self.lanes[lane].offset,
                self.plan.max_events,
                Wait::No,
            )
            .await;
        let read_at = Instant::now();
        // As for writers: a read that never suspends must not hog a worker.
        tokio::task::consume_budget().await;
        match result {
            Ok(events) => {
                if !events.is_empty() && matches!(self.plan.samples, Samples::ReadCalls) {
                    self.tally.read_calls.record(nanos(read_at - started));
                }
                self.roll_second(read_at);
                for event in &events {
                    self.check(lane, event, read_at);
                }
                events.len()
            }
            Err(err) => {
                if self.tally.read_errors == 0 {
                    eprintln!("read failed: {err}");
                }
                self.tally.read_errors += 1;
                0
            }
        }
    }

    /// Hands the filled second to the shared windows once reads move past it.
    fn roll_second(&mut self, read_at: Instant) {
        let Samples::PerSecond(windows) = &self.plan.samples else {
            return;
        };
        let Some(second) = windows.second(read_at) else {
            return;
        };
        if second != self.pending.second {
            let filled = std::mem::replace(
                &mut self.pending,
                Pending {
                    second,
                    ..Pending::default()
                },
            );
            windows.add(filled.second, &filled.read, &filled.head);
        }
    }

    fn flush_second(&mut self) {
        if let Samples::PerSecond(windows) = &self.plan.samples {
            windows.add(self.pending.second, &self.pending.read, &self.pending.head);
        }
    }

    fn check(&mut self, lane: usize, event: &Event, read_at: Instant) {
        let lane = &mut self.lanes[lane];
        let sequence = event.sequence.unwrap_or(Sequence::NONE);
        if sequence <= lane.offset {
            self.tally.sequence_breaks += 1;
        }
        lane.offset = lane.offset.max(sequence);
        self.tally.consumed += 1;
        if self.clock.measures(read_at) {
            self.tally.consumed_in_window += 1;
        }
        let Some(stamp) = Stamp::read(event) else {
            self.tally.unstamped += 1;
            return;
        };
        let last = self.last_n.entry((stamp.writer, stamp.key)).or_insert(0);
        if stamp.n <= *last {
            self.tally.order_breaks += 1;
        }
        *last = (*last).max(stamp.n);
        let scheduled = self.clock.instant(stamp.scheduled);
        let (read, head) = match self.plan.samples {
            Samples::Publish => (&mut self.tally.read, &mut self.tally.head),
            Samples::PerSecond(_) => (&mut self.pending.read, &mut self.pending.head),
            Samples::ReadCalls => return,
        };
        if !self.clock.measures(scheduled) {
            return;
        }
        read.record(nanos(read_at - scheduled));
        if self.plan.mode == ReadMode::Async {
            // A read can run past the last head seen; such an event was seen
            // no later than the read that returned it.
            let seen = lane.head_seen(sequence).unwrap_or(read_at);
            head.record(nanos(seen.saturating_duration_since(scheduled)));
        }
    }
}
