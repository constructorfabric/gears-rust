//! The processing worker's metrics, and its load: the records waiting in the
//! queue and the records being processed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, ObservableGauge};

use crate::domain::record_intake::{DropEvent, DropStage, IntakeEvents};
use crate::infra::intake_events::stage;
use crate::infra::record_queue::QueuedHandOff;

/// What the worker has on hand. The queue is held weakly, so the load never
/// keeps the queue open after intake let go of it.
#[derive(Clone)]
pub struct WorkerLoad {
    in_flight: Arc<AtomicUsize>,
    queue: Weak<QueuedHandOff>,
}

impl WorkerLoad {
    #[must_use]
    pub fn new(queue: Weak<QueuedHandOff>) -> Self {
        Self {
            in_flight: Arc::new(AtomicUsize::new(0)),
            queue,
        }
    }

    /// Records waiting in the queue.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.upgrade().map_or(0, |queue| queue.queued())
    }

    /// Records being processed.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Relaxed)
    }

    /// Count one record as being processed until the guard drops, also when
    /// its processing is stopped or aborted.
    #[must_use]
    pub fn start(&self) -> InFlight {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlight(Arc::clone(&self.in_flight))
    }
}

/// One record being processed; see [`WorkerLoad::start`].
pub struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// `construct_records_processed_total`, `construct_record_processing_seconds`,
/// `construct_records_dropped_total` and the queue gauges, on the gear's meter.
pub struct ProcessingMetrics {
    processed: Counter<u64>,
    duration: Histogram<f64>,
    dropped: Counter<u64>,
    _queued: ObservableGauge<u64>,
    _in_flight: ObservableGauge<u64>,
}

impl ProcessingMetrics {
    /// The metrics of the worker with `load`.
    #[must_use]
    pub fn new(load: &WorkerLoad) -> Self {
        let meter = opentelemetry::global::meter("construct");
        let queued = load.clone();
        let in_flight = load.clone();
        Self {
            processed: meter
                .u64_counter("construct_records_processed_total")
                .with_description("Records the worker finished, stored or dropped")
                .build(),
            duration: meter
                .f64_histogram("construct_record_processing_seconds")
                .with_description("Time to process one record")
                .with_unit("s")
                .build(),
            dropped: meter
                .u64_counter("construct_records_dropped_total")
                .with_description("Records dropped, by stage and reason")
                .build(),
            _queued: meter
                .u64_observable_gauge("construct_processing_queue_depth")
                .with_description("Records waiting in the processing queue")
                .with_callback(move |observer| observer.observe(as_u64(queued.queued()), &[]))
                .build(),
            _in_flight: meter
                .u64_observable_gauge("construct_processing_in_flight")
                .with_description("Records being processed")
                .with_callback(move |observer| {
                    observer.observe(as_u64(in_flight.in_flight()), &[]);
                })
                .build(),
        }
    }

    /// One record finished after `took`.
    pub fn processed(&self, took: Duration) {
        self.processed.add(1, &[]);
        self.duration.record(took.as_secs_f64(), &[]);
    }

    fn dropped(&self, drop_stage: DropStage, reason: &'static str) {
        self.dropped.add(
            1,
            &[
                KeyValue::new("stage", stage(drop_stage)),
                KeyValue::new("reason", reason),
            ],
        );
    }
}

fn as_u64(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

/// [`IntakeEvents`] that counts each drop, then hands it on.
pub struct CountedDrops {
    inner: Arc<dyn IntakeEvents>,
    metrics: Arc<ProcessingMetrics>,
}

impl CountedDrops {
    #[must_use]
    pub fn new(inner: Arc<dyn IntakeEvents>, metrics: Arc<ProcessingMetrics>) -> Self {
        Self { inner, metrics }
    }
}

impl IntakeEvents for CountedDrops {
    fn dropped(&self, event: DropEvent) {
        self.metrics.dropped(event.stage, event.reason.name());
        self.inner.dropped(event);
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use uuid::Uuid;

    use super::*;
    use crate::domain::record_intake::{DropReason, RecordHandOff};
    use crate::infra::record_queue::record_queue;
    use crate::test_support::{RecordingEvents, received_chat_record};

    #[tokio::test]
    async fn a_record_counts_as_in_flight_until_it_ends_also_when_it_is_aborted() {
        let load = WorkerLoad::new(Weak::new());

        let finished = load.start();
        assert_eq!(load.in_flight(), 1);
        drop(finished);
        assert_eq!(load.in_flight(), 0);

        let held = load.start();
        let task = tokio::spawn(async move {
            let _held = held;
            std::future::pending::<()>().await;
        });
        assert_eq!(load.in_flight(), 1);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            load.in_flight(),
            0,
            "an aborted record is no longer counted"
        );
    }

    #[test]
    fn the_queue_depth_follows_the_queue_and_reads_zero_once_it_is_gone() {
        let (hand_off, _queue) = record_queue(NonZeroUsize::new(4).unwrap());
        let hand_off = Arc::new(hand_off);
        let load = WorkerLoad::new(Arc::downgrade(&hand_off));

        hand_off
            .hand_off(received_chat_record(Uuid::new_v4(), Uuid::new_v4()))
            .unwrap();
        assert_eq!(load.queued(), 1);
        drop(hand_off);
        assert_eq!(load.queued(), 0);
    }

    #[test]
    fn a_counted_drop_is_handed_on() {
        let inner = Arc::new(RecordingEvents::default());
        let metrics = Arc::new(ProcessingMetrics::new(&WorkerLoad::new(Weak::new())));
        let events = CountedDrops::new(Arc::clone(&inner) as _, metrics);
        let record = received_chat_record(Uuid::new_v4(), Uuid::new_v4());

        events.dropped(DropEvent::for_record(
            &record,
            DropStage::Checks,
            DropReason::ProcessingFailed,
        ));

        let dropped = inner.taken();
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].stage, DropStage::Checks);
    }
}
