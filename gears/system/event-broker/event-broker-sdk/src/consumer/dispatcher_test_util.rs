//! Test helpers used exclusively by `#[cfg(feature = "test-util")]` dispatcher integration tests.

use crate::sequence::Sequence;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{
    BatchHandlerOutcome, CommitOffset, ConsumerHandler, ConsumerRuntimeEvent,
    ConsumerRuntimeListener, EventBatch, OffsetManagerError, OffsetStore, Position,
};
use crate::error::ConsumerError;
use crate::ids::{ConsumerGroupId, TopicId};

pub(super) type SharedAttempts = Arc<Mutex<Vec<u16>>>;
pub(super) type SharedTimeline = Arc<Mutex<Vec<&'static str>>>;
pub(super) type CommitRecord = (ConsumerGroupId, TopicId, u32, i64);
pub(super) type SharedCommits = Arc<Mutex<Vec<CommitRecord>>>;
pub(super) type SharedScopes = Arc<Mutex<Vec<(String, u32, usize)>>>;
pub(super) type SharedViolations = Arc<Mutex<Vec<String>>>;
pub(super) type SharedRuntimeEvents = Arc<Mutex<Vec<ConsumerRuntimeEvent>>>;

pub(super) fn partition_key_for_partition(target: u32, partitions: u32) -> String {
    assert_eq!(partitions, 2, "only the two-partition fixture is supported");
    match target {
        0 => "partition-key-0-1",
        1 => "partition-key-1-0",
        _ => panic!("two-partition fixture cannot target partition {target}"),
    }
    .to_owned()
}

type SharedPartitionCalls = Arc<Mutex<Vec<(String, u32, i64)>>>;

pub(super) struct SleepingBatchHandler {
    pub(super) calls: SharedPartitionCalls,
    pub(super) delay: Duration,
}

#[async_trait::async_trait]
impl ConsumerHandler for SleepingBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_>,
        _attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        tokio::time::sleep(self.delay).await;
        let chunk = batch.next_chunk(batch.len());
        self.calls.lock().unwrap().extend(
            chunk
                .iter()
                .map(|event| (event.topic.clone(), event.partition, event.offset.as_i64())),
        );
        Ok(chunk
            .last()
            .map(|event| BatchHandlerOutcome::AdvanceThrough {
                offset: event.offset,
            })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

pub(super) struct FailingThenCommitBatchHandler {
    pub(super) failures_remaining: Arc<Mutex<usize>>,
    pub(super) calls: SharedAttempts,
}

#[async_trait::async_trait]
impl ConsumerHandler for FailingThenCommitBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_>,
        attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        {
            let mut guard = self.failures_remaining.lock().unwrap();
            if *guard > 0 {
                *guard -= 1;
                return Err(ConsumerError::Internal(
                    "intentional representative handler failure".to_owned(),
                ));
            }
        }

        self.calls.lock().unwrap().push(attempts);
        Ok(BatchHandlerOutcome::Success)
    }
}

pub(super) struct SequencedOffsetManager {
    pub(super) timeline: SharedTimeline,
}

#[async_trait::async_trait]
impl OffsetStore for SequencedOffsetManager {
    async fn load_position(
        &self,
        _group: &ConsumerGroupId,
        _topic: &TopicId,
        _partition: u32,
    ) -> Result<Position, OffsetManagerError> {
        self.timeline.lock().unwrap().push("load");
        Ok(Position::Earliest)
    }
}

#[async_trait::async_trait]
impl CommitOffset for SequencedOffsetManager {
    async fn commit(
        &self,
        _group: &ConsumerGroupId,
        _topic: &TopicId,
        _partition: u32,
        _offset: Sequence,
    ) -> Result<(), OffsetManagerError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(super) struct RecordingCommitOffsetManager {
    pub(super) commits: SharedCommits,
}

#[async_trait::async_trait]
impl OffsetStore for RecordingCommitOffsetManager {
    async fn load_position(
        &self,
        _group: &ConsumerGroupId,
        _topic: &TopicId,
        _partition: u32,
    ) -> Result<Position, OffsetManagerError> {
        Ok(Position::Earliest)
    }
}

#[async_trait::async_trait]
impl CommitOffset for RecordingCommitOffsetManager {
    async fn commit(
        &self,
        group: &ConsumerGroupId,
        topic: &TopicId,
        partition: u32,
        offset: Sequence,
    ) -> Result<(), OffsetManagerError> {
        self.commits
            .lock()
            .unwrap()
            .push((*group, *topic, partition, offset.as_i64()));
        Ok(())
    }
}

pub(super) struct SequencedBatchHandler {
    pub(super) timeline: SharedTimeline,
}

#[async_trait::async_trait]
impl ConsumerHandler for SequencedBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_>,
        _attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        self.timeline.lock().unwrap().push("handle");
        Ok(BatchHandlerOutcome::Success)
    }
}

#[derive(Default)]
pub(super) struct BatchScopeRecorder {
    pub(super) scopes: SharedScopes,
    pub(super) violations: SharedViolations,
}

#[async_trait::async_trait]
impl ConsumerHandler for BatchScopeRecorder {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_>,
        _attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        if let Some(first) = chunk.first() {
            if chunk
                .iter()
                .any(|event| event.topic != first.topic || event.partition != first.partition)
            {
                self.violations
                    .lock()
                    .unwrap()
                    .push("batch mixed topic IDs or partitions".to_owned());
            }
            self.scopes
                .lock()
                .unwrap()
                .push((first.topic.clone(), first.partition, chunk.len()));
        }
        Ok(chunk
            .last()
            .map(|event| BatchHandlerOutcome::AdvanceThrough {
                offset: event.offset,
            })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

#[derive(Clone, Default)]
pub(super) struct RecordingRuntimeListener {
    pub(super) events: SharedRuntimeEvents,
}

#[async_trait::async_trait]
impl ConsumerRuntimeListener for RecordingRuntimeListener {
    async fn on_consumer_event(&self, event: &ConsumerRuntimeEvent) -> Result<(), ConsumerError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

pub(super) struct FailingRuntimeListener;

#[async_trait::async_trait]
impl ConsumerRuntimeListener for FailingRuntimeListener {
    async fn on_consumer_event(&self, _event: &ConsumerRuntimeEvent) -> Result<(), ConsumerError> {
        Err(ConsumerError::Internal(
            "intentional listener failure".to_owned(),
        ))
    }
}

pub(super) struct SlowRuntimeListener {
    pub(super) delay: Duration,
}

#[async_trait::async_trait]
impl ConsumerRuntimeListener for SlowRuntimeListener {
    async fn on_consumer_event(&self, _event: &ConsumerRuntimeEvent) -> Result<(), ConsumerError> {
        tokio::time::sleep(self.delay).await;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RuntimeEventKind {
    SubscriptionJoining,
    SubscriptionStarted,
    SubscriptionRejoining,
    SubscriptionTerminated,
    SubscriptionConnectionDropped,
    AssignmentChanged,
    ProgressAdvanced,
    PartitionBufferStateChanged,
    HandlerBatchStarted,
    HandlerBatchCompleted,
    HandlerFailed,
    OffsetLoaded,
    OffsetCommitted,
    RetryScheduled,
}

pub(super) fn runtime_event_kind(event: &ConsumerRuntimeEvent) -> RuntimeEventKind {
    match event {
        ConsumerRuntimeEvent::SubscriptionJoining { .. } => RuntimeEventKind::SubscriptionJoining,
        ConsumerRuntimeEvent::SubscriptionStarted { .. } => RuntimeEventKind::SubscriptionStarted,
        ConsumerRuntimeEvent::SubscriptionRejoining { .. } => {
            RuntimeEventKind::SubscriptionRejoining
        }
        ConsumerRuntimeEvent::SubscriptionTerminated { .. } => {
            RuntimeEventKind::SubscriptionTerminated
        }
        ConsumerRuntimeEvent::SubscriptionConnectionDropped { .. } => {
            RuntimeEventKind::SubscriptionConnectionDropped
        }
        ConsumerRuntimeEvent::AssignmentChanged { .. } => RuntimeEventKind::AssignmentChanged,
        ConsumerRuntimeEvent::ProgressAdvanced { .. } => RuntimeEventKind::ProgressAdvanced,
        ConsumerRuntimeEvent::PartitionBufferStateChanged { .. } => {
            RuntimeEventKind::PartitionBufferStateChanged
        }
        ConsumerRuntimeEvent::HandlerBatchStarted { .. } => RuntimeEventKind::HandlerBatchStarted,
        ConsumerRuntimeEvent::HandlerBatchCompleted { .. } => {
            RuntimeEventKind::HandlerBatchCompleted
        }
        ConsumerRuntimeEvent::HandlerFailed { .. } => RuntimeEventKind::HandlerFailed,
        ConsumerRuntimeEvent::OffsetLoaded { .. } => RuntimeEventKind::OffsetLoaded,
        ConsumerRuntimeEvent::OffsetCommitted { .. } => RuntimeEventKind::OffsetCommitted,
        ConsumerRuntimeEvent::RetryScheduled { .. } => RuntimeEventKind::RetryScheduled,
    }
}

/// A broker decorator over an inner broker (typically the reference
/// `MockBroker`) that injects seek/stream faults, for exercising the consumer's
/// topology-version-mismatch recovery and its fail-fast on `PositionsNotSet`.
/// Every operation delegates unchanged except the injected ones, and the call
/// counters let a test observe the recovery path without racing timing.
pub(super) struct SeekFaultBroker {
    inner: Arc<dyn crate::api::EventBrokerApi>,
    /// Reject the first `seek_mismatches` seeks with `TopologyVersionMismatch`
    /// before delegating (`usize::MAX` = always).
    seek_mismatches: usize,
    /// Reject the first stream open with `PositionsNotSet`.
    stream_positions_not_set_once: bool,
    pub(super) seek_calls: Arc<AtomicUsize>,
    pub(super) get_subscription_calls: Arc<AtomicUsize>,
    pub(super) stream_calls: Arc<AtomicUsize>,
}

impl SeekFaultBroker {
    pub(super) fn new(
        inner: Arc<dyn crate::api::EventBrokerApi>,
        seek_mismatches: usize,
        stream_positions_not_set_once: bool,
    ) -> Self {
        Self {
            inner,
            seek_mismatches,
            stream_positions_not_set_once,
            seek_calls: Arc::new(AtomicUsize::new(0)),
            get_subscription_calls: Arc::new(AtomicUsize::new(0)),
            stream_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl crate::api::EventBrokerApi for SeekFaultBroker {
    async fn register_producer(
        &self,
        ctx: &toolkit_security::SecurityContext,
        mode: crate::api::ProducerMode,
        client_agent: &str,
    ) -> Result<crate::ids::ProducerId, ConsumerError> {
        self.inner.register_producer(ctx, mode, client_agent).await
    }
    async fn publish(
        &self,
        ctx: &toolkit_security::SecurityContext,
        event: &crate::models::Event,
    ) -> Result<crate::api::IngestOutcome, ConsumerError> {
        self.inner.publish(ctx, event).await
    }
    async fn publish_sync(
        &self,
        ctx: &toolkit_security::SecurityContext,
        event: &crate::models::Event,
    ) -> Result<crate::api::IngestOutcome, ConsumerError> {
        self.inner.publish_sync(ctx, event).await
    }
    async fn publish_batch(
        &self,
        ctx: &toolkit_security::SecurityContext,
        events: &[crate::models::Event],
    ) -> Result<crate::api::IngestOutcome, ConsumerError> {
        self.inner.publish_batch(ctx, events).await
    }
    async fn get_producer_cursors(
        &self,
        ctx: &toolkit_security::SecurityContext,
        producer_id: crate::ids::ProducerId,
    ) -> Result<crate::api::ProducerCursors, ConsumerError> {
        self.inner.get_producer_cursors(ctx, producer_id).await
    }
    async fn reset_producer_chain(
        &self,
        ctx: &toolkit_security::SecurityContext,
        producer_id: crate::ids::ProducerId,
        scope: crate::models::ResetScope<'_>,
    ) -> Result<(), ConsumerError> {
        self.inner
            .reset_producer_chain(ctx, producer_id, scope)
            .await
    }
    async fn create_consumer_group(
        &self,
        ctx: &toolkit_security::SecurityContext,
        req: crate::models::CreateConsumerGroupRequest,
    ) -> Result<crate::models::ConsumerGroup, ConsumerError> {
        self.inner.create_consumer_group(ctx, req).await
    }
    async fn get_consumer_group(
        &self,
        ctx: &toolkit_security::SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<crate::models::ConsumerGroup, ConsumerError> {
        self.inner.get_consumer_group(ctx, id).await
    }
    async fn list_consumer_groups(
        &self,
        ctx: &toolkit_security::SecurityContext,
        query: crate::models::ConsumerGroupQuery,
    ) -> Result<crate::models::Page<crate::models::ConsumerGroup>, ConsumerError> {
        self.inner.list_consumer_groups(ctx, query).await
    }
    async fn delete_consumer_group(
        &self,
        ctx: &toolkit_security::SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), ConsumerError> {
        self.inner.delete_consumer_group(ctx, id).await
    }
    async fn join(
        &self,
        ctx: &toolkit_security::SecurityContext,
        req: crate::api::JoinRequest,
    ) -> Result<crate::api::SubscriptionAssignment, ConsumerError> {
        self.inner.join(ctx, req).await
    }
    async fn get_subscription(
        &self,
        ctx: &toolkit_security::SecurityContext,
        id: crate::ids::SubscriptionId,
    ) -> Result<crate::models::Subscription, ConsumerError> {
        self.get_subscription_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.get_subscription(ctx, id).await
    }
    async fn list_subscriptions(
        &self,
        ctx: &toolkit_security::SecurityContext,
    ) -> Result<Vec<crate::models::Subscription>, ConsumerError> {
        self.inner.list_subscriptions(ctx).await
    }
    async fn leave(
        &self,
        ctx: &toolkit_security::SecurityContext,
        id: crate::ids::SubscriptionId,
    ) -> Result<(), ConsumerError> {
        self.inner.leave(ctx, id).await
    }
    async fn stream(
        &self,
        ctx: &toolkit_security::SecurityContext,
        id: crate::ids::SubscriptionId,
    ) -> Result<crate::api::FrameStream, ConsumerError> {
        if self.stream_positions_not_set_once
            && self.stream_calls.fetch_add(1, Ordering::SeqCst) == 0
        {
            return Err(ConsumerError::PositionsNotSet {
                unseeded: Vec::new(),
                detail: "injected".to_owned(),
                instance: String::new(),
            });
        }
        self.inner.stream(ctx, id).await
    }
    async fn seek(
        &self,
        ctx: &toolkit_security::SecurityContext,
        id: crate::ids::SubscriptionId,
        topology_version: i64,
        positions: &[crate::api::SeekPosition],
    ) -> Result<Vec<crate::api::SeekResult>, ConsumerError> {
        let n = self.seek_calls.fetch_add(1, Ordering::SeqCst);
        if n < self.seek_mismatches {
            return Err(ConsumerError::TopologyVersionMismatch {
                detail: "injected".to_owned(),
                instance: String::new(),
            });
        }
        self.inner.seek(ctx, id, topology_version, positions).await
    }
    async fn list_topics(
        &self,
        ctx: &toolkit_security::SecurityContext,
    ) -> Result<Vec<crate::models::Topic>, ConsumerError> {
        self.inner.list_topics(ctx).await
    }
    async fn list_topic_segments(
        &self,
        ctx: &toolkit_security::SecurityContext,
        topic: &str,
        partition: u32,
        range: crate::models::PartitionRange,
    ) -> Result<crate::models::TopicSegment, ConsumerError> {
        self.inner
            .list_topic_segments(ctx, topic, partition, range)
            .await
    }
    async fn list_event_types(
        &self,
        ctx: &toolkit_security::SecurityContext,
    ) -> Result<Vec<crate::models::EventType>, ConsumerError> {
        self.inner.list_event_types(ctx).await
    }
    async fn get_event_type(
        &self,
        ctx: &toolkit_security::SecurityContext,
        id: &str,
    ) -> Result<crate::models::EventType, ConsumerError> {
        self.inner.get_event_type(ctx, id).await
    }
}
