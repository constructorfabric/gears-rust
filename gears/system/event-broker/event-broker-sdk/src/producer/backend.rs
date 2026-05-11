use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::models::ResetScope;
use crate::producer::ProducerMode;

/// Result of a single event publish. Returned by [`ProducerBackend::ingest_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Broker accepted the event (202 Accepted). The async publish path returns
    /// this once the event is admitted but before persist confirmation.
    Accepted,
    /// Broker accepted *and* durably persisted the event. Returned by the
    /// persist-confirming sync publish path (`publish_sync`); never by the async
    /// path. (The 201-vs-202 HTTP status is decided at the HTTP layer.)
    Persisted,
    /// Broker recognised an idempotent duplicate (200 OK). The local chain
    /// tracker must NOT advance for this response.
    Duplicate,
}

/// Broker cursor for one `(topic, partition)` pair.
#[derive(Debug, Clone)]
pub struct ProducerCursor {
    pub topic: String,
    pub partition: u32,
    pub last_sequence: i64,
}

/// Transport contract for producer operations against the Event Broker ingest API.
///
/// The impl crate provides the concrete type; the SDK uses this trait to remain
/// transport-agnostic (supports both in-process and HTTP).
#[async_trait]
pub trait ProducerBackend: Send + Sync {
    /// Register a new producer and return its broker-issued id.
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError>;

    /// Publish one event to the ingest path.
    async fn ingest_event(
        &self,
        ctx: &SecurityContext,
        event: &crate::models::Event,
    ) -> Result<IngestOutcome, EventBrokerError>;

    /// Retrieve the broker's last_sequence per (topic, partition) for a producer.
    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<Vec<ProducerCursor>, EventBrokerError>;

    /// Reset a producer's chain state.
    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError>;
}
