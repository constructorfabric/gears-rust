//! Event Broker SDK
//!
//! High-level typed event publishing and consumption for the Cyberfabric Event Broker.
//!
//! See [`EventBroker`] for the entry point; obtain it from `ClientHub`:
//! ```ignore
//! let broker = hub.get::<dyn EventBroker>()?;
//! ```

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod consumer;
pub mod dlq;
pub mod error;
pub mod ids;
pub mod models;
pub mod producer;
pub mod sdk;
pub mod typed_event;

#[cfg(test)]
mod api_tests;

#[cfg(feature = "mock")]
pub mod mock;

mod internal;

/// Exposes internals needed by integration tests and the impl crate.
/// Not part of the stable public API.
#[doc(hidden)]
pub mod internal_test_helpers;

pub use api::{
    AssignedPartition, BarrierMode, EventBroker, EventBrokerBackend, JoinRequest, ResolvedPosition,
    SeekResult, StorageBackendConfig, SubscriptionAssignment, TenantTraversalDepth,
};
pub use consumer::{
    BatchHandlerOutcome, CachedTypeRegistryResolver, CommitOffset, CompiledFilterRef,
    ConnectionDropReason, Consumer, ConsumerBatching, ConsumerBuffering, ConsumerBuilder,
    ConsumerCommitMode, ConsumerGroupRef, ConsumerHandler, ConsumerListenerSettings,
    ConsumerProfile, ConsumerRetry, ConsumerRuntimeEvent, ConsumerRuntimeListener,
    ConsumerSettings, ConsumerSettingsOverrides, ConsumerSlowDetection, ControlCode, EventBatch,
    EventTypeRef, EventTypeSelector, Fallback, FilterEngineId, FilterEngineRef, FrameStream,
    HandlerOutcome, InMemoryOffsetManager, OffsetStore, PartitionBufferState,
    PartitionBufferStateSnapshot, PartitionPosition, PartitionProgress, PartitionSlot, RawEvent,
    ResolvedSubscriptionFilter, ResolvedSubscriptionInterest, SeekPosition, SingleEventHandler,
    SlowConsumerTrigger, SubscriptionFilterRef, SubscriptionInterest, TopicRef,
    TypeRegistryResolver, WireEvent, WireFrame,
};
#[cfg(feature = "db")]
pub use consumer::{
    CommitOffsetInTx, LocalDbOffsetManager, TxCommitHandle, TxConsumerHandler,
    TxSingleEventHandler, WithTx,
};

pub use error::{ConsumerError, EventBrokerError, OffsetManagerError, StorageBackendError};
pub use ids::{ConsumerGroupId, EventTypeId, ProducerId, SubscriptionId, TopicId};
pub use models::{
    ConsumerGroup, ConsumerGroupKind, ConsumerGroupQuery, CreateConsumerGroupRequest, Event,
    EventType, Page, PartitionAssignment, PartitionLeader, PartitionRange, ResetScope,
    Subscription, Topic, TopicSegment,
};
#[cfg(feature = "outbox")]
pub use producer::AsyncProducer;
pub use producer::{
    IngestOutcome, ProducerBackend, ProducerBuilder, ProducerCursor, ProducerMode, SyncProducer,
    ValidationTiming,
};
pub use sdk::EventBrokerSdk;
pub use typed_event::{EnvelopedEvent, TypedEvent};
