//! The processing worker's metrics, and its load: the records waiting in the
//! queue and the records being processed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter, ObservableGauge};

use crate::domain::record_intake::{DropEvent, DropStage, IntakeEvents};
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
/// `construct_records_dropped_total` and the queue gauges.
pub struct ProcessingMetrics {
    processed: Counter<u64>,
    duration: Histogram<f64>,
    dropped: Counter<u64>,
    _queued: ObservableGauge<u64>,
    _in_flight: ObservableGauge<u64>,
}

impl ProcessingMetrics {
    /// The metrics of the worker with `load`, on `meter`: the gear's own,
    /// `opentelemetry::global::meter("construct")`.
    #[must_use]
    pub fn new(load: &WorkerLoad, meter: &Meter) -> Self {
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
                KeyValue::new("stage", drop_stage.name()),
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
        let metrics = Arc::new(ProcessingMetrics::new(
            &WorkerLoad::new(Weak::new()),
            &opentelemetry::global::meter("construct"),
        ));
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

    mod exported {
        use opentelemetry::metrics::MeterProvider as _;
        use opentelemetry_sdk::metrics::data::{
            AggregatedMetrics, GaugeDataPoint, MetricData, ResourceMetrics, ScopeMetrics,
        };
        use opentelemetry_sdk::metrics::{
            InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        };

        use super::*;

        fn provider() -> (SdkMeterProvider, InMemoryMetricExporter) {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            (provider, exporter)
        }

        /// `read` applied to the last export of the metric `name`.
        fn last<R>(
            exporter: &InMemoryMetricExporter,
            name: &str,
            read: impl FnOnce(&AggregatedMetrics) -> R,
        ) -> R {
            let finished = exporter.get_finished_metrics().unwrap();
            let metric = finished
                .iter()
                .rev()
                .flat_map(ResourceMetrics::scope_metrics)
                .flat_map(ScopeMetrics::metrics)
                .find(|metric| metric.name() == name)
                .unwrap_or_else(|| panic!("{name} was not exported"));
            read(metric.data())
        }

        /// Each data point of the u64 sum `name`, with its attributes by key.
        fn sum(exporter: &InMemoryMetricExporter, name: &str) -> Vec<(Vec<KeyValue>, u64)> {
            last(exporter, name, |metric| {
                let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric else {
                    panic!("{name} is not a u64 sum");
                };
                sum.data_points()
                    .map(|point| {
                        let mut attributes: Vec<KeyValue> = point.attributes().cloned().collect();
                        attributes.sort_by(|a, b| a.key.as_str().cmp(b.key.as_str()));
                        (attributes, point.value())
                    })
                    .collect()
            })
        }

        /// The value of the u64 gauge `name`.
        fn gauge(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
            last(exporter, name, |metric| {
                let AggregatedMetrics::U64(MetricData::Gauge(gauge)) = metric else {
                    panic!("{name} is not a u64 gauge");
                };
                gauge.data_points().map(GaugeDataPoint::value).sum()
            })
        }

        /// The count and the sum of the f64 histogram `name`.
        fn histogram(exporter: &InMemoryMetricExporter, name: &str) -> (u64, f64) {
            last(exporter, name, |metric| {
                let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric else {
                    panic!("{name} is not an f64 histogram");
                };
                histogram
                    .data_points()
                    .fold((0, 0.0), |(count, sum), point| {
                        (count + point.count(), sum + point.sum())
                    })
            })
        }

        #[test]
        fn a_processed_record_counts_once_with_its_duration() {
            let (provider, exporter) = provider();
            let metrics =
                ProcessingMetrics::new(&WorkerLoad::new(Weak::new()), &provider.meter("construct"));

            metrics.processed(Duration::from_millis(250));
            metrics.processed(Duration::from_millis(750));
            provider.force_flush().unwrap();

            assert_eq!(
                sum(&exporter, "construct_records_processed_total"),
                [(Vec::new(), 2)]
            );
            let (count, seconds) = histogram(&exporter, "construct_record_processing_seconds");
            assert_eq!(count, 2);
            assert!((seconds - 1.0).abs() < 1e-9, "{seconds}");
        }

        #[test]
        fn a_drop_counts_by_its_stage_and_reason() {
            let (provider, exporter) = provider();
            let metrics = Arc::new(ProcessingMetrics::new(
                &WorkerLoad::new(Weak::new()),
                &provider.meter("construct"),
            ));
            let events = CountedDrops::new(Arc::new(RecordingEvents::default()), metrics);
            let record = received_chat_record(Uuid::new_v4(), Uuid::new_v4());

            events.dropped(DropEvent::for_record(
                &record,
                DropStage::Worker,
                DropReason::ProcessingFailed,
            ));
            provider.force_flush().unwrap();

            assert_eq!(
                sum(&exporter, "construct_records_dropped_total"),
                [(
                    vec![
                        KeyValue::new("reason", "processing_failed"),
                        KeyValue::new("stage", "worker"),
                    ],
                    1
                )]
            );
        }

        #[test]
        fn the_gauges_read_the_queue_and_the_records_in_flight() {
            let (provider, exporter) = provider();
            let (hand_off, _queue) = record_queue(NonZeroUsize::new(4).unwrap());
            let hand_off = Arc::new(hand_off);
            let load = WorkerLoad::new(Arc::downgrade(&hand_off));
            let _metrics = ProcessingMetrics::new(&load, &provider.meter("construct"));
            hand_off
                .hand_off(received_chat_record(Uuid::new_v4(), Uuid::new_v4()))
                .unwrap();
            let _in_flight = (load.start(), load.start());

            provider.force_flush().unwrap();

            assert_eq!(gauge(&exporter, "construct_processing_queue_depth"), 1);
            assert_eq!(gauge(&exporter, "construct_processing_in_flight"), 2);
        }
    }
}
