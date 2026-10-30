use std::num::NonZeroU32;
use std::time::Duration;

use async_trait::async_trait;
use gts::{GtsIdPattern, GtsInstanceId, GtsTypeId};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use crate::models::Event;
use crate::models::{
    ConsumerGroup, ConsumerGroupQuery, CreateConsumerGroupRequest, EventType, Page, PartitionRange,
    ResetScope, Subscription, Topic, TopicSegment,
};
use crate::sequence::Sequence;

// --- Supporting types ---------------------------------------------------------

/// Producer deduplication mode declared at broker registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

/// Broker `last_sequence` of a producer's chain on one topic. The chain is
/// kept per `(producer_id, topic)`: no partition is part of it (ADR-0004
/// "Chain Scope").
#[derive(Debug, Clone)]
pub struct TopicCursors {
    pub topic: GtsInstanceId,
    pub last_sequence: i64,
}

/// Producer cursor snapshot: the broker's known `last_sequence` per topic, plus
/// the echoed producer identity. Object-shaped (not a bare array) so
/// producer-level fields can be carried and the response stays extensible.
#[derive(Debug, Clone)]
pub struct ProducerCursors {
    pub producer_id: ProducerId,
    pub client_agent: String,
    pub topics: Vec<TopicCursors>,
}

impl ProducerCursors {
    /// True when the producer has no recorded cursor on any topic.
    pub fn is_empty(&self) -> bool {
        self.topics.is_empty()
    }

    /// The broker's known `last_sequence` for `topic`, if any.
    pub fn last_sequence(&self, topic: &GtsInstanceId) -> Option<i64> {
        self.topics
            .iter()
            .find(|t| &t.topic == topic)
            .map(|t| t.last_sequence)
    }
}

/// Where the consumer wants the broker to begin emitting for an assigned
/// `(topic, partition)`. The integer in [`Position::Exact`] is the
/// last offset the consumer has already processed. The broker computes
/// "emit from offset + 1" server-side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Position {
    /// Last offset the consumer has processed; broker emits from offset + 1.
    Exact(Sequence),
    /// Broker-resolved: emit from the partition's retention floor onwards.
    Earliest,
    /// Broker-resolved: emit only events admitted after this SEEK.
    Latest,
    /// Broker-resolved: seek so delivery begins at the first event the broker
    /// stored (`sequence_time`) at or after this instant - never by the
    /// producer's `occurred_at`, which is untrusted. Parsed at the API boundary,
    /// so no backend re-parses a string or has to invent behaviour for one it
    /// cannot read.
    At(chrono::DateTime<chrono::Utc>),
}

/// One per-partition seed for the pre-stream SEEK call.
#[derive(Debug, Clone)]
pub struct SeekPosition {
    pub topic: GtsInstanceId,
    pub partition: u32,
    pub value: Position,
}

/// Partition assignment returned from a JOIN.
/// The starting cursor is established separately via SEEK.
#[derive(Debug, Clone)]
pub struct AssignedPartition {
    pub topic: GtsInstanceId,
    pub partition: u32,
}

/// Response returned from a JOIN.
#[derive(Debug, Clone)]
pub struct SubscriptionAssignment {
    pub subscription_id: SubscriptionId,
    pub topology_version: i64,
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
///
/// It names no topic: a topic is what the event's `type_id` declares, so the
/// consumer resolves it rather than the broker repeating it on every frame.
#[derive(Debug, Clone)]
pub struct WireEvent {
    pub id: Uuid,
    pub type_id: GtsTypeId,
    pub tenant_id: Uuid,
    pub subject: String,
    pub subject_type: GtsTypeId,
    pub partition: u32,
    pub sequence: Sequence,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub sequence_time: chrono::DateTime<chrono::Utc>,
    pub trace_parent: Option<String>,
    pub data: serde_json::Value,
}

/// A per-partition cursor position carried by stream topology/control frames.
///
/// The topic is named rather than indexed, so a position identifies its partition
/// on its own - `partition` alone is ambiguous across the topics one subscription
/// can be assigned.
#[derive(Debug, Clone)]
pub struct PartitionPosition {
    pub topic: GtsInstanceId,
    pub partition: u32,
    /// Session cursor - last processed offset.
    pub offset: Sequence,
    /// Highest offset the broker has scanned for this group/partition.
    pub last_examined: Sequence,
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

/// Boxed stream returned by [`EventBrokerApi::stream`].
pub type FrameStream =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<WireFrame, EventBrokerError>> + Send>>;

/// Whether to stop traversal at self-managed tenant boundaries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// GTS id of the filter engine that evaluates `expression`.
    pub(crate) engine: GtsInstanceId,
    /// Expression in the engine's language; validated to 1..=4096 bytes.
    pub(crate) expression: String,
}

impl Filter {
    pub fn new(
        engine: GtsInstanceId,
        expression: impl Into<String>,
    ) -> Result<Self, EventBrokerError> {
        let expression = expression.into();
        if expression.is_empty() || expression.len() > 4096 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "subscription filter expression must be 1..=4096 bytes (got {})",
                    expression.len()
                ),
            });
        }
        Ok(Self { engine, expression })
    }

    pub fn engine(&self) -> &GtsInstanceId {
        &self.engine
    }

    pub fn expression(&self) -> &str {
        &self.expression
    }
}

/// One interest entry for a subscription JOIN.
#[derive(Debug, Clone)]
pub struct SubscriptionInterest {
    /// Topic the interest subscribes to.
    pub(crate) topic: GtsInstanceId,
    /// Root tenant whose events are of interest.
    pub(crate) tenant_id: Uuid,
    /// How far below `tenant_id` the tenant hierarchy is traversed.
    pub(crate) tenant_depth: TenantTraversalDepth,
    /// How tenant barriers are treated while traversing descendants.
    pub(crate) barrier_mode: BarrierMode,
    /// Event type patterns to match; empty means all types.
    pub(crate) types: Vec<GtsIdPattern>,
    /// Optional extra content filter applied to matching events.
    pub(crate) filter: Option<Filter>,
}

#[derive(Debug, Default)]
pub struct SubscriptionInterestBuilder {
    topic: Option<GtsInstanceId>,
    tenant_id: Option<Uuid>,
    tenant_depth: TenantTraversalDepth,
    barrier_mode: BarrierMode,
    types: Vec<GtsIdPattern>,
    filter: Option<Filter>,
}

impl SubscriptionInterest {
    pub fn builder() -> SubscriptionInterestBuilder {
        SubscriptionInterestBuilder::default()
    }

    pub fn topic(&self) -> &GtsInstanceId {
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

    pub fn types(&self) -> &[GtsIdPattern] {
        &self.types
    }

    pub fn filter(&self) -> Option<&Filter> {
        self.filter.as_ref()
    }
}

impl SubscriptionInterestBuilder {
    pub fn topic(mut self, topic: GtsInstanceId) -> Self {
        self.topic = Some(topic);
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

    pub fn types<I>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = GtsIdPattern>,
    {
        self.types = types.into_iter().collect();
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
            })?;
        let tenant_id = self
            .tenant_id
            .ok_or_else(|| EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest tenant_id is required".to_owned(),
            })?;
        if self.types.is_empty() || self.types.len() > 32 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "subscription interest event types must be 1..=32 entries (got {})",
                    self.types.len()
                ),
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
#[derive(Debug, Clone)]
pub struct StorageBackendConfig {
    pub gts_type_id: GtsTypeId,
    pub config: serde_json::Value,
}

/// Resolved position returned from a SEEK call, one entry per requested partition.
#[derive(Debug, Clone)]
pub struct SeekResult {
    pub topic: GtsInstanceId,
    pub partition: u32,
    pub offset: Sequence,
}

// --- EventBrokerApi - client-facing interface ------------------------------------

/// The Event Broker client interface - one method per broker operation.
///
/// Resolved from `ClientHub`:
/// ```ignore
/// let broker = hub.get::<dyn EventBrokerApi>()?;
/// ```
///
/// Implemented by: the in-process direct backend (the gear's `LocalBroker`) and
/// the remote HTTP backend. This boundary is **transport-agnostic** - no
/// HTTP types leak through it, and method docs describe operations, not wire paths.
/// HTTP verbs/paths (and their renames) live solely in `openapi.yaml` and the HTTP
/// backend, the single source of truth.
#[async_trait]
pub trait EventBrokerApi: Send + Sync {
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

    /// Publish a batch atomically. A batch is all-or-nothing (a single
    /// mode-shape or chain violation rejects the whole batch, per DESIGN), so
    /// the outcome is one value for the batch, not one per event.
    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError>;

    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError>;

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

    /// Seed cursor positions for a subscription's assigned partitions before a
    /// stream is opened.
    ///
    /// `topology_version` is the `topology_version` the caller last
    /// observed for the subscription (from the JOIN response or a subscription
    /// read). It fences the seek against a concurrent rebalance: if the group's
    /// topology has moved since, the whole seek is rejected with
    /// [`EventBrokerError::TopologyVersionMismatch`] and the caller re-reads the
    /// subscription and re-seeks. This is the only way a partition reassigned by
    /// a rebalance in the pre-stream window is told apart from a partition the
    /// caller never owned.
    async fn seek(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
        topology_version: i64,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError>;

    // -- Topic / event-type introspection -------------------------------------
    async fn list_topics(&self, ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError>;

    /// The backend's segment manifest for one `(topic, partition)`. The wire
    /// response is a single manifest object (`docs/openapi.yaml`), not a page.
    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError>;

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
