use std::time::Duration;

use async_trait::async_trait;
use gts::{GtsIdPattern, GtsInstanceId, GtsTypeId};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::error::EventBrokerError;
use crate::gts::EventV1;
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
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
    /// Idempotency by `meta.producer` and `meta.sequence`.
    Monotonic,
    /// Idempotency by `meta.producer`, `meta.previous` and `meta.sequence`.
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

/// The guarantee a publish answers on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// Answer once the broker admits the event: `Accepted`, or `Duplicate` for an event it
    /// already admitted.
    Accepted,
    /// Hold the answer until the backend stores the event, for up to `within`: `Persisted`
    /// once stored, `Accepted` when the wait ends first (the event stays admitted), and
    /// `Duplicate` at once for an event the broker already admitted, which does not say
    /// that the earlier copy is stored. The broker caps the wait by its configuration, and
    /// each transport states how it rounds `within`.
    Persisted {
        /// The longest wait the caller accepts.
        within: Duration,
    },
}

/// Broker `last_sequence` of a producer's chain on one topic. The chain is
/// kept per `(producer, topic)`: no partition is part of it (ADR-0004
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
    pub id: ProducerId,
    pub client_agent: String,
    pub topics: Vec<TopicCursors>,
}

impl ProducerCursors {
    /// True when the producer has no recorded cursor on any topic.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.topics.is_empty()
    }

    /// The broker's known `last_sequence` for `topic`, if any.
    #[must_use]
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
    pub id: SubscriptionId,
    pub topology_version: i64,
    pub assigned: Vec<AssignedPartition>,
    /// The interests whose `types` name entries that matched no event type
    /// registered on the topic at JOIN, in interest order; an interest whose
    /// entries all matched is not listed.
    pub unmatched_types: Vec<UnmatchedTypes>,
}

/// The entries of one JOIN interest's `types` that matched no event type
/// registered on its topic when the broker answered the JOIN. Events of such a
/// type are delivered once it registers and the consume scope admits it.
#[derive(Debug, Clone, PartialEq)]
pub struct UnmatchedTypes {
    /// The interest's position in the JOIN request's interests.
    pub interest: usize,
    /// The interest's entries that matched no registered type, never empty.
    pub types: Vec<GtsIdPattern>,
}

/// Request body for a JOIN.
#[derive(Debug, Clone)]
pub struct JoinRequest {
    pub consumer_group: ConsumerGroupId,
    /// RFC 9110 User-Agent grammar; ASCII 1-256 bytes.
    pub client_agent: String,
    /// Per-member interests (topic-anchored typed-filter selections per ADR-0005).
    pub interests: Vec<SubscriptionInterest>,
    /// How long the subscription survives after its stream closes; it is kept alive while a
    /// stream is open. Default PT30S. Must be positive: the broker refuses a zero duration with
    /// `400` (`InvalidSessionTimeout`).
    pub session_timeout: Option<Duration>,
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
    /// Last delivered sequence in this stream (not stored by the broker).
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
#[expect(
    clippy::large_enum_variant,
    reason = "`WireFrame::Event(EventV1)` stays unboxed to keep the public stream API shape"
)]
#[derive(Debug, Clone)]
pub enum WireFrame {
    /// A delivered event as stored: its source and the broker-stamped position
    /// (partition, sequence, sequence time) are set. A malformed stored `trace_parent`
    /// (an event written before ingest checked the field) is dropped on delivery with a
    /// warning, so the event arrives without it. It names no topic: a topic is what the
    /// event's type declares, so the consumer resolves it from the type.
    Event(EventV1),
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

/// Which of the events the consume scope admits a subscription interest reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OwnershipFilter {
    /// Events of the caller's own tenant whose `subject_owner` is the caller.
    /// An event with no owner is never delivered.
    #[default]
    OnlyMine,
    /// Every event of the interest's tenant, owned or not.
    EntireTenant,
    /// Every event of the interest's tenant and of the tenants below it, as
    /// far as the barrier mode and the consume scope reach.
    TenantSubtreeIncluded,
}

/// The tenant a subscription interest reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InterestTenant {
    /// The caller's own tenant, resolved by the broker. The JOIN names none.
    #[default]
    Caller,
    /// A named tenant; under [`OwnershipFilter::TenantSubtreeIncluded`] the
    /// root of the subtree read.
    Tenant(Uuid),
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
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidConsumerOptions`] when `expression` is empty or
    /// longer than 4096 bytes.
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

    #[must_use]
    pub fn engine(&self) -> &GtsInstanceId {
        &self.engine
    }

    #[must_use]
    pub fn expression(&self) -> &str {
        &self.expression
    }
}

/// One interest entry for a subscription JOIN.
#[derive(Debug, Clone)]
pub struct SubscriptionInterest {
    /// Topic the interest subscribes to.
    pub(crate) topic: GtsInstanceId,
    /// The tenant whose events are of interest.
    pub(crate) subject_tenant: InterestTenant,
    /// Which of the tenant's events the interest reads.
    pub(crate) ownership_filter: OwnershipFilter,
    /// How tenant barriers are treated while traversing descendants.
    pub(crate) barrier_mode: BarrierMode,
    /// Event type patterns to match; empty means all types.
    pub(crate) types: Vec<GtsIdPattern>,
    /// Subject-type patterns an event's `subject_type` must match; empty means no selector.
    pub(crate) subject_types: Vec<GtsIdPattern>,
    /// Optional extra content filter applied to matching events.
    pub(crate) filter: Option<Filter>,
}

#[derive(Debug, Default)]
pub struct SubscriptionInterestBuilder {
    topic: Option<GtsInstanceId>,
    subject_tenant: InterestTenant,
    ownership_filter: OwnershipFilter,
    barrier_mode: BarrierMode,
    types: Vec<GtsIdPattern>,
    subject_types: Vec<GtsIdPattern>,
    filter: Option<Filter>,
}

impl SubscriptionInterest {
    #[must_use]
    pub fn builder() -> SubscriptionInterestBuilder {
        SubscriptionInterestBuilder::default()
    }

    #[must_use]
    pub fn topic(&self) -> &GtsInstanceId {
        &self.topic
    }

    /// The tenant the interest reads; [`InterestTenant::Caller`] unless one
    /// was named.
    #[must_use]
    pub fn subject_tenant(&self) -> InterestTenant {
        self.subject_tenant
    }

    #[must_use]
    pub fn ownership_filter(&self) -> OwnershipFilter {
        self.ownership_filter
    }

    #[must_use]
    pub fn barrier_mode(&self) -> BarrierMode {
        self.barrier_mode
    }

    #[must_use]
    pub fn types(&self) -> &[GtsIdPattern] {
        &self.types
    }

    /// The subject-type selector; empty when the interest has none.
    #[must_use]
    pub fn subject_types(&self) -> &[GtsIdPattern] {
        &self.subject_types
    }

    #[must_use]
    pub fn filter(&self) -> Option<&Filter> {
        self.filter.as_ref()
    }
}

impl SubscriptionInterestBuilder {
    #[must_use]
    pub fn topic(mut self, topic: GtsInstanceId) -> Self {
        self.topic = Some(topic);
        self
    }

    /// The tenant whose events the interest reads; the caller's own tenant
    /// when not set. The nil UUID is refused by `build()`. Under
    /// [`OwnershipFilter::OnlyMine`] the broker refuses a tenant other than the
    /// caller's.
    #[must_use]
    pub fn subject_tenant(mut self, tenant: Uuid) -> Self {
        self.subject_tenant = InterestTenant::Tenant(tenant);
        self
    }

    /// Which of the tenant's events the interest reads;
    /// [`OwnershipFilter::OnlyMine`] when not set.
    #[must_use]
    pub fn ownership_filter(mut self, filter: OwnershipFilter) -> Self {
        self.ownership_filter = filter;
        self
    }

    #[must_use]
    pub fn barrier_mode(mut self, barrier_mode: BarrierMode) -> Self {
        self.barrier_mode = barrier_mode;
        self
    }

    #[must_use]
    pub fn types<I>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = GtsIdPattern>,
    {
        self.types = types.into_iter().collect();
        self
    }

    /// Delivers only events whose `subject_type` matches one of `patterns`, matched
    /// like `types`. A delivery filter with no authorization meaning: it never
    /// widens what the caller may read. At most 32 patterns; none means every
    /// subject type.
    #[must_use]
    pub fn subject_types<I>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = GtsIdPattern>,
    {
        self.subject_types = patterns.into_iter().collect();
        self
    }

    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Builds the interest.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::InvalidConsumerOptions`] when the topic is missing,
    /// the named tenant is the nil UUID, the event types are not 1 to 32
    /// entries, or there are more than 32 subject types.
    pub fn build(self) -> Result<SubscriptionInterest, EventBrokerError> {
        let topic = self
            .topic
            .ok_or_else(|| EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest topic is required".to_owned(),
            })?;
        if self.subject_tenant == InterestTenant::Tenant(Uuid::nil()) {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest subject_tenant must not be the nil UUID".to_owned(),
            });
        }
        if self.types.is_empty() || self.types.len() > 32 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "subscription interest event types must be 1..=32 entries (got {})",
                    self.types.len()
                ),
            });
        }
        if self.subject_types.len() > 32 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "subscription interest subject types must be at most 32 entries (got {})",
                    self.subject_types.len()
                ),
            });
        }

        Ok(SubscriptionInterest {
            topic,
            subject_tenant: self.subject_tenant,
            ownership_filter: self.ownership_filter,
            barrier_mode: self.barrier_mode,
            types: self.types,
            subject_types: self.subject_types,
            filter: self.filter,
        })
    }
}

/// Opaque backend configuration envelope.
/// `backend_type` is a full GTS identifier registered with `types-registry-sdk`.
/// `config` is JSON validated against the GTS type's schema.
#[derive(Debug, Clone)]
pub struct StorageBackendConfig {
    pub backend_type: GtsTypeId,
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

    /// Publishes `event` and answers on `ack`. The event must pass
    /// [`EventV1::check_publishable`].
    ///
    /// # Errors
    ///
    /// - [`EventBrokerError::InvalidEventField`] for a broker-stamped member, a missing
    ///   `source` or a `meta.version` the transport cannot carry; nothing is sent.
    /// - Otherwise the broker's error.
    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &EventV1,
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError>;

    /// Publish a batch atomically. A batch is all-or-nothing (a single
    /// mode-shape or chain violation rejects the whole batch, per DESIGN), so
    /// the outcome is one value for the batch, not one per event. Every event must
    /// pass [`EventV1::check_publishable`].
    ///
    /// # Errors
    ///
    /// - [`EventBrokerError::InvalidEventField`] for the first event with a broker-stamped
    ///   member, a missing `source` or a `meta.version` the transport cannot carry; nothing
    ///   is sent.
    /// - Otherwise the broker's error.
    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[EventV1],
        ack: Ack,
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
