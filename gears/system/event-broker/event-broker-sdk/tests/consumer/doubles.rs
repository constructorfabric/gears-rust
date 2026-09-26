//! Test doubles shared by the harness-driven consumer tests.
//!
//! These are client-side doubles only - handlers, offset stores, listeners, and
//! a fault-injecting broker *decorator*. `SeekFaultBroker` wraps a real
//! `Arc<dyn EventBrokerApi>` (the harness broker) and delegates every operation
//! unchanged except the seek/stream faults it injects, so a test can exercise
//! the consumer's topology-mismatch recovery and its fail-fast on
//! `PositionsNotSet` against the real broker. Nothing here fakes broker
//! behaviour.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use event_broker_sdk::models::{
    ConsumerGroup, ConsumerGroupQuery, CreateConsumerGroupRequest, Event, EventType, Page,
    PartitionRange, ResetScope, Subscription, Topic, TopicSegment,
};
use event_broker_sdk::{
    BatchHandlerOutcome, CommitOffset, ConsumerError, ConsumerGroupId, ConsumerHandler,
    ConsumerRuntimeEvent, ConsumerRuntimeListener, EventBatch, EventBrokerApi, FrameStream,
    IngestOutcome, JoinRequest, OffsetManagerError, OffsetStore, Position, ProducerCursors,
    ProducerId, ProducerMode, SeekPosition, SeekResult, Sequence, SubscriptionAssignment,
    SubscriptionId, TopicId,
};

pub(super) type SharedScopes = Arc<Mutex<Vec<(String, u32, usize)>>>;
pub(super) type SharedViolations = Arc<Mutex<Vec<String>>>;
pub(super) type SharedNamedOffsets = Arc<Mutex<Vec<(&'static str, i64)>>>;
pub(super) type SharedCommits = Arc<Mutex<Vec<(ConsumerGroupId, TopicId, u32, i64)>>>;

/// Records the first event of each batch as `(name, offset)` and returns a
/// fixed outcome - a handler for asserting what the runtime dispatched.
pub(super) struct RecordingBatchHandler {
    pub(super) name: &'static str,
    pub(super) calls: SharedNamedOffsets,
    pub(super) outcome: BatchHandlerOutcome,
}

#[async_trait]
impl ConsumerHandler for RecordingBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_>,
        _attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        if let Some(event) = batch.next_event() {
            self.calls
                .lock()
                .unwrap()
                .push((self.name, event.offset.as_i64()));
        }
        Ok(self.outcome.clone())
    }
}

/// Records the `(topic, partition, len)` scope of every batch and flags any
/// batch that mixed topics or partitions - the invariant the runtime must keep.
#[derive(Default)]
pub(super) struct BatchScopeRecorder {
    pub(super) scopes: SharedScopes,
    pub(super) violations: SharedViolations,
}

#[async_trait]
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
            self.scopes.lock().unwrap().push((
                first.topic.to_string(),
                first.partition,
                chunk.len(),
            ));
        }
        Ok(chunk
            .last()
            .map(|event| BatchHandlerOutcome::AdvanceThrough {
                offset: event.offset,
            })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

/// A broker decorator over the real harness broker that injects seek/stream
/// faults, for exercising the consumer's topology-version-mismatch recovery and
/// its fail-fast on `PositionsNotSet`. Every operation delegates unchanged
/// except the injected ones; the call counters let a test observe the recovery
/// path without racing timing.
pub(super) struct SeekFaultBroker {
    inner: Arc<dyn EventBrokerApi>,
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
        inner: Arc<dyn EventBrokerApi>,
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
impl EventBrokerApi for SeekFaultBroker {
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, ConsumerError> {
        self.inner.register_producer(ctx, mode, client_agent).await
    }
    async fn publish(
        &self,
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, ConsumerError> {
        self.inner.publish(ctx, event).await
    }
    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[Event],
    ) -> Result<IngestOutcome, ConsumerError> {
        self.inner.publish_batch(ctx, events).await
    }
    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<ProducerCursors, ConsumerError> {
        self.inner.get_producer_cursors(ctx, producer_id).await
    }
    async fn reset_producer_chain(
        &self,
        ctx: &SecurityContext,
        producer_id: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), ConsumerError> {
        self.inner
            .reset_producer_chain(ctx, producer_id, scope)
            .await
    }
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, ConsumerError> {
        self.inner.create_consumer_group(ctx, req).await
    }
    async fn get_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, ConsumerError> {
        self.inner.get_consumer_group(ctx, id).await
    }
    async fn list_consumer_groups(
        &self,
        ctx: &SecurityContext,
        query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, ConsumerError> {
        self.inner.list_consumer_groups(ctx, query).await
    }
    async fn delete_consumer_group(
        &self,
        ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), ConsumerError> {
        self.inner.delete_consumer_group(ctx, id).await
    }
    async fn join(
        &self,
        ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, ConsumerError> {
        self.inner.join(ctx, req).await
    }
    async fn get_subscription(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<Subscription, ConsumerError> {
        self.get_subscription_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.get_subscription(ctx, id).await
    }
    async fn list_subscriptions(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, ConsumerError> {
        self.inner.list_subscriptions(ctx).await
    }
    async fn leave(&self, ctx: &SecurityContext, id: SubscriptionId) -> Result<(), ConsumerError> {
        self.inner.leave(ctx, id).await
    }
    async fn stream(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, ConsumerError> {
        if self.stream_positions_not_set_once
            && self.stream_calls.fetch_add(1, Ordering::SeqCst) == 0
        {
            return Err(ConsumerError::PositionsNotSet {
                unseeded: Vec::new(),
                detail: "injected".to_owned(),
            });
        }
        self.inner.stream(ctx, id).await
    }
    async fn seek(
        &self,
        ctx: &SecurityContext,
        id: SubscriptionId,
        topology_version: i64,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, ConsumerError> {
        let n = self.seek_calls.fetch_add(1, Ordering::SeqCst);
        if n < self.seek_mismatches {
            return Err(ConsumerError::TopologyVersionMismatch {
                detail: "injected".to_owned(),
            });
        }
        self.inner.seek(ctx, id, topology_version, positions).await
    }
    async fn list_topics(&self, ctx: &SecurityContext) -> Result<Vec<Topic>, ConsumerError> {
        self.inner.list_topics(ctx).await
    }
    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        range: PartitionRange,
    ) -> Result<TopicSegment, ConsumerError> {
        self.inner
            .list_topic_segments(ctx, topic, partition, range)
            .await
    }
    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, ConsumerError> {
        self.inner.list_event_types(ctx).await
    }
    async fn get_event_type(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, ConsumerError> {
        self.inner.get_event_type(ctx, id).await
    }
}

// -- handlers, offset stores, listeners for the runtime-behaviour tests -------

pub(super) type SharedAttempts = Arc<Mutex<Vec<u16>>>;
pub(super) type SharedTimeline = Arc<Mutex<Vec<&'static str>>>;
pub(super) type SharedRuntimeEvents = Arc<Mutex<Vec<ConsumerRuntimeEvent>>>;
type SharedPartitionCalls = Arc<Mutex<Vec<(String, u32, i64)>>>;

/// Acks every event, recording each delivered `(topic, partition, offset)`.
pub(super) struct AckAllBatchHandler {
    pub(super) calls: SharedPartitionCalls,
}

#[async_trait]
impl ConsumerHandler for AckAllBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_>,
        _attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        self.calls.lock().unwrap().extend(
            chunk
                .iter()
                .map(|e| (e.topic.to_string(), e.partition, e.offset.as_i64())),
        );
        Ok(chunk
            .last()
            .map(|e| BatchHandlerOutcome::AdvanceThrough { offset: e.offset })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

/// Sleeps `delay` before acking - drives slow-consumer / handler-latency paths.
pub(super) struct SleepingBatchHandler {
    pub(super) calls: SharedPartitionCalls,
    pub(super) delay: Duration,
}

#[async_trait]
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
                .map(|e| (e.topic.to_string(), e.partition, e.offset.as_i64())),
        );
        Ok(chunk
            .last()
            .map(|e| BatchHandlerOutcome::AdvanceThrough { offset: e.offset })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

/// Fails its first `failures_remaining` batches, then records the attempt count.
pub(super) struct FailingThenCommitBatchHandler {
    pub(super) failures_remaining: Arc<Mutex<usize>>,
    pub(super) calls: SharedAttempts,
}

#[async_trait]
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

/// Records a "load"/"handle" timeline across load_position and the handler, for
/// asserting the drain-before-rejoin ordering.
pub(super) struct SequencedOffsetManager {
    pub(super) timeline: SharedTimeline,
}

#[async_trait]
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

#[async_trait]
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

pub(super) struct SequencedBatchHandler {
    pub(super) timeline: SharedTimeline,
}

#[async_trait]
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

#[derive(Clone, Default)]
pub(super) struct RecordingRuntimeListener {
    pub(super) events: SharedRuntimeEvents,
}

#[async_trait]
impl ConsumerRuntimeListener for RecordingRuntimeListener {
    async fn on_consumer_event(&self, event: &ConsumerRuntimeEvent) -> Result<(), ConsumerError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

pub(super) struct FailingRuntimeListener;

#[async_trait]
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

#[async_trait]
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

/// An offset store that records every `commit` as `(group, topic, partition, offset)`.
#[derive(Clone, Default)]
pub(super) struct RecordingCommitOffsetManager {
    pub(super) commits: SharedCommits,
}

#[async_trait]
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

#[async_trait]
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
