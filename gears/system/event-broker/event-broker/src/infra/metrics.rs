//! OpenTelemetry metrics of the ingest outbox (feature 0007 §4.1).
//!
//! Nothing an ingest outbox holds is dead-lettered, so a message that keeps
//! failing stalls its outbox partition. These two make that visible: how often
//! a message is retried, and how long the message at the head of each outbox
//! partition has waited.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::BackendError;
use opentelemetry::metrics::{Counter, Meter, ObservableGauge};
use opentelemetry::{InstrumentationScope, KeyValue};

/// Instrumentation scope of this gear's metrics.
pub const SCOPE: &str = "cf-gears-event-broker";

/// The `backend` label of a retry no backend was resolved for.
pub const NO_BACKEND: &str = "none";

/// Why the ingest outbox handler asked for a message to be retried.
#[derive(Debug)]
pub enum RetryCause {
    /// The backend's `append` failed.
    Append(BackendError),
    /// The payload does not decode.
    UndecodablePayload,
    /// The payload names a topic the registry does not hold.
    UnregisteredTopic,
}

impl RetryCause {
    /// Exhaustive: a new backend error needs a label of its own.
    const fn label(&self) -> &'static str {
        match self {
            Self::Append(error) => match error {
                BackendError::Unavailable { .. } => "unavailable",
                BackendError::InvalidConfig { .. } => "invalid_config",
                BackendError::OffsetOutOfRange { .. } => "offset_out_of_range",
                BackendError::PartitionNotFound { .. } => "partition_not_found",
                BackendError::AppendFailed { .. } => "append_failed",
                BackendError::ReadFailed { .. } => "read_failed",
                BackendError::LogConflict { .. } => "log_conflict",
                BackendError::NotProvisioned { .. } => "not_provisioned",
                BackendError::Internal(_) => "internal",
            },
            Self::UndecodablePayload => "undecodable_payload",
            Self::UnregisteredTopic => "unregistered_topic",
        }
    }
}

/// The message each outbox partition is handling, by the outbox's partition
/// id: its creation time while it is in hand, `None` once it is acknowledged.
type InHand = Arc<Mutex<HashMap<i64, Option<DateTime<Utc>>>>>;

pub struct IngestMetrics {
    outbox: &'static str,
    /// `evbk_ingest_append_retries_total{backend, outbox, partition, error}`.
    append_retries: Counter<u64>,
    in_hand: InHand,
    /// `evbk_ingest_outbox_oldest_age_seconds{outbox, partition}`, observed
    /// from `in_hand`; held so its callback stays registered.
    _oldest_age: ObservableGauge<f64>,
}

impl IngestMetrics {
    /// Declares the instruments on `meter`, for the outbox group `outbox`.
    /// `now` is the clock the age is measured with.
    #[must_use]
    pub fn new(meter: &Meter, outbox: &'static str, now: fn() -> DateTime<Utc>) -> Self {
        let in_hand: InHand = Arc::default();
        let observed = Arc::clone(&in_hand);
        let oldest_age = meter
            .f64_observable_gauge("evbk_ingest_outbox_oldest_age_seconds")
            .with_description(
                "Age of the message an ingest outbox partition is handling; 0 once it is \
                 acknowledged. Grows while the message is retried",
            )
            .with_unit("s")
            .with_callback(move |observer| {
                let now = now();
                let in_hand = observed.lock().unwrap_or_else(PoisonError::into_inner);
                for (partition, created_at) in in_hand.iter() {
                    let age = created_at.map_or(0.0, |created_at| {
                        (now - created_at).as_seconds_f64().max(0.0)
                    });
                    observer.observe(
                        age,
                        &[
                            KeyValue::new("outbox", outbox),
                            KeyValue::new("partition", *partition),
                        ],
                    );
                }
            })
            .build();
        Self {
            outbox,
            append_retries: meter
                .u64_counter("evbk_ingest_append_retries_total")
                .with_description(
                    "Ingest outbox messages retried, by the backend they route to and why",
                )
                .build(),
            in_hand,
            _oldest_age: oldest_age,
        }
    }

    /// Declares the instruments on the global `MeterProvider`.
    #[must_use]
    pub fn global(outbox: &'static str) -> Arc<Self> {
        let scope = InstrumentationScope::builder(SCOPE).build();
        Arc::new(Self::new(
            &opentelemetry::global::meter_with_scope(scope),
            outbox,
            Utc::now,
        ))
    }

    /// `partition` is handling the message created at `created_at`: the
    /// oldest it holds, since an outbox partition drains in order.
    pub fn handling(&self, partition: i64, created_at: DateTime<Utc>) {
        self.lock().insert(partition, Some(created_at));
    }

    /// `partition` acknowledged the message it was handling.
    pub fn handled(&self, partition: i64) {
        self.lock().insert(partition, None);
    }

    /// `partition` asked for its message to be retried.
    pub fn retried(&self, partition: i64, backend: &str, cause: &RetryCause) {
        self.append_retries.add(
            1,
            &[
                KeyValue::new("backend", backend.to_owned()),
                KeyValue::new("outbox", self.outbox),
                KeyValue::new("partition", partition),
                KeyValue::new("error", cause.label()),
            ],
        );
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Option<DateTime<Utc>>>> {
        self.in_hand.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
