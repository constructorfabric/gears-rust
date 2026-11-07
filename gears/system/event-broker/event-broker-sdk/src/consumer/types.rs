use std::sync::Arc;
use std::time::Duration;
use toolkit_gts::gts_id;

use gts::{GtsIdPattern, GtsInstanceId, GtsSchema, GtsTypeId};
use uuid::Uuid;

#[cfg(feature = "db")]
use super::commit::TxCommitHandle;
#[cfg(feature = "db")]
use super::offset_manager::CommitOffsetInTx;
use crate::api::{AssignedPartition, Position};
use crate::error::{ConsumerError, DecodeCategory, EventBrokerError};
use crate::gts::{EventPayload, EventV1, StoredPosition};
use crate::ids::{ConsumerGroupId, SubscriptionId, TopicId};
use crate::sequence::Sequence;

/// How an event or a batch reaches its handler: the topic and partition it was read
/// from, and which attempt this call is. `attempts` is 1 on the first call and grows by
/// one each time the runtime redelivers the same batch after a `Retry` or an error.
#[derive(Debug, Clone)]
pub struct Delivery {
    topic: GtsInstanceId,
    partition: u32,
    attempts: u16,
}

impl Delivery {
    /// A delivery context. The runtime builds one per handler call; a gear builds one to
    /// call its handler in a test.
    #[must_use]
    pub fn new(topic: GtsInstanceId, partition: u32, attempts: u16) -> Self {
        Self {
            topic,
            partition,
            attempts,
        }
    }

    /// The topic the event or batch was read from.
    #[must_use]
    pub fn topic(&self) -> &GtsInstanceId {
        &self.topic
    }

    /// The partition the event or batch was read from.
    #[must_use]
    pub fn partition(&self) -> u32 {
        self.partition
    }

    /// Which attempt this call is: 1 on the first call.
    #[must_use]
    pub fn attempts(&self) -> u16 {
        self.attempts
    }
}

/// Handler outcome without DLQ - `Reject` is structurally absent.
#[derive(Debug, Clone)]
pub enum HandlerOutcome {
    Success,
    Retry { reason: String },
}

impl HandlerOutcome {
    /// The batch outcome of a single-event handler for the event at `offset`: `Success`
    /// advances through that event, and `Retry` stays a retry.
    pub(crate) fn into_batch_outcome(self, offset: Sequence) -> BatchHandlerOutcome {
        match self {
            Self::Success => BatchHandlerOutcome::AdvanceThrough { offset },
            Self::Retry { reason } => BatchHandlerOutcome::Retry { reason },
        }
    }
}

/// Batch handler outcome. Partial progress is always the contiguous delivered
/// prefix through the given offset.
#[derive(Debug, Clone)]
pub enum BatchHandlerOutcome {
    Success,
    AdvanceThrough { offset: Sequence },
    Retry { reason: String },
}

/// Read-only view over one topic-partition batch. Stateless: each delivery constructs
/// a fresh view, and progress is tracked externally via the handler outcome offset and
/// the committed partition frontier (not by any per-batch cursor).
///
/// Every event in a delivered batch carries its partition, sequence and sequence time,
/// so `event.sequence()` is `Some` for each; the batch's topic is on its [`Delivery`].
// `P: GtsSchema` is the bound `#[gts_type_schema]` puts on `EventV1`'s parameter.
pub struct EventBatch<'a, P: GtsSchema> {
    events: &'a [EventV1<P>],
}

impl<'a, P: GtsSchema> EventBatch<'a, P> {
    /// A view over `events`, in delivery order.
    #[must_use]
    pub fn new(events: &'a [EventV1<P>]) -> Self {
        Self { events }
    }

    /// The first event of the batch.
    #[must_use]
    pub fn next_event(&self) -> Option<&'a EventV1<P>> {
        self.events.first()
    }

    /// The first `n` events of the batch, or all of them when it holds fewer.
    #[must_use]
    pub fn next_chunk(&self, n: usize) -> &'a [EventV1<P>] {
        &self.events[..n.min(self.events.len())]
    }

    /// The events of the batch, in delivery order.
    pub fn iter(&self) -> impl Iterator<Item = &'a EventV1<P>> {
        self.events.iter()
    }

    /// How many events the batch holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the batch holds no event.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

// Prints the length only: the events carry payloads, which may hold personal or
// secret data.
impl<P: GtsSchema> std::fmt::Debug for EventBatch<'_, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBatch")
            .field("len", &self.events.len())
            .finish()
    }
}

/// Tracks the committed frontier for one topic partition.
#[derive(Debug, Clone)]
pub(crate) struct PartitionFrontier {
    committed: Sequence,
}

impl PartitionFrontier {
    pub(crate) fn new(committed: Sequence) -> Self {
        Self { committed }
    }

    pub(crate) fn committed(&self) -> Sequence {
        self.committed
    }
}

/// Consumer-group reference for the builder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConsumerGroupRef {
    Id(ConsumerGroupId),
    Gts(GtsInstanceId),
    AutoAnonymous { alias: String },
}

impl ConsumerGroupRef {
    pub fn id(id: ConsumerGroupId) -> Self {
        Self::Id(id)
    }

    pub fn existing(id: ConsumerGroupId) -> Self {
        Self::Id(id)
    }

    pub fn gts(gts: GtsInstanceId) -> Self {
        Self::Gts(gts)
    }

    pub fn auto_anonymous(alias: impl Into<String>) -> Self {
        Self::AutoAnonymous {
            alias: alias.into(),
        }
    }
}

impl From<ConsumerGroupId> for ConsumerGroupRef {
    fn from(value: ConsumerGroupId) -> Self {
        Self::Id(value)
    }
}

/// Per-interest imperative subscription filter. `engine` is the filter engine's
/// GTS instance id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubscriptionFilterRef {
    pub engine: GtsInstanceId,
    pub expression: String,
}

impl SubscriptionFilterRef {
    pub fn new(engine: GtsInstanceId, expression: impl Into<String>) -> Self {
        Self {
            engine,
            expression: expression.into(),
        }
    }

    pub fn cel(expression: impl Into<String>) -> Self {
        Self::new(
            GtsInstanceId::new(
                gts_id!("cf.core.events.filter.v1~"),
                "cf.core.expression.cel.v1",
            ),
            expression,
        )
    }
}

/// Topic-scoped consumer interest. Event type selectors and filters belong to
/// exactly one topic. A selector is a [`GtsIdPattern`]: a bare type id already
/// covers itself and its derived subtree, and `…event.v1~*` selects every type.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionInterest {
    pub topic: GtsInstanceId,
    pub event_types: Vec<GtsIdPattern>,
    pub filter: Option<SubscriptionFilterRef>,
}

impl SubscriptionInterest {
    pub fn builder() -> SubscriptionInterestBuilder {
        SubscriptionInterestBuilder::default()
    }
}

#[derive(Default)]
pub struct SubscriptionInterestBuilder {
    topic: Option<GtsInstanceId>,
    event_types: Vec<GtsIdPattern>,
    filter: Option<SubscriptionFilterRef>,
}

impl SubscriptionInterestBuilder {
    pub fn topic(mut self, topic: GtsInstanceId) -> Self {
        self.topic = Some(topic);
        self
    }

    pub fn types<I>(mut self, event_types: I) -> Self
    where
        I: IntoIterator<Item = GtsIdPattern>,
    {
        self.event_types.extend(event_types);
        self
    }

    pub fn filter(mut self, filter: SubscriptionFilterRef) -> Self {
        self.filter = Some(filter);
        self
    }

    pub fn build(self) -> Result<SubscriptionInterest, EventBrokerError> {
        let topic = self
            .topic
            .ok_or_else(|| EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest requires a topic".to_owned(),
            })?;
        if self.event_types.is_empty() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "subscription interest requires at least one event type selector"
                    .to_owned(),
            });
        }
        Ok(SubscriptionInterest {
            topic,
            event_types: self.event_types,
            filter: self.filter,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerProfile {
    pub buffering: ConsumerBuffering,
    pub batching: ConsumerBatching,
    pub slow_detection: ConsumerSlowDetection,
    pub retry: ConsumerRetry,
    pub listener: ConsumerListenerSettings,
}

impl ConsumerProfile {
    pub fn default_profile() -> Self {
        Self {
            buffering: ConsumerBuffering {
                partition_capacity: 256,
                high_watermark: 205,
                low_watermark: 128,
            },
            batching: ConsumerBatching {
                max_events: 1,
                max_wait: Duration::from_millis(0),
            },
            slow_detection: ConsumerSlowDetection {
                handler_latency: Duration::from_secs(5),
                handler_strikes: 3,
            },
            retry: ConsumerRetry {
                base_delay: Duration::from_secs(1),
                max_delay: Duration::from_secs(60),
            },
            listener: ConsumerListenerSettings {
                timeout: Duration::from_millis(250),
                channel_capacity: 1024,
            },
        }
    }

    pub fn low_latency() -> Self {
        Self {
            buffering: ConsumerBuffering {
                partition_capacity: 128,
                high_watermark: 96,
                low_watermark: 48,
            },
            batching: ConsumerBatching {
                max_events: 1,
                max_wait: Duration::from_millis(0),
            },
            slow_detection: ConsumerSlowDetection {
                handler_latency: Duration::from_secs(1),
                handler_strikes: 2,
            },
            retry: ConsumerRetry {
                base_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(5),
            },
            listener: ConsumerListenerSettings {
                timeout: Duration::from_millis(100),
                channel_capacity: 2048,
            },
        }
    }

    pub fn high_throughput() -> Self {
        Self {
            buffering: ConsumerBuffering {
                partition_capacity: 1024,
                high_watermark: 819,
                low_watermark: 512,
            },
            batching: ConsumerBatching {
                max_events: 128,
                max_wait: Duration::from_millis(500),
            },
            slow_detection: ConsumerSlowDetection {
                handler_latency: Duration::from_secs(10),
                handler_strikes: 5,
            },
            retry: ConsumerRetry {
                base_delay: Duration::from_secs(1),
                max_delay: Duration::from_secs(120),
            },
            listener: ConsumerListenerSettings {
                timeout: Duration::from_millis(500),
                channel_capacity: 4096,
            },
        }
    }

    pub fn replay() -> Self {
        Self {
            buffering: ConsumerBuffering {
                partition_capacity: 4096,
                high_watermark: 3584,
                low_watermark: 2048,
            },
            batching: ConsumerBatching {
                max_events: 512,
                max_wait: Duration::from_secs(1),
            },
            slow_detection: ConsumerSlowDetection {
                handler_latency: Duration::from_secs(60),
                handler_strikes: 10,
            },
            retry: ConsumerRetry {
                base_delay: Duration::from_secs(2),
                max_delay: Duration::from_secs(300),
            },
            listener: ConsumerListenerSettings {
                timeout: Duration::from_millis(500),
                channel_capacity: 1024,
            },
        }
    }

    pub fn relaxed() -> Self {
        Self {
            buffering: ConsumerBuffering {
                partition_capacity: 128,
                high_watermark: 102,
                low_watermark: 64,
            },
            batching: ConsumerBatching {
                max_events: 16,
                max_wait: Duration::from_secs(2),
            },
            slow_detection: ConsumerSlowDetection {
                handler_latency: Duration::from_secs(30),
                handler_strikes: 5,
            },
            retry: ConsumerRetry {
                base_delay: Duration::from_secs(5),
                max_delay: Duration::from_secs(300),
            },
            listener: ConsumerListenerSettings {
                timeout: Duration::from_millis(250),
                channel_capacity: 256,
            },
        }
    }
}

impl Default for ConsumerProfile {
    fn default() -> Self {
        Self::default_profile()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerSettings {
    pub buffering: ConsumerBuffering,
    pub batching: ConsumerBatching,
    pub slow_detection: ConsumerSlowDetection,
    pub retry: ConsumerRetry,
    pub listener: ConsumerListenerSettings,
}

impl ConsumerSettings {
    pub fn from_profile(profile: ConsumerProfile) -> Self {
        Self {
            buffering: profile.buffering,
            batching: profile.batching,
            slow_detection: profile.slow_detection,
            retry: profile.retry,
            listener: profile.listener,
        }
    }

    pub fn resolve(profile: ConsumerProfile, overrides: ConsumerSettingsOverrides) -> Self {
        Self {
            buffering: overrides.buffering.unwrap_or(profile.buffering),
            batching: overrides.batching.unwrap_or(profile.batching),
            slow_detection: overrides.slow_detection.unwrap_or(profile.slow_detection),
            retry: overrides.retry.unwrap_or(profile.retry),
            listener: overrides.listener.unwrap_or(profile.listener),
        }
    }

    pub fn validate(&self) -> Result<(), EventBrokerError> {
        if self.buffering.partition_capacity == 0 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "partition buffer capacity must be greater than zero".to_owned(),
            });
        }
        if self.buffering.high_watermark > self.buffering.partition_capacity {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "buffer high watermark must not exceed partition capacity".to_owned(),
            });
        }
        if self.buffering.low_watermark > self.buffering.high_watermark {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "buffer low watermark must not exceed high watermark".to_owned(),
            });
        }
        if self.batching.max_events == 0 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "batching max_events must be greater than zero".to_owned(),
            });
        }
        if self.slow_detection.handler_strikes == 0 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "slow detection handler_strikes must be greater than zero".to_owned(),
            });
        }
        if self.retry.max_delay < self.retry.base_delay {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "retry max_delay must be greater than or equal to base_delay".to_owned(),
            });
        }
        if self.listener.channel_capacity == 0 {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "listener channel capacity must be greater than zero".to_owned(),
            });
        }
        if self.listener.timeout.is_zero() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "listener timeout must be greater than zero".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConsumerSettingsOverrides {
    pub buffering: Option<ConsumerBuffering>,
    pub batching: Option<ConsumerBatching>,
    pub slow_detection: Option<ConsumerSlowDetection>,
    pub retry: Option<ConsumerRetry>,
    pub listener: Option<ConsumerListenerSettings>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerBuffering {
    pub partition_capacity: usize,
    pub high_watermark: usize,
    pub low_watermark: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerBatching {
    pub max_events: usize,
    pub max_wait: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerSlowDetection {
    pub handler_latency: Duration,
    pub handler_strikes: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerRetry {
    pub base_delay: Duration,
    pub max_delay: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerCommitMode {
    Auto { interval: Duration },
    Manual,
}

impl ConsumerCommitMode {
    pub fn auto(interval: Duration) -> Self {
        Self::Auto { interval }
    }

    pub fn manual() -> Self {
        Self::Manual
    }
}

impl Default for ConsumerCommitMode {
    fn default() -> Self {
        Self::Auto {
            interval: Duration::from_secs(20),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerListenerSettings {
    pub timeout: Duration,
    pub channel_capacity: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionBufferState {
    Connected,
    SlowDetected,
    ConnectionDropped,
    Draining,
    Rejoining,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlowConsumerTrigger {
    BufferHighWatermark,
    HandlerLatencyStrikes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionBufferStateSnapshot {
    pub group_id: ConsumerGroupId,
    pub subscription_id: SubscriptionId,
    pub topic_id: TopicId,
    pub topic: GtsInstanceId,
    pub partition: u32,
    pub state: PartitionBufferState,
    pub trigger: Option<SlowConsumerTrigger>,
    pub buffered_count: usize,
    pub capacity: usize,
    pub latest_observed_offset: Option<Sequence>,
    pub last_delivered_offset: Option<Sequence>,
    pub consecutive_slow_handlers: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionDropReason {
    SlowConsumer {
        topic_id: TopicId,
        topic: GtsInstanceId,
        partition: u32,
        trigger: SlowConsumerTrigger,
    },
    HeartbeatThreshold,
    StreamTerminated,
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionProgress {
    pub topic_id: TopicId,
    pub topic: GtsInstanceId,
    pub partition: u32,
    pub offset: Sequence,
}

#[derive(Debug, Clone)]
pub enum ConsumerRuntimeEvent {
    SubscriptionJoining {
        group_id: ConsumerGroupId,
    },
    SubscriptionStarted {
        group_id: ConsumerGroupId,
        subscription_id: SubscriptionId,
        assigned: Vec<AssignedPartition>,
    },
    SubscriptionRejoining {
        group_id: ConsumerGroupId,
        previous_subscription_id: SubscriptionId,
    },
    /// The consumer closed a subscription's stream before it re-JOINs. Emitted only for a
    /// stream that opened (got its first frame): one that failed to open or ended before
    /// its first frame reports [`Self::SubscriptionRejoining`] only.
    SubscriptionTerminated {
        subscription_id: SubscriptionId,
        reason: ConnectionDropReason,
    },
    SubscriptionConnectionDropped {
        subscription_id: SubscriptionId,
        reason: ConnectionDropReason,
        affected: Vec<AssignedPartition>,
    },
    AssignmentChanged {
        subscription_id: SubscriptionId,
        assigned: Vec<AssignedPartition>,
    },
    ProgressAdvanced {
        subscription_id: SubscriptionId,
        progress: Vec<PartitionProgress>,
    },
    PartitionBufferStateChanged {
        state: PartitionBufferStateSnapshot,
    },
    HandlerBatchStarted {
        topic_id: TopicId,
        topic: GtsInstanceId,
        partition: u32,
        len: usize,
    },
    HandlerBatchCompleted {
        topic_id: TopicId,
        topic: GtsInstanceId,
        partition: u32,
        outcome: BatchHandlerOutcome,
    },
    HandlerFailed {
        topic_id: TopicId,
        topic: GtsInstanceId,
        partition: u32,
        error: String,
    },
    OffsetLoaded {
        topic_id: TopicId,
        topic: GtsInstanceId,
        partition: u32,
        position: Position,
    },
    OffsetCommitted {
        topic_id: TopicId,
        topic: GtsInstanceId,
        partition: u32,
        offset: Sequence,
    },
    RetryScheduled {
        topic_id: TopicId,
        topic: GtsInstanceId,
        partition: u32,
        attempt: u16,
        delay: Duration,
    },
    /// A typed route whose policy is [`Undecodable::Skip`] advanced past an event that
    /// does not decode as its payload type, without handing it to a handler.
    EventSkipped {
        topic: GtsInstanceId,
        partition: u32,
        sequence: Sequence,
        type_id: GtsTypeId,
        category: DecodeCategory,
    },
}

#[async_trait::async_trait]
pub trait ConsumerRuntimeListener: Send + Sync {
    async fn on_consumer_event(&self, event: &ConsumerRuntimeEvent) -> Result<(), ConsumerError>;
}

/// Handles one delivered event at a time. Returning `Success` advances the consumer
/// past the event; returning `Retry` redelivers it after a backoff.
///
/// # Delivery guarantees
///
/// - Delivery is at-least-once: events after the last committed offset are redelivered
///   after a restart or a rebalance.
/// - Event ids are not unique. The broker refuses a repeated id only within one batch,
///   and dedups only the retries of a deduplicating producer (`producer_id` and
///   `sequence`).
/// - A handler whose effect must not repeat makes that effect idempotent itself.
#[async_trait::async_trait]
pub trait EventHandler<P: EventPayload>: Send + Sync {
    /// Handles `event`, read as `delivery` says. The event carries its partition,
    /// sequence and sequence time.
    ///
    /// # Errors
    ///
    /// Any [`ConsumerError`]; the runtime reports it and treats it as `Retry`.
    async fn handle(
        &self,
        event: EventV1<P>,
        delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError>;
}

/// Handles one topic-partition batch at a time and declares progress through
/// [`BatchHandlerOutcome`]. Every event in the batch carries its partition, sequence and
/// sequence time, so `AdvanceThrough { offset }` takes an event's `sequence()`.
///
/// # Delivery guarantees
///
/// - Delivery is at-least-once: events after the last committed offset are redelivered
///   after a restart or a rebalance.
/// - Event ids are not unique. The broker refuses a repeated id only within one batch,
///   and dedups only the retries of a deduplicating producer (`producer_id` and
///   `sequence`).
/// - A handler whose effect must not repeat makes that effect idempotent itself.
#[async_trait::async_trait]
pub trait BatchHandler<P: EventPayload>: Send + Sync {
    /// Handles `batch`, read as `delivery` says.
    ///
    /// # Errors
    ///
    /// Any [`ConsumerError`]; the runtime reports it and treats it as `Retry`.
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, P>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError>;
}

/// Handles one delivered event at a time and commits its offset in the handler's own
/// transaction through `commit`. The consumer advances only past what was committed.
///
/// # Delivery guarantees
///
/// - Delivery is at-least-once: events after the last committed offset are redelivered
///   after a restart or a rebalance.
/// - Event ids are not unique. The broker refuses a repeated id only within one batch,
///   and dedups only the retries of a deduplicating producer (`producer_id` and
///   `sequence`).
/// - The handler commits the offset in the same transaction as its writes, so while the
///   consumer owns the partition a redelivered event never re-applies a committed
///   transaction. A rebalance can hand the partition to another member before a slow
///   handler's commit lands.
/// - An effect outside the transaction is not covered: a handler whose such effect must
///   not repeat makes it idempotent itself.
#[cfg(feature = "db")]
#[async_trait::async_trait]
pub trait TxEventHandler<P: EventPayload, OM: CommitOffsetInTx>: Send + Sync {
    /// Handles `event`, read as `delivery` says. The event carries its partition,
    /// sequence and sequence time.
    ///
    /// # Errors
    ///
    /// Any [`ConsumerError`]; the runtime reports it and treats it as `Retry`.
    async fn handle(
        &self,
        event: EventV1<P>,
        delivery: &Delivery,
        commit: TxCommitHandle<OM>,
    ) -> Result<HandlerOutcome, ConsumerError>;
}

/// Handles one topic-partition batch at a time and commits an offset of the batch in
/// the handler's own transaction through `commit`. Every event in the batch carries its
/// partition, sequence and sequence time.
///
/// # Delivery guarantees
///
/// - Delivery is at-least-once: events after the last committed offset are redelivered
///   after a restart or a rebalance.
/// - Event ids are not unique. The broker refuses a repeated id only within one batch,
///   and dedups only the retries of a deduplicating producer (`producer_id` and
///   `sequence`).
/// - The handler commits the offset in the same transaction as its writes, so while the
///   consumer owns the partition a redelivered event never re-applies a committed
///   transaction. A rebalance can hand the partition to another member before a slow
///   handler's commit lands.
/// - An effect outside the transaction is not covered: a handler whose such effect must
///   not repeat makes it idempotent itself.
#[cfg(feature = "db")]
#[async_trait::async_trait]
pub trait TxBatchHandler<P: EventPayload, OM: CommitOffsetInTx>: Send + Sync {
    /// Handles `batch`, read as `delivery` says.
    ///
    /// # Errors
    ///
    /// Any [`ConsumerError`]; the runtime reports it and treats it as `Retry`.
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, P>,
        delivery: &Delivery,
        commit: TxCommitHandle<OM>,
    ) -> Result<HandlerOutcome, ConsumerError>;
}

/// What a typed route does with an event whose data does not decode as the route's
/// payload type. A typed [`EventHandler`] route maps it as follows, where `at` is the
/// event's position:
///
/// - `Stall`: `Retry`, whose reason is the decode error, which carries no data; no
///   handler is called.
/// - `Skip`: `AdvanceThrough { at }`, reported as [`ConsumerRuntimeEvent::EventSkipped`].
/// - `Fallback(h)`: `h` gets the raw event; its `Success` -> `AdvanceThrough { at }`,
///   its `Retry` -> `Retry`.
///
/// An event that decodes goes to the typed handler: `Success` -> `AdvanceThrough { at }`,
/// `Retry` -> `Retry`. A handler error, typed or raw, is reported and retried as any
/// handler error is. For a typed [`BatchHandler`] route see
/// [`TypedRouteBuilder::batch_handler`].
///
/// [`TypedRouteBuilder::batch_handler`]: super::TypedRouteBuilder::batch_handler
#[derive(Clone)]
pub enum Undecodable {
    /// Retry the event; the partition does not move past it.
    Stall,
    /// Advance past the event and report it as [`ConsumerRuntimeEvent::EventSkipped`].
    Skip,
    /// Hand the raw event to this handler.
    Fallback(Arc<dyn EventHandler<serde_json::Value>>),
}

// Prints the variant name only: a fallback handler need not implement `Debug`.
impl std::fmt::Debug for Undecodable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stall => "Stall",
            Self::Skip => "Skip",
            Self::Fallback(_) => "Fallback",
        })
    }
}

/// What a transactional typed route does with an event whose data does not decode as
/// the route's payload type. A transactional handler advances the consumer only by
/// committing an offset in its own transaction, and the route has no transaction of
/// its own, so it cannot skip an event.
///
/// A typed [`TxEventHandler`] route maps it as follows:
///
/// - `Stall`: `Retry`, whose reason is the decode error, which carries no data;
///   no handler is called and nothing is committed.
/// - `Fallback(h)`: `h` gets the raw event and the delivery's commit handle; its
///   outcome passes through.
///
/// An event that decodes goes to the typed handler with the delivery's commit handle;
/// its outcome passes through, and the consumer advances through what it committed. A
/// handler error, typed or raw, is reported and retried as any handler error is. For a
/// typed [`TxBatchHandler`] route see [`TxTypedRouteBuilder::batch_handler`].
///
/// [`TxTypedRouteBuilder::batch_handler`]: super::TxTypedRouteBuilder::batch_handler
#[cfg(feature = "db")]
pub enum TxUndecodable<OM: CommitOffsetInTx> {
    /// Retry the event; the partition does not move past it.
    Stall,
    /// Hand the raw event to this handler.
    Fallback(Arc<dyn TxEventHandler<serde_json::Value, OM>>),
}

// Prints the variant name only: a fallback handler need not implement `Debug`.
#[cfg(feature = "db")]
impl<OM: CommitOffsetInTx> std::fmt::Debug for TxUndecodable<OM> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stall => "Stall",
            Self::Fallback(_) => "Fallback",
        })
    }
}

/// What one plain delivery produced: the batch outcome, and the event a typed route's
/// `Skip` policy advanced past, which the runtime reports.
#[derive(Debug, Clone)]
pub(crate) struct Delivered {
    pub(crate) outcome: BatchHandlerOutcome,
    pub(crate) skipped: Option<SkippedEvent>,
}

impl From<BatchHandlerOutcome> for Delivered {
    fn from(outcome: BatchHandlerOutcome) -> Self {
        Self {
            outcome,
            skipped: None,
        }
    }
}

/// An event a typed route skipped because its data does not decode as the payload
/// type. Carries no part of the data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkippedEvent {
    pub(crate) event_id: Uuid,
    pub(crate) sequence: Sequence,
    pub(crate) type_id: GtsTypeId,
    pub(crate) category: DecodeCategory,
}

/// What the plain runtime calls for one topic-partition batch: the events, their
/// positions in the same order, and the delivery context.
#[async_trait::async_trait]
pub(crate) trait DeliverBatch: Send + Sync {
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        delivery: &Delivery,
    ) -> Result<Delivered, ConsumerError>;
}

/// What the transactional runtime calls for one topic-partition batch.
#[cfg(feature = "db")]
#[async_trait::async_trait]
pub(crate) trait DeliverTxBatch<OM: CommitOffsetInTx>: Send + Sync {
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        delivery: &Delivery,
        commit: TxCommitHandle<OM>,
    ) -> Result<HandlerOutcome, ConsumerError>;
}

/// Hands the first event of each batch to an [`EventHandler`].
pub(crate) struct EventHandlerAdapter<H> {
    inner: H,
}

impl<H> EventHandlerAdapter<H> {
    pub(crate) fn new(inner: H) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl<H> DeliverBatch for EventHandlerAdapter<H>
where
    H: EventHandler<serde_json::Value> + 'static,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        delivery: &Delivery,
    ) -> Result<Delivered, ConsumerError> {
        let Some((event, at)) = batch.next_event().zip(positions.first()) else {
            return Ok(BatchHandlerOutcome::Success.into());
        };

        let outcome = self.inner.handle(event.clone(), delivery).await?;
        Ok(outcome.into_batch_outcome(at.sequence).into())
    }
}

/// Decodes the first event of each batch as `P` and hands it to an [`EventHandler<P>`];
/// an event that does not decode goes by the route's [`Undecodable`] policy.
pub(crate) struct TypedEventHandlerAdapter<H, P> {
    inner: H,
    undecodable: Undecodable,
    _payload: std::marker::PhantomData<fn() -> P>,
}

impl<H, P> TypedEventHandlerAdapter<H, P> {
    pub(crate) fn new(inner: H, undecodable: Undecodable) -> Self {
        Self {
            inner,
            undecodable,
            _payload: std::marker::PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<H, P> DeliverBatch for TypedEventHandlerAdapter<H, P>
where
    H: EventHandler<P> + 'static,
    P: EventPayload,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        delivery: &Delivery,
    ) -> Result<Delivered, ConsumerError> {
        let Some((event, at)) = batch.next_event().zip(positions.first()) else {
            return Ok(BatchHandlerOutcome::Success.into());
        };
        let offset = at.sequence;

        let err = match event.decode::<P>() {
            Ok(typed) => {
                let outcome = self.inner.handle(typed, delivery).await?;
                return Ok(outcome.into_batch_outcome(offset).into());
            }
            Err(err) => err,
        };
        // `decode` fails only with `Decode` today, so this branch is kept untested; it stays so
        // that any other error kind is reported and retried, not taken for undecodable data.
        let EventBrokerError::Decode { category, .. } = err else {
            return Err(err);
        };
        match &self.undecodable {
            Undecodable::Stall => Ok(BatchHandlerOutcome::Retry {
                reason: err.to_string(),
            }
            .into()),
            Undecodable::Skip => Ok(Delivered {
                outcome: BatchHandlerOutcome::AdvanceThrough { offset },
                skipped: Some(SkippedEvent {
                    event_id: event.id(),
                    sequence: offset,
                    type_id: event.event_type().clone(),
                    category,
                }),
            }),
            Undecodable::Fallback(fallback) => {
                let outcome = fallback.handle(event.clone(), delivery).await?;
                Ok(outcome.into_batch_outcome(offset).into())
            }
        }
    }
}

/// Decodes each batch as `P` and hands its decoded prefix to a [`BatchHandler<P>`]; the
/// first event that does not decode goes by the route's [`Undecodable`] policy.
///
/// Decoding runs in delivery order and stops at the first event that does not decode,
/// `bad`. The events before it are the typed prefix, `last` is the position of the
/// prefix's last event, and the events after `bad` are the tail: they are not decoded,
/// and no outcome advances through them.
///
/// - Every event decodes: the handler's outcome passes through unchanged; an empty
///   batch is `Success` without calling the handler.
/// - A non-empty prefix: the handler gets it first. Its `Retry` and an
///   `AdvanceThrough` short of `last` are returned without running the policy. An
///   `AdvanceThrough` to a position outside the prefix (`bad`'s included) is the
///   handler's error, [`EventBrokerError::InvalidConsumerOptions`]. Its `Success` or
///   `AdvanceThrough { last }` goes on to the policy.
/// - `Stall`: `AdvanceThrough { last }` after a prefix; with none, `Retry`, whose
///   reason is the decode error, which carries no data.
/// - `Skip`: `AdvanceThrough` through `bad`, reported as
///   [`ConsumerRuntimeEvent::EventSkipped`].
/// - `Fallback(h)`: `h` gets the raw `bad`. Its `Success` advances through `bad`; its
///   `Retry` gives `AdvanceThrough { last }` after a prefix, else `Retry`.
///
/// A handler error, typed or fallback, passes through: the runtime reports it and
/// retries the whole batch, so a prefix the typed handler already handled is handed
/// to it again (at-least-once).
pub(crate) struct TypedBatchHandlerAdapter<H, P> {
    inner: H,
    undecodable: Undecodable,
    _payload: std::marker::PhantomData<fn() -> P>,
}

impl<H, P> TypedBatchHandlerAdapter<H, P> {
    pub(crate) fn new(inner: H, undecodable: Undecodable) -> Self {
        Self {
            inner,
            undecodable,
            _payload: std::marker::PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<H, P> DeliverBatch for TypedBatchHandlerAdapter<H, P>
where
    H: BatchHandler<P> + 'static,
    P: EventPayload,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        delivery: &Delivery,
    ) -> Result<Delivered, ConsumerError> {
        let mut typed = Vec::with_capacity(batch.len());
        let mut undecodable = None;
        for (event, at) in batch.iter().zip(positions) {
            match event.decode::<P>() {
                Ok(decoded) => typed.push(decoded),
                Err(err) => {
                    undecodable = Some((event, at.sequence, err));
                    break;
                }
            }
        }

        let Some((bad, bad_at, err)) = undecodable else {
            if typed.is_empty() {
                return Ok(BatchHandlerOutcome::Success.into());
            }
            let outcome = self
                .inner
                .handle_batch(&EventBatch::new(&typed), delivery)
                .await?;
            return Ok(outcome.into());
        };
        // `decode` fails only with `Decode` today, so this branch is kept untested; it stays so
        // that any other error kind is reported and retried, not taken for undecodable data.
        let EventBrokerError::Decode { category, .. } = err else {
            return Err(err);
        };

        let prefix = &positions[..typed.len()];
        let last = prefix.last().map(|at| at.sequence);
        if let Some(last) = last {
            let outcome = self
                .inner
                .handle_batch(&EventBatch::new(&typed), delivery)
                .await?;
            match outcome {
                BatchHandlerOutcome::Retry { .. } => return Ok(outcome.into()),
                BatchHandlerOutcome::AdvanceThrough { offset } if offset != last => {
                    // The handler saw only the prefix, so an offset outside it is its error.
                    BatchHandlerOutcome::processed_count_through(offset, prefix)?;
                    return Ok(outcome.into());
                }
                BatchHandlerOutcome::Success | BatchHandlerOutcome::AdvanceThrough { .. } => {}
            }
        }

        match &self.undecodable {
            Undecodable::Stall => Ok(last
                .map_or_else(
                    || BatchHandlerOutcome::Retry {
                        reason: err.to_string(),
                    },
                    |offset| BatchHandlerOutcome::AdvanceThrough { offset },
                )
                .into()),
            Undecodable::Skip => Ok(Delivered {
                outcome: BatchHandlerOutcome::AdvanceThrough { offset: bad_at },
                skipped: Some(SkippedEvent {
                    event_id: bad.id(),
                    sequence: bad_at,
                    type_id: bad.event_type().clone(),
                    category,
                }),
            }),
            Undecodable::Fallback(fallback) => {
                let outcome = match fallback.handle(bad.clone(), delivery).await? {
                    HandlerOutcome::Success => {
                        BatchHandlerOutcome::AdvanceThrough { offset: bad_at }
                    }
                    HandlerOutcome::Retry { reason } => last
                        .map_or(BatchHandlerOutcome::Retry { reason }, |offset| {
                            BatchHandlerOutcome::AdvanceThrough { offset }
                        }),
                };
                Ok(outcome.into())
            }
        }
    }
}

/// Hands each batch to a [`BatchHandler`].
pub(crate) struct BatchHandlerAdapter<H> {
    inner: H,
}

impl<H> BatchHandlerAdapter<H> {
    pub(crate) fn new(inner: H) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl<H> DeliverBatch for BatchHandlerAdapter<H>
where
    H: BatchHandler<serde_json::Value> + 'static,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _positions: &[StoredPosition],
        delivery: &Delivery,
    ) -> Result<Delivered, ConsumerError> {
        Ok(self.inner.handle_batch(batch, delivery).await?.into())
    }
}

/// Hands the first event of each batch to a [`TxEventHandler`].
#[cfg(feature = "db")]
pub(crate) struct TxEventHandlerAdapter<H, OM: CommitOffsetInTx> {
    inner: H,
    _offset_manager: std::marker::PhantomData<OM>,
}

#[cfg(feature = "db")]
impl<H, OM: CommitOffsetInTx> TxEventHandlerAdapter<H, OM> {
    pub(crate) fn new(inner: H) -> Self {
        Self {
            inner,
            _offset_manager: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl<H, OM> DeliverTxBatch<OM> for TxEventHandlerAdapter<H, OM>
where
    H: TxEventHandler<serde_json::Value, OM> + 'static,
    OM: CommitOffsetInTx + 'static,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _positions: &[StoredPosition],
        delivery: &Delivery,
        commit: TxCommitHandle<OM>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let Some(event) = batch.next_event() else {
            return Ok(HandlerOutcome::Success);
        };

        self.inner.handle(event.clone(), delivery, commit).await
    }
}

/// Hands each batch to a [`TxBatchHandler`].
#[cfg(feature = "db")]
pub(crate) struct TxBatchHandlerAdapter<H, OM: CommitOffsetInTx> {
    inner: H,
    _offset_manager: std::marker::PhantomData<OM>,
}

#[cfg(feature = "db")]
impl<H, OM: CommitOffsetInTx> TxBatchHandlerAdapter<H, OM> {
    pub(crate) fn new(inner: H) -> Self {
        Self {
            inner,
            _offset_manager: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl<H, OM> DeliverTxBatch<OM> for TxBatchHandlerAdapter<H, OM>
where
    H: TxBatchHandler<serde_json::Value, OM> + 'static,
    OM: CommitOffsetInTx + 'static,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _positions: &[StoredPosition],
        delivery: &Delivery,
        commit: TxCommitHandle<OM>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.inner.handle_batch(batch, delivery, commit).await
    }
}

/// Decodes the first event of each delivery as `P` and hands it to a
/// [`TxEventHandler<P, OM>`] with the delivery's commit handle; an event that does not
/// decode goes by the route's [`TxUndecodable`] policy.
#[cfg(feature = "db")]
pub(crate) struct TxTypedEventHandlerAdapter<H, P, OM: CommitOffsetInTx> {
    inner: H,
    undecodable: TxUndecodable<OM>,
    _payload: std::marker::PhantomData<fn() -> P>,
}

#[cfg(feature = "db")]
impl<H, P, OM: CommitOffsetInTx> TxTypedEventHandlerAdapter<H, P, OM> {
    pub(crate) fn new(inner: H, undecodable: TxUndecodable<OM>) -> Self {
        Self {
            inner,
            undecodable,
            _payload: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl<H, P, OM> DeliverTxBatch<OM> for TxTypedEventHandlerAdapter<H, P, OM>
where
    H: TxEventHandler<P, OM> + 'static,
    P: EventPayload,
    OM: CommitOffsetInTx + 'static,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _positions: &[StoredPosition],
        delivery: &Delivery,
        commit: TxCommitHandle<OM>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let Some(event) = batch.next_event() else {
            return Ok(HandlerOutcome::Success);
        };

        let err = match event.decode::<P>() {
            Ok(typed) => return self.inner.handle(typed, delivery, commit).await,
            Err(err) => err,
        };
        // `decode` fails only with `Decode` today, so this branch is kept untested; it stays so
        // that any other error kind is reported and retried, not taken for undecodable data.
        if !matches!(err, EventBrokerError::Decode { .. }) {
            return Err(err);
        }
        match &self.undecodable {
            TxUndecodable::Stall => Ok(HandlerOutcome::Retry {
                reason: err.to_string(),
            }),
            TxUndecodable::Fallback(fallback) => {
                fallback.handle(event.clone(), delivery, commit).await
            }
        }
    }
}

/// Decodes each delivery as `P` and hands its decoded prefix to a
/// [`TxBatchHandler<P, OM>`]; the first event that does not decode goes by the route's
/// [`TxUndecodable`] policy.
///
/// Decoding runs in delivery order and stops at the first event that does not decode,
/// `bad`. The events before it are the typed prefix, `last` is the position of the
/// prefix's last event, and the events after `bad` are the tail: they are not decoded.
/// Each handler gets a commit handle over its own events only (the prefix, or `bad`),
/// sharing the delivery's commit state, which the runtime reads to advance.
///
/// - Every event decodes: the handler gets the delivery's handle and its outcome passes
///   through; an empty delivery is `Success` without calling the handler.
/// - A non-empty prefix: the handler gets it first. Its `Retry` is returned without the
///   policy. After its `Success`, the policy runs only if it committed `last`. With an
///   earlier offset committed, `Success` is returned and the runtime advances through
///   that offset; with none, its `Success` passes through and the runtime reports the
///   missing commit.
/// - `Stall`: `Success` after a committed prefix, so the runtime advances through it;
///   with no prefix, `Retry`, whose reason is the decode error, which carries no data.
/// - `Fallback(h)`: `h` gets the raw `bad`. Its `Success` passes through; its `Retry`
///   gives `Success` after a committed prefix, else `Retry`.
///
/// A handler error passes through and the runtime retries the whole delivery, except a
/// fallback error after a committed prefix: the prefix's transaction is done, so the
/// error is logged and `Success` returned, and while the consumer owns the partition the
/// prefix is not handed to the handler again.
#[cfg(feature = "db")]
pub(crate) struct TxTypedBatchHandlerAdapter<H, P, OM: CommitOffsetInTx> {
    inner: H,
    undecodable: TxUndecodable<OM>,
    _payload: std::marker::PhantomData<fn() -> P>,
}

#[cfg(feature = "db")]
impl<H, P, OM: CommitOffsetInTx> TxTypedBatchHandlerAdapter<H, P, OM> {
    pub(crate) fn new(inner: H, undecodable: TxUndecodable<OM>) -> Self {
        Self {
            inner,
            undecodable,
            _payload: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl<H, P, OM> DeliverTxBatch<OM> for TxTypedBatchHandlerAdapter<H, P, OM>
where
    H: TxBatchHandler<P, OM> + 'static,
    P: EventPayload,
    OM: CommitOffsetInTx + 'static,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        delivery: &Delivery,
        commit: TxCommitHandle<OM>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let mut typed = Vec::with_capacity(batch.len());
        let mut undecodable = None;
        for (event, at) in batch.iter().zip(positions) {
            match event.decode::<P>() {
                Ok(decoded) => typed.push(decoded),
                Err(err) => {
                    undecodable = Some((event, at.sequence, err));
                    break;
                }
            }
        }

        let Some((bad, bad_at, err)) = undecodable else {
            if typed.is_empty() {
                return Ok(HandlerOutcome::Success);
            }
            return self
                .inner
                .handle_batch(&EventBatch::new(&typed), delivery, commit)
                .await;
        };
        // `decode` fails only with `Decode` today, so this branch is kept untested; it stays so
        // that any other error kind is reported and retried, not taken for undecodable data.
        if !matches!(err, EventBrokerError::Decode { .. }) {
            return Err(err);
        }

        let prefix: Vec<Sequence> = positions[..typed.len()]
            .iter()
            .map(|at| at.sequence)
            .collect();
        let last = prefix.last().copied();
        if let Some(last) = last {
            let outcome = self
                .inner
                .handle_batch(&EventBatch::new(&typed), delivery, commit.scoped(prefix))
                .await?;
            if let HandlerOutcome::Retry { .. } = outcome {
                return Ok(outcome);
            }
            // The guard is dropped at the end of the statement; nothing awaits under it.
            let committed = *commit
                .committed_offset
                .lock()
                .map_err(|_| EventBrokerError::Internal("tx commit state mutex poisoned".into()))?;
            match committed {
                Some(offset) if offset == last => {}
                // Part of the prefix committed: the runtime advances through it, and `bad`
                // is not reached.
                Some(_) => return Ok(HandlerOutcome::Success),
                // Nothing committed: the runtime reports it as for any handler.
                None => return Ok(outcome),
            }
        }
        // From here on, a prefix exists only if it is committed.
        let prefix_committed = last.is_some();

        match &self.undecodable {
            TxUndecodable::Stall => Ok(if prefix_committed {
                HandlerOutcome::Success
            } else {
                HandlerOutcome::Retry {
                    reason: err.to_string(),
                }
            }),
            TxUndecodable::Fallback(fallback) => {
                match fallback
                    .handle(bad.clone(), delivery, commit.scoped(vec![bad_at]))
                    .await
                {
                    Ok(HandlerOutcome::Retry { .. }) if prefix_committed => {
                        Ok(HandlerOutcome::Success)
                    }
                    Err(e) if prefix_committed => {
                        tracing::warn!(
                            error = %e,
                            "transactional fallback handler returned Err after a committed \
                             typed prefix; treating as Success"
                        );
                        Ok(HandlerOutcome::Success)
                    }
                    result => result,
                }
            }
        }
    }
}
