use std::time::Duration;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use serde::{Deserialize, Serialize};

use crate::consumer::backend::{FrameStream, SeekPosition, SubscriptionInterest};
use crate::error::{EventBrokerError, StorageBackendError};
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use crate::internal::envelope::Event as WireEnvelope;
use crate::models::{
    ConsumerGroup, ConsumerGroupKind, CreateConsumerGroupRequest, EventType, Page, PartitionLeader,
    PartitionRange, Subscription, Topic, TopicSegment,
};
use crate::producer::backend::{IngestOutcome, ProducerCursor};

// ─── Supporting types ─────────────────────────────────────────────────────────

/// Partition assignment returned from a JOIN.
/// Starting cursor is established separately via SEEK (`POST /v1/subscriptions/{id}/positions`).
#[derive(Debug, Clone)]
pub struct AssignedPartition {
    pub topic: String,
    pub partition: u32,
}

/// Response returned from `POST /v1/subscriptions` (JOIN).
#[derive(Debug, Clone)]
pub struct SubscriptionAssignment {
    pub subscription_id: SubscriptionId,
    pub topology_version: i64,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub assigned: Vec<AssignedPartition>,
}

/// Request body for `POST /v1/subscriptions` (JOIN).
#[derive(Debug, Clone)]
pub struct JoinRequest {
    pub group: ConsumerGroupId,
    /// RFC 9110 User-Agent grammar; ASCII 1–256 bytes.
    pub client_agent: String,
    /// Per-member interests (topic-anchored typed-filter selections per ADR-0005).
    pub interests: Vec<SubscriptionInterest>,
    /// Session TTL, refreshed on every poll/ack. Default PT30S.
    pub session_timeout: Option<Duration>,
}

/// Opaque backend configuration envelope.
/// `gts_type_id` is a full GTS identifier registered with `types-registry-sdk`.
/// `config` is JSON validated against the GTS type's schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageBackendConfig {
    pub gts_type_id: String,
    pub config: serde_json::Value,
}

/// Resolved position returned from a SEEK call, one entry per requested partition.
#[derive(Debug, Clone)]
pub struct SeekResult {
    pub topic: String,
    pub partition: u32,
    pub offset: i64,
}

// ─── EventBroker — client-facing interface ────────────────────────────────────

/// The Event Broker client interface — one method per REST endpoint.
///
/// Resolved from `ClientHub`:
/// ```ignore
/// let broker = hub.get::<dyn EventBroker>()?;
/// ```
///
/// Implemented by: the in-process direct backend, the remote HTTP backend,
/// and the mock (`--features mock`). No HTTP types leak through this boundary.
#[async_trait]
pub trait EventBroker: Send + Sync {
    // ── Producer ──────────────────────────────────────────────────────────────
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode_str: &str,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError>; // → POST /v1/producers (response body field: "id" (not "producer_id"))

    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &WireEnvelope,
    ) -> Result<IngestOutcome, EventBrokerError>; // → POST /v1/events

    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[WireEnvelope],
    ) -> Result<Vec<IngestOutcome>, EventBrokerError>; // → POST /v1/events:batch

    async fn producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<Vec<ProducerCursor>, EventBrokerError>; // → GET /v1/producers/{id}/cursors

    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
        topic: Option<&str>,
        partition: Option<u32>,
    ) -> Result<(), EventBrokerError>; // → POST /v1/producers/{id}:reset

    // ── Consumer groups ───────────────────────────────────────────────────────
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError>; // → POST /v1/consumer_groups

    async fn get_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError>; // → GET /v1/consumer_groups/{id}

    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
        limit: Option<u32>,
        cursor: Option<String>,
        filter: Option<String>,
        orderby: Option<String>,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError>; // → GET /v1/consumer-groups

    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError>; // → DELETE /v1/consumer_groups/{id}

    // ── Subscriptions ─────────────────────────────────────────────────────────
    async fn join(
        &self,
        ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError>; // → POST /v1/subscriptions

    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError>; // → GET /v1/subscriptions/{id}

    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError>; // → GET /v1/subscriptions

    async fn leave(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError>; // → DELETE /v1/subscriptions/{id}

    async fn stream(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError>; // → GET /v1/events:stream?subscription_id=

    async fn seek(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError>; // → POST /v1/subscriptions/{id}:seek

    // ── Topic / event-type introspection ─────────────────────────────────────
    async fn list_topics(&self, ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError>; // → GET /v1/topics

    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, EventBrokerError>; // → GET /v1/topics/segments

    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError>; // → GET /v1/event_types

    async fn get_event_type(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError>; // → GET /v1/event_types?$filter=id eq '<id>'
}

// ─── EventBrokerBackend — storage plugin seam ────────────────────────────────

/// Plugin trait for swappable storage backends.
///
/// Implemented by the built-in memory and postgres backends; third-party backends
/// register via GTS type extension without modifying broker core.
///
/// **Note:** This trait exposes `internal::envelope::Event` — the only place the
/// wire envelope leaks into the public surface. Backend authors need the full
/// shape to persist and assign offsets correctly.
#[async_trait]
pub trait EventBrokerBackend: Send + Sync {
    async fn persist(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        events: &[WireEnvelope],
    ) -> Result<(), StorageBackendError>;

    async fn read(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        start_offset: i64,
        max_count: usize,
    ) -> Result<Vec<WireEnvelope>, StorageBackendError>;

    async fn query(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, StorageBackendError>;

    async fn list_partition_leaders(
        &self,
        ctx: &SecurityContext,
        topic: &str,
    ) -> Result<Vec<PartitionLeader>, StorageBackendError>;
}
