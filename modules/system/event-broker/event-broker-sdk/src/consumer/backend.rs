use async_trait::async_trait;
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::consumer::offset_manager::ResolvedPosition;
use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, SubscriptionId};
use crate::models::{
    ConsumerGroup, CreateConsumerGroupRequest, PartitionRange, Topic, TopicSegment,
};

/// One per-partition seed for the pre-stream SEEK call. The `value` carries
/// either an exact last-processed offset, or a server-resolved sentinel.
#[derive(Debug, Clone)]
pub struct SeekPosition {
    pub topic: String,
    pub partition: u32,
    pub value: ResolvedPosition,
}

/// An event received from the broker in a poll response.
#[derive(Debug, Clone)]
pub struct WireEvent {
    pub id: Uuid,
    pub type_id: String,
    pub topic: String,
    pub tenant_id: Uuid,
    pub subject: String,
    pub subject_type: String,
    pub partition: u32,
    pub sequence: i64,
    pub offset: i64,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub sequence_time: chrono::DateTime<chrono::Utc>,
    pub trace_parent: Option<String>,
    pub data: serde_json::Value,
}

/// Assignment returned from a subscription JOIN.
#[derive(Debug, Clone)]
pub struct SubscriptionAssignment {
    pub subscription_id: SubscriptionId,
    pub assigned: Vec<PartitionSlot>,
    pub topology_version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PartitionSlot {
    pub topic_ix: u16,
    pub partition: u32,
}

/// One frame on the consumption stream.
///
/// Maps 1:1 to a multipart part on `GET /v1/events:stream` (and to an SSE event
/// on `GET /v1/events:sse`).
#[derive(Debug, Clone)]
pub enum WireFrame {
    /// One delivered event.
    Event(WireEvent),
    /// Idle marker — broker is alive, no events at this moment.
    Heartbeat,
    /// Broker → consumer advisory (rate-limit hint, deprecation notice, etc.).
    Advisory { code: String, detail: String },
    /// Subscription assignment or topology-version update.
    Topology {
        topology_version: i64,
        assigned: Vec<PartitionSlot>,
    },
}

/// Boxed `Stream<Item = Result<WireFrame, EventBrokerError>>`.
/// Returned by `ConsumerBackend::stream`.
pub type FrameStream =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<WireFrame, EventBrokerError>> + Send>>;

/// Transport contract for consumer operations against the Event Broker delivery API.
#[async_trait]
pub trait ConsumerBackend: Send + Sync {
    // ---- Consumer-group ----

    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError>;

    // ---- Subscription ----

    async fn create_subscription(
        &self,
        ctx: &SecurityContext,
        group: &ConsumerGroupId,
        client_agent: &str,
        session_timeout: Option<&str>,
        interests: &[SubscriptionInterest],
    ) -> Result<SubscriptionAssignment, EventBrokerError>;

    async fn delete_subscription(
        &self,
        ctx: &SecurityContext,
        subscription_id: SubscriptionId,
    ) -> Result<(), EventBrokerError>;

    // ---- Streaming consumption ----

    /// Open a long-lived multipart stream for the given subscription. The returned
    /// stream emits `WireFrame`s as they arrive (events, heartbeats, advisories,
    /// topology updates) and terminates when the subscription ends, the connection
    /// closes, or an error occurs (e.g., `SubscriptionGone`).
    ///
    /// Backed by `GET /v1/events:stream?subscription_id=...` on the wire.
    async fn stream(
        &self,
        ctx: &SecurityContext,
        subscription_id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError>;

    // ---- Ack / seek ----

    async fn ack(
        &self,
        ctx: &SecurityContext,
        subscription_id: SubscriptionId,
        partition_offsets: &[(u32, i64)],
    ) -> Result<i64, EventBrokerError>; // returns topology_version

    /// Pre-stream SEEK: set the per-partition starting position for the
    /// subscription. Accepts exact last-processed offsets or `Earliest` /
    /// `Latest` sentinels (server-resolved at admission). Called by the
    /// dispatcher once after JOIN (before opening the stream), and again on
    /// `WireFrame::Topology` for newly-assigned partitions.
    ///
    /// Backed by `POST /v1/subscriptions/{id}/positions` on the wire.
    async fn seek(
        &self,
        ctx: &SecurityContext,
        subscription_id: SubscriptionId,
        positions: &[SeekPosition],
    ) -> Result<(), EventBrokerError>;

    // ---- Topic introspection ----

    async fn list_topics(&self, ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError>;

    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, EventBrokerError>;
}

/// One interest entry for a subscription JOIN.
#[derive(Debug, Clone)]
pub struct SubscriptionInterest {
    pub topic: String,
    pub event_type_pattern: String,
    pub filter_engine: Option<String>,
    pub filter_expression: Option<String>,
}
