use std::num::NonZeroU32;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::error::{EventBrokerError, StorageBackendError};
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use crate::models::Event;
use crate::models::{
    ConsumerGroup, ConsumerGroupQuery, CreateConsumerGroupRequest, EventType, Page,
    PartitionLeader, PartitionRange, ResetScope, Subscription, Topic, TopicSegment,
};

// --- Supporting types ---------------------------------------------------------

/// Producer deduplication mode declared at broker registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerMode {
    /// No producer id and no broker-side idempotency metadata.
    Stateless,
    /// Idempotency by `producer_id + sequence`.
    Monotonic,
    /// Idempotency by `producer_id + previous + sequence`.
    #[default]
    Chained,
}

/// Result of a single event publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// The event was admitted before persistence confirmation.
    Accepted,
    /// The event was durably persisted.
    Persisted,
    /// The event matched an idempotent duplicate.
    Duplicate,
}

/// Broker cursor for one `(topic, partition)` pair.
#[derive(Debug, Clone)]
pub struct ProducerCursor {
    pub topic: String,
    pub partition: u32,
    pub last_sequence: i64,
}

/// Where the consumer wants the broker to begin emitting for an assigned
/// `(topic, partition)`. The integer in [`ResolvedPosition::Exact`] is the
/// last offset the consumer has already processed. The broker computes
/// "emit from offset + 1" server-side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedPosition {
    /// Last offset the consumer has processed; broker emits from offset + 1.
    Exact(i64),
    /// Broker-resolved: emit from the partition's retention floor onwards.
    Earliest,
    /// Broker-resolved: emit only events admitted after this SEEK.
    Latest,
    /// Broker-resolved: seek to the first offset whose `occurred_at` is at or
    /// after the given ISO-8601 timestamp.
    AtTimestamp(String),
}

/// One per-partition seed for the pre-stream SEEK call.
#[derive(Debug, Clone)]
pub struct SeekPosition {
    pub topic: String,
    pub partition: u32,
    pub value: ResolvedPosition,
}

/// Partition assignment returned from a JOIN.
/// The starting cursor is established separately via SEEK.
#[derive(Debug, Clone)]
pub struct AssignedPartition {
    pub topic: String,
    pub partition: u32,
}

/// Response returned from a JOIN.
#[derive(Debug, Clone)]
pub struct SubscriptionAssignment {
    pub subscription_id: SubscriptionId,
    pub topology_version: i64,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub assigned: Vec<AssignedPartition>,
}

/// Request body for a JOIN.
#[derive(Debug, Clone)]
pub struct JoinRequest {
    pub group: ConsumerGroupId,
    /// RFC 9110 User-Agent grammar; ASCII 1-256 bytes.
    pub client_agent: String,
    /// Per-member interests (topic-anchored typed-filter selections per ADR-0005).
    pub interests: Vec<SubscriptionInterest>,
    /// Session TTL, refreshed on each poll/seek. Default PT30S.
    pub session_timeout: Option<Duration>,
}

/// An event received from the broker stream.
#[derive(Debug, Clone)]
pub struct WireEvent {
    pub id: Uuid,
    pub type_id: String,
    pub topic: String,
    pub tenant_id: Uuid,
    pub subject: String,
    pub subject_type: String,
    pub partition_key: Option<String>,
    pub partition: u32,
    pub sequence: i64,
    pub offset: i64,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub sequence_time: chrono::DateTime<chrono::Utc>,
    pub trace_parent: Option<String>,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PartitionSlot {
    pub topic_ix: u16,
    pub partition: u32,
}

/// A per-partition cursor position carried by stream topology/control frames.
#[derive(Debug, Clone)]
pub struct PartitionPosition {
    pub slot: PartitionSlot,
    /// Session cursor - last processed offset.
    pub offset: i64,
    /// Highest offset the broker has scanned for this group/partition.
    pub last_examined: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCode {
    Progress,
    Terminal,
}

/// One frame on the consumption stream.
// `WireFrame::Event(WireEvent)` stays unboxed to preserve the public stream API shape.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum WireFrame {
    Event(WireEvent),
    Heartbeat {
        at: String,
    },
    Topology {
        topology_version: i64,
        assigned: Vec<PartitionPosition>,
    },
    Control {
        code: ControlCode,
        positions: Vec<PartitionPosition>,
        reason: Option<String>,
    },
}

/// Boxed stream returned by [`EventBroker::stream`].
pub type FrameStream =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<WireFrame, EventBrokerError>> + Send>>;

/// Whether to stop traversal at self-managed tenant boundaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrierMode {
    #[default]
    Respect,
    Ignore,
}

/// Tenant hierarchy traversal scope for a subscription interest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TenantTraversalDepth {
    #[default]
    CurrentTenant,
    Descendants(NonZeroU32),
    UnlimitedDescendants,
}

impl TenantTraversalDepth {
    pub fn direct_children() -> Self {
        Self::Descendants(NonZeroU32::new(1).expect("1 is non-zero"))
    }

    pub fn descendants(depth: NonZeroU32) -> Self {
        Self::Descendants(depth)
    }

    pub fn unlimited() -> Self {
        Self::UnlimitedDescendants
    }
}

/// Paired filter engine + expression for a subscription interest.
#[derive(Debug, Clone)]
pub struct Filter {
    pub(crate) engine: String,
    pub(crate) expression: String,
}

impl Filter {
    pub fn new(
        engine: impl Into<String>,
        expression: impl Into<String>,
    ) -> Result<Self, EventBrokerError> {
        let engine = engine.into();
        let expression = expression.into();
        if engine.trim().is_empty() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "subscription filter engine must not be empty".to_owned(),
                instance: String::new(),
            });
        }
        if expression.is_empty() || expression.len() > 4096 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "subscription filter expression must be 1..=4096 bytes (got {})",
                    expression.len()
                ),
                instance: String::new(),
            });
        }
        Ok(Self { engine, expression })
    }

    pub fn engine(&self) -> &str {
        &self.engine
    }

    pub fn expression(&self) -> &str {
        &self.expression
    }
}

/// One interest entry for a subscription JOIN.
#[derive(Debug, Clone)]
pub struct SubscriptionInterest {
    pub(crate) topic: String,
    pub(crate) tenant_id: Uuid,
    pub(crate) tenant_depth: TenantTraversalDepth,
    pub(crate) barrier_mode: BarrierMode,
    pub(crate) types: Vec<String>,
    pub(crate) filter: Option<Filter>,
}

#[derive(Debug, Default)]
pub struct SubscriptionInterestBuilder {
    topic: Option<String>,
    tenant_id: Option<Uuid>,
    tenant_depth: TenantTraversalDepth,
    barrier_mode: BarrierMode,
    types: Vec<String>,
    filter: Option<Filter>,
}

impl SubscriptionInterest {
    pub fn builder() -> SubscriptionInterestBuilder {
        SubscriptionInterestBuilder::default()
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }

    pub fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    pub fn tenant_depth(&self) -> TenantTraversalDepth {
        self.tenant_depth
    }

    pub fn barrier_mode(&self) -> BarrierMode {
        self.barrier_mode
    }

    pub fn types(&self) -> &[String] {
        &self.types
    }

    pub fn filter(&self) -> Option<&Filter> {
        self.filter.as_ref()
    }
}

impl SubscriptionInterestBuilder {
    pub fn topic(mut self, topic: impl Into<String>) -> Self {
        self.topic = Some(topic.into());
        self
    }

    pub fn tenant_id(mut self, tenant_id: Uuid) -> Self {
        self.tenant_id = Some(tenant_id);
        self
    }

    pub fn tenant_depth(mut self, tenant_depth: TenantTraversalDepth) -> Self {
        self.tenant_depth = tenant_depth;
        self
    }

    pub fn barrier_mode(mut self, barrier_mode: BarrierMode) -> Self {
        self.barrier_mode = barrier_mode;
        self
    }

    pub fn types<I, S>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.types = types.into_iter().map(Into::into).collect();
        self
    }

    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    pub fn build(self) -> Result<SubscriptionInterest, EventBrokerError> {
        let topic = self
            .topic
            .ok_or_else(|| EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest topic is required".to_owned(),
                instance: String::new(),
            })?;
        let tenant_id = self
            .tenant_id
            .ok_or_else(|| EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest tenant_id is required".to_owned(),
                instance: String::new(),
            })?;
        if topic.trim().is_empty() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest topic must not be empty".to_owned(),
                instance: String::new(),
            });
        }
        if self.types.is_empty() || self.types.len() > 32 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "subscription interest event types must be 1..=32 entries (got {})",
                    self.types.len()
                ),
                instance: String::new(),
            });
        }
        if self.types.iter().any(|ty| ty.trim().is_empty()) {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest event types must not contain empty entries"
                    .to_owned(),
                instance: String::new(),
            });
        }

        Ok(SubscriptionInterest {
            topic,
            tenant_id,
            tenant_depth: self.tenant_depth,
            barrier_mode: self.barrier_mode,
            types: self.types,
            filter: self.filter,
        })
    }
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

// --- EventBroker - client-facing interface ------------------------------------

/// The Event Broker client interface - one method per broker operation.
///
/// Resolved from `ClientHub`:
/// ```ignore
/// let broker = hub.get::<dyn EventBroker>()?;
/// ```
///
/// Implemented by: the in-process direct backend, the remote HTTP backend,
/// and the mock (`--features test-util`). This boundary is **transport-agnostic** - no
/// HTTP types leak through it, and method docs describe operations, not wire paths.
/// HTTP verbs/paths (and their renames) live solely in `openapi.yaml` and the HTTP
/// backend, the single source of truth.
#[async_trait]
pub trait EventBroker: Send + Sync {
    // -- Producer --------------------------------------------------------------
    /// Register a producer; returns the broker-issued producer id. (On the HTTP
    /// wire the response body field is `id`, not `producer_id`.)
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError>;

    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError>;

    /// Persist-confirming publish: awaits the backend durable write and returns
    /// `IngestOutcome::Persisted` on success (vs. `Accepted` for the async path).
    /// A `Duplicate` is still reported as `Duplicate`.
    ///
    /// Default impl falls back to [`publish`] for backends that don't model
    /// persist confirmation; the mock overrides it.
    async fn publish_sync(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.publish(ctx, event).await
    }

    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[Event],
    ) -> Result<Vec<IngestOutcome>, EventBrokerError>;

    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<Vec<ProducerCursor>, EventBrokerError>;

    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError>;

    // -- Consumer groups -------------------------------------------------------
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError>;

    async fn get_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError>;

    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
        query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError>;

    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError>;

    // -- Subscriptions ---------------------------------------------------------
    async fn join(
        &self,
        ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError>;

    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError>;

    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError>;

    async fn leave(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError>;

    async fn stream(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError>;

    async fn seek(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError>;

    // -- Topic / event-type introspection -------------------------------------
    async fn list_topics(&self, ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError>;

    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, EventBrokerError>;

    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError>;

    async fn get_event_type(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError>;
}

// --- EventBrokerBackend - storage plugin seam --------------------------------

/// Plugin trait for swappable storage backends.
///
/// Implemented by the built-in memory and postgres backends; third-party backends
/// register via GTS type extension without modifying broker core.
///
/// **Note:** This trait exposes the public [`Event`](crate::models::Event) envelope.
/// Backend authors need the full shape to persist and assign offsets correctly.
#[async_trait]
pub trait EventBrokerBackend: Send + Sync {
    async fn persist(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        events: &[Event],
    ) -> Result<(), StorageBackendError>;

    async fn read(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        start_offset: i64,
        max_count: usize,
    ) -> Result<Vec<Event>, StorageBackendError>;

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
