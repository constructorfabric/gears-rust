//! The three readers of each topic: tail (head watch), lagging (paced below
//! the write rate) and history (random positions).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use event_broker_sdk::backend::support::poll_heads;
use event_broker_sdk::backend::{Backend, BackendError, Head, Wait, Watch};
use event_broker_sdk::models::Event;
use event_broker_sdk::{Position, Sequence};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::checks::InOrder;
use crate::tally::Tally;
use crate::topics::{Rng, Topic};

/// Most events one read returns.
const READ_MAX: usize = 1000;
const RESUBSCRIBE_BACKOFF: Duration = Duration::from_secs(1);
const ERROR_BACKOFF: Duration = Duration::from_millis(500);

/// Reads `partition` after `offset`, counting a failure.
async fn read(
    backend: &dyn Backend,
    tally: &Tally,
    topic: &Topic,
    partition: u32,
    offset: i64,
    max: usize,
) -> Option<Vec<Event>> {
    match backend
        .read(
            &topic.at(partition),
            Sequence::assigned(offset),
            max,
            Wait::No,
        )
        .await
    {
        Ok(events) => {
            tally.up();
            Some(events)
        }
        Err(err) => {
            tally.read_errors.fetch_add(1, Ordering::Relaxed);
            if matches!(err, BackendError::Unavailable { .. }) {
                tally.down(&err.to_string());
            }
            None
        }
    }
}

/// Follows the head watch over every partition and reads up to each head.
pub async fn tail(
    backend: Arc<dyn Backend>,
    tally: Arc<Tally>,
    topic: Topic,
    stop: CancellationToken,
) {
    let mut order = InOrder::new("tail", topic.clone());
    loop {
        let mut heads: BoxStream<'static, Result<Head, BackendError>> = match backend.watch() {
            Watch::Native(watch) => watch.watch(&topic.all()),
            Watch::Poll(interval) => poll_heads(Arc::clone(&backend), &topic.all(), interval),
        };
        loop {
            let next = tokio::select! {
                () = stop.cancelled() => return,
                next = heads.next() => next,
            };
            let Some(Ok(head)) = next else {
                tally.watch_errors.fetch_add(1, Ordering::Relaxed);
                break;
            };
            catch_up(
                &*backend,
                &tally,
                &mut order,
                head.at.partition.value(),
                head.sequence.as_i64(),
                &stop,
            )
            .await;
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(RESUBSCRIBE_BACKOFF) => {}
        }
    }
}

/// Reads `partition` up to `head`.
async fn catch_up(
    backend: &dyn Backend,
    tally: &Arc<Tally>,
    order: &mut InOrder,
    partition: u32,
    head: i64,
    stop: &CancellationToken,
) {
    while order.positions[partition as usize] < head && !stop.is_cancelled() {
        let at = order.positions[partition as usize];
        let started = tokio::time::Instant::now();
        let Some(events) = read(backend, tally, &order.topic, partition, at, READ_MAX).await else {
            tokio::time::sleep(ERROR_BACKOFF).await;
            continue;
        };
        if events.is_empty() {
            return;
        }
        tally.tail_read(started.elapsed(), &events);
        tally.read.fetch_add(events.len() as u64, Ordering::Relaxed);
        let skipped = order.accept(backend, tally, partition, &events).await;
        tally.tail_skipped.fetch_add(skipped, Ordering::Relaxed);
        tally.tail_at(
            order.topic.kind,
            partition,
            order.positions[partition as usize],
        );
    }
}

/// Reads every partition round-robin at `rate` events per second, below
/// what writers write, so it falls behind and, on a bounded topic, under the
/// floor.
pub async fn lagging(
    backend: Arc<dyn Backend>,
    tally: Arc<Tally>,
    topic: Topic,
    rate: f64,
    stop: CancellationToken,
) {
    const TICK: Duration = Duration::from_millis(100);
    let mut order = InOrder::new("lagging", topic.clone());
    let mut budget = 0.0_f64;
    let mut partition = 0;
    let mut ticks = tokio::time::interval(TICK);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticks.tick() => {}
        }
        // At most one tick's worth carried over a pause, so it never catches
        // up in one rush after an outage.
        budget = (budget + rate * TICK.as_secs_f64()).min(rate * TICK.as_secs_f64() * 2.0);
        let mut empty = 0;
        while budget >= 1.0 && empty < topic.partitions {
            let at = order.positions[partition as usize];
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "budget is small and positive"
            )]
            let max = (budget as usize).min(READ_MAX);
            let Some(events) = read(&*backend, &tally, &topic, partition, at, max).await else {
                break;
            };
            if events.is_empty() {
                empty += 1;
            } else {
                empty = 0;
                budget -= events.len() as f64;
                let skipped = order.accept(&*backend, &tally, partition, &events).await;
                tally.skipped.fetch_add(skipped, Ordering::Relaxed);
            }
            partition = (partition + 1) % topic.partitions;
        }
    }
}

/// Once a second, reads `batch` events from a random position of a random
/// partition, and checks they are contiguous from it or start past the floor.
pub async fn history(
    backend: Arc<dyn Backend>,
    tally: Arc<Tally>,
    topic: Topic,
    batch: usize,
    stop: CancellationToken,
) {
    let mut rng = Rng::seeded();
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    let name = topic.kind.name();
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let partition = u32::try_from(rng.below(u64::from(topic.partitions))).unwrap_or(0);
        let at = topic.at(partition);
        let (Ok(floor), Ok(head)) = (
            backend.resolve(&at, Position::Earliest).await,
            backend.resolve(&at, Position::Latest).await,
        ) else {
            tally.read_errors.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let (floor, head) = (floor.as_i64(), head.as_i64());
        if head <= floor {
            continue;
        }
        let offset =
            floor + i64::try_from(rng.below(u64::try_from(head - floor).unwrap_or(1))).unwrap_or(0);
        match backend
            .read(&at, Sequence::assigned(offset), batch, Wait::No)
            .await
        {
            Ok(events) => {
                let sequences: Vec<i64> = events
                    .iter()
                    .filter_map(|e| e.sequence.map(Sequence::as_i64))
                    .collect();
                let contiguous = sequences.windows(2).all(|w| w[1] == w[0] + 1);
                if !contiguous
                    || sequences.first().is_some_and(|&first| first <= offset)
                    || sequences.len() != events.len()
                {
                    tally.violation(format!(
                        "history {name}/{partition}: read after {offset} returned {:?}..{:?} ({} events, contiguous {contiguous})",
                        sequences.first(),
                        sequences.last(),
                        events.len()
                    ));
                } else if sequences.first().is_some_and(|&first| first > offset + 1) {
                    // Started past the floor: the range was removed between
                    // choosing it and reading.
                    let now = backend
                        .resolve(&at, Position::Earliest)
                        .await
                        .map_or(i64::MAX, Sequence::as_i64);
                    if sequences[0] - 1 > now {
                        tally.violation(format!(
                            "history {name}/{partition}: read after {offset} began at {} above the floor {now}",
                            sequences[0]
                        ));
                    }
                }
            }
            Err(BackendError::OffsetOutOfRange { .. }) => {}
            Err(_) => {
                tally.read_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
