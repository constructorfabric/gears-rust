//! @cpt-dod:cpt-cf-bss-products-dod-events-in-outbox-tx:p1
//! @cpt-dod:cpt-cf-bss-products-dod-outbox-same-tx:p1
//! Typed event enqueueing on the caller's transaction runner. An enqueue wakes the outbox's
//! sequencer only after the transaction commits (P-D-221): the writer enqueues through the
//! transaction's [`TxOutbox`], and [`transaction`] fires what the committed attempt enqueued.
use crate::infra::broker::EventSink;
use event_broker_sdk::TypedEvent;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
};
use toolkit_db::{
    Db, DbError, DbTx,
    outbox::Wake,
    secure::{DBRunner, TxConfig},
};

pub const OUTBOX_TABLE_PREFIX: &str = "bss_products_outbox";
pub const QUEUE_NAME: &str = "bss_products_events";
pub const PARTITIONS: u16 = 8;

/// The event sink as one transaction sees it (P-D-221, twin of pricing D-455). An event enqueued
/// through it leaves its outbox [`Wake`] here instead of waking the sequencer at once: a sequencer
/// woken before the commit finds nothing committed, and the row then waits for the cold
/// reconciler. Whoever opens the transaction settles the handle when the transaction ends:
/// [`fire`](Self::fire) once it has committed, [`discard`](Self::discard) when it rolls back.
/// [`transaction`] does both, and every door that enqueues runs in it; an approval subject holds
/// the attempt's handle, so the events its apply enqueues leave the engine with it. Clones share
/// one handle.
#[derive(Clone)]
pub struct TxOutbox {
    sink: EventSink,
    wake: Arc<Mutex<Wake>>,
}
impl TxOutbox {
    /// A handle over `sink` that holds no wake yet.
    #[must_use]
    pub fn new(sink: EventSink) -> Self {
        Self {
            sink,
            wake: Arc::new(Mutex::new(Wake::empty())),
        }
    }
    fn add(&self, wake: Wake) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) += wake;
    }
    fn take(&self) -> Wake {
        std::mem::replace(
            &mut *self.wake.lock().unwrap_or_else(PoisonError::into_inner),
            Wake::empty(),
        )
    }
    /// Wake the sequencers for every event enqueued through this handle. Call it only once the
    /// transaction has committed.
    pub fn fire(&self) {
        self.take().fire();
    }
    /// Drop every enqueued event's wake unfired: the transaction rolled back, and its rows with
    /// it, so there is nothing to wake a sequencer for.
    pub fn discard(&self) {
        self.take().discard();
    }
}

/// Run `work` in `db`'s retrying transaction with a [`TxOutbox`] over `sink`, and wake the
/// outbox's sequencers for what it enqueued only once it has committed (P-D-221). An attempt that
/// the retry repeats has rolled back, so the next attempt first discards its wakes; a transaction
/// that fails discards the rest. This is the toolkit's `outbox::in_transaction` contract on the
/// retrying transaction the gear's doors run in.
/// # Errors
/// The transaction's error after its retries, as [`Db::transaction_with_retry`] answers it.
pub async fn transaction<T, E, X, F>(
    db: &Db,
    sink: &EventSink,
    config: TxConfig,
    extract_db_err: X,
    mut work: F,
) -> Result<T, E>
where
    E: From<DbError> + Send + 'static,
    T: Send + 'static,
    X: Fn(&E) -> Option<&sea_orm::DbErr> + Send,
    F: for<'a> FnMut(
            &'a DbTx<'a>,
            TxOutbox,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
        + Send,
{
    let outbox = TxOutbox::new(sink.clone());
    let attempt = outbox.clone();
    let result = db
        .transaction_with_retry(config, extract_db_err, move |tx| {
            attempt.discard();
            work(tx, attempt.clone())
        })
        .await;
    if result.is_ok() {
        outbox.fire();
    } else {
        outbox.discard();
    }
    result
}

/// Hold messages until a broker is available.
pub struct PendingBrokerProducer;

#[async_trait::async_trait]
impl toolkit_db::outbox::LeasedMessageHandler for PendingBrokerProducer {
    async fn handle(
        &self,
        msg: &toolkit_db::outbox::OutboxMessage,
    ) -> toolkit_db::outbox::MessageResult {
        tracing::debug!(
            queue = QUEUE_NAME,
            payload_type = %msg.payload_type,
            "bss-products: no EventBrokerApi was present at boot, so the SDK producer \
             was not bound; holding the message in the queue"
        );
        toolkit_db::outbox::MessageResult::Retry
    }
}

/// Ambient W3C trace context, if present.
#[must_use]
pub fn traceparent() -> Option<String> {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = tracing::Span::current().context();
    let span = opentelemetry::trace::TraceContextExt::span(&context);
    let span_context = span.span_context();
    (span_context.trace_id() != opentelemetry::trace::TraceId::INVALID).then(|| {
        format!(
            "00-{}-{}-{:02x}",
            span_context.trace_id(),
            span_context.span_id(),
            span_context.trace_flags().to_u8()
        )
    })
}

/// Serialization or durable enqueue failure.
#[derive(Debug, thiserror::Error)]
pub enum EventsError {
    #[error("event serialization: {0}")]
    Serialize(String),
    #[error("broker producer: {0}")]
    Producer(#[source] event_broker_sdk::EventBrokerError),
    #[error("event database: {0}")]
    Db(#[source] sea_orm::DbErr),
    #[error("interim outbox: {0}")]
    Outbox(#[source] toolkit_db::outbox::OutboxError),
}

/// Write a typed event through the bound broker producer or the interim outbox. Its wake stays
/// with `outbox` until the transaction ends (P-D-221).
/// # Errors
/// Returns serialization or enqueue failures; the caller must roll back its transaction.
pub(crate) async fn enqueue_typed<E: TypedEvent>(
    outbox: &TxOutbox,
    runner: &(impl DBRunner + Sync),
    event: E,
) -> Result<(), EventsError> {
    // Neither arm fires its `Wake`: the transaction is still open, and a
    // sequencer woken now would find nothing committed. `outbox` keeps it for
    // the transaction's end.
    match &outbox.sink {
        // SDK ProducerOutbox::enqueue currently erases OutboxError into
        // EventBrokerError::Internal(String), exposing no DbErr/source to recover.
        EventSink::Broker(producer) => producer
            .enqueue(runner, event)
            .await
            .map(|wake| outbox.add(wake))
            .map_err(EventsError::from),
        EventSink::Interim(queue) => {
            // The SDK constructor is crate-private and DbProducer::outbox_envelope
            // needs a prepared broker/schema cache. Persist its v1 wire format here.
            // Stateless backlog needs no producer registration; the later processor
            // honors the mode in each envelope and publishes this durable event id.
            let envelope = serde_json::json!({
                "version": 1,
                "event_id": uuid::Uuid::now_v7(),
                "type": E::TYPE_ID,
                "topic": crate::infra::broker::TOPIC,
                "tenant_id": event.tenant_id(),
                "source": E::SOURCE,
                "subject": event.subject(),
                "subject_type": E::SUBJECT_TYPE,
                "occurred_at": time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).map_err(|e| EventsError::Serialize(e.to_string()))?,
                "trace_parent": event.trace_parent(),
                "data": serde_json::to_value(&event).map_err(|e| EventsError::Serialize(e.to_string()))?,
                "broker_partition": 0,
                "producer_mode": "stateless",
                "diagnostic_metadata": {"sdk_client_agent": crate::infra::broker::SOURCE}
            });
            let payload =
                serde_json::to_vec(&envelope).map_err(|e| EventsError::Serialize(e.to_string()))?;
            let partition = event.tenant_id().map_or(0, |t| {
                u32::from(u16::from_le_bytes([t.as_bytes()[14], t.as_bytes()[15]]) % PARTITIONS)
            });
            let record = toolkit_db::outbox::Record::to(QUEUE_NAME, partition)
                .payload(
                    payload,
                    "application/vnd.constructorfabric.event-broker.producer-outbox+json;version=1",
                )
                .build()?;
            queue
                .enqueue(runner, record)
                .await
                .map(|wake| outbox.add(wake))
                .map_err(EventsError::from)
        }
    }
}

impl From<toolkit_db::outbox::OutboxError> for EventsError {
    fn from(error: toolkit_db::outbox::OutboxError) -> Self {
        match error {
            toolkit_db::outbox::OutboxError::Database(source) => Self::Db(source),
            other => Self::Outbox(other),
        }
    }
}
// `ApprovalError::Store` is the engine's string, and that string is the internal detail.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<EventsError> for bss_approval::ApprovalError {
    fn from(error: EventsError) -> Self {
        match error {
            EventsError::Db(source) => Self::Db(source),
            other => Self::Store(other.to_string()),
        }
    }
}

impl From<event_broker_sdk::EventBrokerError> for EventsError {
    fn from(error: event_broker_sdk::EventBrokerError) -> Self {
        // Recover any typed source the SDK does expose. Its local enqueue
        // Internal(String) case has already lost the source and stays opaque.
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        while let Some(error) = cause {
            if let Some(db) = error.downcast_ref::<sea_orm::DbErr>() {
                return Self::Db(db.clone());
            }
            // SDK offset-manager errors store their source in an Arc. Walking
            // Arc::source directly would skip the wrapped error itself.
            cause = error
                .downcast_ref::<std::sync::Arc<dyn std::error::Error + Send + Sync>>()
                .map_or_else(|| error.source(), |shared| Some(shared.as_ref()));
        }
        Self::Producer(error)
    }
}
