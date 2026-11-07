//! Event Broker SDK
//!
//! High-level typed event publishing and consumption for the `event-broker` gear.
//!
//! See [`EventBrokerApi`] for the entry point; obtain it from `ClientHub`:
//! ```ignore
//! let broker = hub.get::<dyn EventBrokerApi>()?;
//! ```

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod backend;
pub mod consumer;
pub mod dlq;
pub mod error;
pub mod gts;
pub mod ids;
pub mod models;
pub mod producer;
pub mod sdk;
pub mod sequence;
pub mod validate;

#[cfg(test)]
mod api_tests;

#[cfg(test)]
mod broker_stub;

#[cfg(test)]
mod gts_event_tests;

#[cfg(test)]
mod gts_tests;

#[cfg(test)]
mod sequence_tests;

#[cfg(test)]
mod validate_tests;

#[cfg(all(test, any(feature = "db", feature = "rest-client")))]
mod warn_capture;

#[cfg(feature = "rest-client")]
pub mod rest;

pub use api::{
    Ack, AssignedPartition, BarrierMode, EventBrokerApi, IngestOutcome, JoinRequest, Position,
    ProducerCursors, ProducerMode, SeekResult, StorageBackendConfig, SubscriptionAssignment,
    TenantTraversalDepth, TopicCursors,
};
pub use consumer::{
    BatchHandler, BatchHandlerOutcome, CommitOffset, ConnectionDropReason, Consumer,
    ConsumerBatching, ConsumerBuffering, ConsumerBuilder, ConsumerCommitMode, ConsumerGroupRef,
    ConsumerListenerSettings, ConsumerProfile, ConsumerRetry, ConsumerRuntimeEvent,
    ConsumerRuntimeListener, ConsumerSettings, ConsumerSettingsOverrides, ConsumerSlowDetection,
    ControlCode, Delivery, EventBatch, EventHandler, Fallback, FrameStream, HandlerOutcome,
    InMemoryOffsetManager, OffsetStore, PartitionBufferState, PartitionBufferStateSnapshot,
    PartitionPosition, PartitionProgress, SeekPosition, SlowConsumerTrigger, SubscriptionFilterRef,
    SubscriptionInterest, TypedRouteBuilder, Undecodable, WireFrame,
};
#[cfg(feature = "db")]
pub use consumer::{
    CommitOffsetInTx, LocalDbOffsetManager, TxBatchHandler, TxCommitHandle, TxEventHandler,
    TxTypedRouteBuilder, TxUndecodable, WithTx,
};

pub use error::{
    ConsumerError, DecodeCategory, EventBrokerError, OffsetManagerError, OutOfRange,
    PositionViolation,
};
pub use gts::{
    CONSUMER_GROUP_RESOURCE_TYPE, EVENT_TYPE_RESOURCE_TYPE, EventPayload, EventV1, EventV1Builder,
    PRODUCER_RESOURCE_TYPE, REQUEST_RESOURCE_TYPE, SUBSCRIPTION_RESOURCE_TYPE, StoredPosition,
    SubjectV1, TopicV1,
};
// The canonical GTS id/pattern types (from the external `gts` crate, reached via
// `::gts` because this crate's own `gts` module shadows the name here) and the
// `gts_id!` macro, re-exported so callers build typed ids without a direct gts dep.
pub use ::gts::{GtsIdPattern, GtsInstanceId, GtsTypeId};
pub use ids::{ConsumerGroupId, EventTypeId, ProducerId, SubscriptionId, TopicId};
pub use models::{
    ConsumerGroup, ConsumerGroupKind, ConsumerGroupQuery, CreateConsumerGroupRequest, Page,
    PartitionAssignment, PartitionRange, ResetScope, Subscription, Topic, TopicSegment,
};
#[cfg(feature = "db")]
pub use producer::producer_registration_migrations;
pub use producer::{DirectProducer, DirectProducerBuilder};
#[cfg(feature = "outbox")]
pub use producer::{
    ManagedRegistration, OutboxBinder, OutboxProducer, OutboxProducerHandle, OutboxRegistration,
    OutboxSetup, OutboxSetupBuilder,
};
pub use sdk::EventBrokerSdk;
pub use sequence::Sequence;
pub use toolkit_gts::gts_id;
