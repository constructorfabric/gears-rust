use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::error::EventBrokerError;
use crate::ids::ProducerId;

/// Result of a single event publish. Returned by [`ProducerBackend::ingest_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Broker accepted the event (202 Accepted or 201 Created).
    Accepted,
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
    /// `POST /v1/producers` — register a new producer and return its id.
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode_str: &str,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError>;

    /// `POST /v1/events` — publish one event to the ingest API.
    async fn ingest_event(
        &self,
        ctx: &SecurityContext,
        event: &crate::internal::envelope::Event,
    ) -> Result<IngestOutcome, EventBrokerError>;

    /// `GET /v1/producers/{id}/cursors` — retrieve last_sequence per (topic, partition).
    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<Vec<ProducerCursor>, EventBrokerError>;

    /// `POST /v1/producers/{id}:reset` — reset chain state.
    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
        topic: Option<&str>,
        partition: Option<u32>,
    ) -> Result<(), EventBrokerError>;
}
