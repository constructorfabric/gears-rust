mod builder;
mod commit;
mod consumer;
mod dispatcher;
mod offset_manager;
mod types;

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod builder_tests;
#[cfg(test)]
mod commit_tests;
#[cfg(test)]
#[cfg(feature = "db")]
mod offset_manager_tests;
#[cfg(test)]
mod types_tests;

pub use commit::CommitHandle;
#[cfg(feature = "db")]
pub use commit::TxCommitHandle;

#[cfg(feature = "db")]
pub use builder::WithTx;
pub use builder::{
    BrokerOnly, ConsumerBatchReady, ConsumerBuilder, ConsumerReady, ConsumerRoute,
    ConsumerRouteBuilder, ConsumerRouteHandlerKind, ConsumerRoutedReady, NoDefaultHandler, NoDlq,
    RouteHasTopic, RouteMissingTopic, WithDlq,
};

pub use consumer::{Consumer, ConsumerHandle};
pub use offset_manager::{CommitOffset, Fallback, InMemoryOffsetManager, OffsetStore};
#[cfg(feature = "db")]
pub use offset_manager::{
    CommitOffsetInTx, LOCAL_DB_OFFSET_STORE_MIGRATION_SQL, LocalDbOffsetManager,
};

pub use crate::api::{
    BarrierMode, ControlCode, FrameStream, PartitionPosition, PartitionSlot, ResolvedPosition,
    SeekPosition, SubscriptionAssignment, TenantTraversalDepth, WireEvent, WireFrame,
};
pub use crate::error::OffsetManagerError;
pub use types::{
    BatchEventHandler, CachedTypeRegistryResolver, CompiledFilterRef, ConsumerBatching,
    ConsumerBuffering, ConsumerCommitMode, ConsumerGroupRef, ConsumerListenerSettings,
    ConsumerProfile, ConsumerRetry, ConsumerSettings, ConsumerSettingsOverrides,
    ConsumerSlowDetection, DeadLetterEvent, EventBatch, EventHandler, EventTypeRef,
    EventTypeSelector, FilterEngineId, FilterEngineRef, HandlerOutcome, RawEvent,
    RejectableOutcome, ResolvedSubscriptionFilter, ResolvedSubscriptionInterest,
    SingleEventHandlerAdapter, SubscriptionFilterRef, SubscriptionInterest, TopicRef,
    TypeRegistryResolver,
};
