//! Binds the Event Broker managed producer and reports its readiness (D-200, §3.4 step 1).
//!
//! Before Orders may enqueue an event, binding resolves the real `EventBrokerApi` provider and
//! the canonical platform-root tenant, prepares the topic and all eleven schemas, builds the
//! managed Chained producer (each outbox partition registers durably on its first delivery) and
//! starts the toolkit's leased outbox workers. Every
//! prerequisite is required: there is no holding queue, direct publish or degraded mode, and a
//! missing provider leaves Orders not ready rather than silently undelivered (unlike Pricing's
//! interim `PendingProducer`).
use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::{
    DbDeduplication, DbProducer, EventBrokerApi, EventBrokerError, ProducerIdentity, ProducerMode,
    ProducerOutboxHandle,
};
use tenant_resolver_sdk::{TenantResolverClient, TenantStatus};
use toolkit_db::outbox::{Outbox, OutboxProfile, Partitions, WorkerTuning};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::events::payload::{
    OrderAcceptanceRecorded, OrderAmended, OrderApproved, OrderCancelled, OrderCompleted,
    OrderExpired, OrderFulfillmentFailed, OrderHeld, OrderRejected, OrderResumed, OrderSubmitted,
};
use super::events::{
    EVENT_TYPE_WILDCARD, EventSink, OUTBOX_PARTITIONS, OrderEvent, PRODUCER_KEY, QUEUE, RootTenant,
    SOURCE, TOPIC,
};

/// The Orders producer principal's subject type presented to Event Broker.
pub const PRODUCER_SUBJECT_TYPE: &str = "bss-orders.producer";

/// The configured producer principal and broker deployment facts.
#[derive(Debug, Clone, Copy)]
pub struct ProducerSettings {
    /// The producer principal's subject ID.
    pub subject_id: Uuid,
    /// The producer principal's subject tenant ID.
    pub tenant_id: Uuid,
}

/// Why the producer is not bound. Each is a readiness failure, never a degraded mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindFailure {
    /// No `EventBrokerApi` provider is registered.
    BrokerUnavailable,
    /// No `TenantResolverClient` provider is registered.
    TenantResolverUnavailable,
    /// The platform-root tenant could not be resolved.
    RootTenantUnavailable,
    /// The resolved root tenant is nil, has a parent or is not active.
    RootTenantInvalid,
    /// The topic or an event schema could not be prepared.
    SchemaPreparation,
    /// The producer identity or managed registration was rejected.
    ProducerRegistration,
    /// The outbox queue or its workers failed to start.
    WorkerStart,
    /// A binding step exceeded its time limit.
    Timeout,
}
impl BindFailure {
    /// The readiness code reported for this failure.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::BrokerUnavailable => "event-broker-unavailable",
            Self::TenantResolverUnavailable => "tenant-resolver-unavailable",
            Self::RootTenantUnavailable => "root-tenant-unavailable",
            Self::RootTenantInvalid => "root-tenant-invalid",
            Self::SchemaPreparation => "event-schema-preparation",
            Self::ProducerRegistration => "producer-registration",
            Self::WorkerStart => "producer-worker-start",
            Self::Timeout => "producer-binding-timeout",
        }
    }
}

/// A failed binding attempt: the readiness class, a sanitized operator detail naming the step,
/// and the underlying broker, resolver or outbox error with its source chain.
#[derive(Debug, thiserror::Error)]
#[error("bss-orders event producer: {}: {detail}", .failure.code())]
pub struct BindError {
    /// The readiness class of the failure.
    pub failure: BindFailure,
    /// Sanitized operator detail.
    pub detail: String,
    /// The underlying failure, when a dependency returned one.
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}
impl BindError {
    fn new(failure: BindFailure, detail: impl std::fmt::Display) -> Self {
        Self {
            failure,
            detail: detail.to_string(),
            source: None,
        }
    }
    fn caused(
        failure: BindFailure,
        detail: impl std::fmt::Display,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            failure,
            detail: detail.to_string(),
            source: Some(source.into()),
        }
    }
}

/// Producer readiness as the gear reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProducerReadiness {
    /// Binding has not finished yet.
    Starting,
    /// Binding failed for this reason.
    Unavailable(BindFailure),
    /// The producer is bound and Orders may enqueue events.
    Ready,
    /// The gear stopped and the producer was released.
    Stopped,
}
impl ProducerReadiness {
    /// The readiness code: `starting`, `ready`, `stopped` or the failure code.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Unavailable(failure) => failure.code(),
            Self::Ready => "ready",
            Self::Stopped => "stopped",
        }
    }
}

/// The bound producer: the transaction sink and the library worker handle.
pub struct BoundProducer {
    sink: EventSink,
    handle: ProducerOutboxHandle,
}
impl BoundProducer {
    /// The sink transactions enqueue Orders events through.
    #[must_use]
    pub fn sink(&self) -> &EventSink {
        &self.sink
    }
    /// Graceful library-managed shutdown; committed queue work is retained for takeover.
    pub async fn stop(self) {
        self.handle.stop().await;
    }
}

/// The producer principal's security context. The broker binds the managed registration to
/// this subject and authorizes the explicit root envelope tenant against its grants.
///
/// # Errors
/// An invalid configured identity.
pub fn producer_context(settings: &ProducerSettings) -> Result<SecurityContext, BindError> {
    SecurityContext::builder()
        .subject_id(settings.subject_id)
        .subject_tenant_id(settings.tenant_id)
        .subject_type(PRODUCER_SUBJECT_TYPE)
        .build()
        .map_err(|e| {
            BindError::caused(
                BindFailure::ProducerRegistration,
                "producer security context",
                e,
            )
        })
}

async fn step<T, E: Into<Box<dyn std::error::Error + Send + Sync>>>(
    limit: Duration,
    failure: BindFailure,
    what: &'static str,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, BindError> {
    match tokio::time::timeout(limit, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(BindError::caused(failure, what, e)),
        Err(_) => Err(BindError::new(
            BindFailure::Timeout,
            format!("{} step exceeded {limit:?}", failure.code()),
        )),
    }
}

/// Resolve the canonical platform-root tenant from the platform tenant resolver (D-95).
async fn root_tenant(
    resolver: &dyn TenantResolverClient,
    ctx: &SecurityContext,
    limit: Duration,
) -> Result<RootTenant, BindError> {
    let root = step(
        limit,
        BindFailure::RootTenantUnavailable,
        "root tenant lookup",
        resolver.get_root_tenant(ctx),
    )
    .await?;
    if root.parent_id.is_some() || root.status != TenantStatus::Active {
        return Err(BindError::new(
            BindFailure::RootTenantInvalid,
            "resolved root tenant is not an active parentless tenant",
        ));
    }
    RootTenant::resolved(root.id.0).ok_or_else(|| {
        BindError::new(
            BindFailure::RootTenantInvalid,
            "resolved root tenant is nil",
        )
    })
}

fn classify(error: &EventBrokerError) -> BindFailure {
    match error {
        EventBrokerError::EventTypeUnknown { .. }
        | EventBrokerError::EventTypeNotDeclared { .. }
        | EventBrokerError::TypeNotInDeclaredTopic { .. }
        | EventBrokerError::TopicNotFound { .. }
        | EventBrokerError::SchemaNotPrepared { .. } => BindFailure::SchemaPreparation,
        _ => BindFailure::ProducerRegistration,
    }
}

async fn prepare<K: super::events::EventKind>(
    producer: &DbProducer,
    limit: Duration,
) -> Result<(), BindError> {
    match tokio::time::timeout(limit, producer.prepare::<OrderEvent<K>>()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(BindError::caused(
            BindFailure::SchemaPreparation,
            K::NAME,
            e,
        )),
        Err(_) => Err(BindError::new(
            BindFailure::Timeout,
            format!("preparing {}", K::NAME),
        )),
    }
}

/// Bind the managed Chained producer over the Orders queue (DESIGN §3.7 *Producer queue*).
///
/// `processor_tuning` overrides only the processor's retry cadence; production passes `None`
/// and runs the `high_throughput` profile.
///
/// # Errors
/// The first missing or failed prerequisite. Nothing is enqueued by a failed binding.
// NOT cancel-safe: `bind_until_ready` drops this future at any await when the lifecycle is
// cancelled, and the steps it already completed are not undone. What a drop leaves behind is
// what a failed attempt leaves, and the next bind (on this or any replica) reuses it:
// - during or after `prepare_all`: nothing durable from registration; the SDK registers a
//   producer per outbox partition on its first delivery, under the same `PRODUCER_KEY` the
//   next bind compares;
// - between the eleven `prepare` steps: nothing durable; each `prepare` only resolves one event
//   type from the broker into the dropped producer's in-memory schema cache;
// - inside `queue.start` (toolkit-db `OutboxBuilder::start`): the queue's partition rows, which
//   `register_queue` inserts only if absent, and possibly some spawned outbox workers. Those
//   live in the start's local `TaskSet`, whose drop cancels their token, so they exit without
//   processing; the start signal they wait on is never sent.
// No `BoundProducer` exists until `bind` returns, so nothing is enqueued and no `EventSink` is
// published by a dropped attempt.
pub async fn bind(
    broker: Option<Arc<dyn EventBrokerApi>>,
    tenants: Option<Arc<dyn TenantResolverClient>>,
    db: toolkit_db::Db,
    settings: &ProducerSettings,
    limit: Duration,
    processor_tuning: Option<WorkerTuning>,
) -> Result<BoundProducer, BindError> {
    let broker = broker.ok_or_else(|| {
        BindError::new(BindFailure::BrokerUnavailable, "no EventBrokerApi provider")
    })?;
    let tenants = tenants.ok_or_else(|| {
        BindError::new(
            BindFailure::TenantResolverUnavailable,
            "no TenantResolverClient provider",
        )
    })?;
    let ctx = producer_context(settings)?;
    let root = root_tenant(tenants.as_ref(), &ctx, limit).await?;
    let deduplication = DbDeduplication::managed(ProducerMode::Chained)
        .key(PRODUCER_KEY)
        .build()
        .map_err(|e| {
            BindError::caused(
                BindFailure::ProducerRegistration,
                "producer deduplication",
                e,
            )
        })?;
    let built = DbProducer::builder()
        .broker(broker)
        .db(db.clone())
        .security_context(ctx)
        // No version in the agent: a stored registration compares it verbatim.
        .identity(ProducerIdentity::new().source(SOURCE).client_agent(SOURCE))
        .deduplication(deduplication)
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE_WILDCARD])
        .prepare_all();
    let producer = match tokio::time::timeout(limit, built).await {
        Ok(Ok(producer)) => producer,
        Ok(Err(e)) => {
            return Err(BindError::caused(
                classify(&e),
                "producer preparation and registration",
                e,
            ));
        }
        Err(_) => {
            return Err(BindError::new(
                BindFailure::Timeout,
                "producer preparation and registration",
            ));
        }
    };
    // Every one of the eleven schemas must resolve before the first transaction can enqueue it.
    prepare::<OrderSubmitted>(&producer, limit).await?;
    prepare::<OrderApproved>(&producer, limit).await?;
    prepare::<OrderRejected>(&producer, limit).await?;
    prepare::<OrderAmended>(&producer, limit).await?;
    prepare::<OrderHeld>(&producer, limit).await?;
    prepare::<OrderResumed>(&producer, limit).await?;
    prepare::<OrderCancelled>(&producer, limit).await?;
    prepare::<OrderExpired>(&producer, limit).await?;
    prepare::<OrderCompleted>(&producer, limit).await?;
    prepare::<OrderFulfillmentFailed>(&producer, limit).await?;
    prepare::<OrderAcceptanceRecorded>(&producer, limit).await?;
    let queue = producer
        .outbox_queue(QUEUE, Partitions::of(OUTBOX_PARTITIONS))
        .map_err(|e| BindError::caused(BindFailure::WorkerStart, "outbox queue", e))?;
    let mut builder = Outbox::builder(db).profile(OutboxProfile::high_throughput());
    if let Some(tuning) = processor_tuning {
        builder = builder.processor_tuning(tuning);
    }
    let handle = Box::pin(step(
        limit,
        BindFailure::WorkerStart,
        "outbox worker start",
        queue.start(builder),
    ))
    .await?;
    let sink = EventSink::new(handle.outbox().clone(), root);
    Ok(BoundProducer { sink, handle })
}
