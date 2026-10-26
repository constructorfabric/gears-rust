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
