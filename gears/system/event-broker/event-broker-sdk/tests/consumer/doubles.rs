//! Test doubles shared by the harness-driven consumer tests.
//!
//! These are client-side doubles only - handlers, offset stores, listeners, and
//! a fault-injecting broker *decorator*. `SeekFaultBroker` wraps a real
//! `Arc<dyn EventBrokerApi>` (the harness broker) and delegates every operation
//! unchanged except the seek/stream faults it injects, so a test can exercise
//! the consumer's topology-mismatch recovery, its recovery from
//! `PositionsNotSet` at open and its skip of an event without a stamped position
//! against the real broker. Nothing here fakes broker behaviour.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;

use event_broker_sdk::models::{
    ConsumerGroup, ConsumerGroupQuery, CreateConsumerGroupRequest, EventType, Page, PartitionRange,
    ResetScope, Subscription, Topic, TopicSegment,
};
use event_broker_sdk::{
    Ack, BatchHandler, BatchHandlerOutcome, CommitOffset, ConsumerError, ConsumerGroupId,
    ConsumerRuntimeEvent, ConsumerRuntimeListener, Delivery, EventBatch, EventBrokerApi, EventV1,
    FrameStream, IngestOutcome, JoinRequest, OffsetManagerError, OffsetStore, Position,
    ProducerCursors, ProducerId, ProducerMode, SeekPosition, SeekResult, Sequence,
    SubscriptionAssignment, SubscriptionId, TopicId, WireFrame,
};
use futures_util::StreamExt;

pub(super) type SharedScopes = Arc<Mutex<Vec<(String, u32, usize)>>>;
pub(super) type SharedViolations = Arc<Mutex<Vec<String>>>;
pub(super) type SharedNamedOffsets = Arc<Mutex<Vec<(&'static str, i64)>>>;
pub(super) type SharedSubjectOffsets = Arc<Mutex<Vec<(String, i64)>>>;
pub(super) type SharedCommits = Arc<Mutex<Vec<(ConsumerGroupId, TopicId, u32, i64)>>>;

/// Records the first event of each batch as `(name, offset)` and returns a
/// fixed outcome - a handler for asserting what the runtime dispatched.
pub(super) struct RecordingBatchHandler {
    pub(super) name: &'static str,
    pub(super) calls: SharedNamedOffsets,
    pub(super) outcome: BatchHandlerOutcome,
}

#[async_trait]
impl BatchHandler<serde_json::Value> for RecordingBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        if let Some(event) = batch.next_event() {
            self.calls.lock().unwrap().push((
                self.name,
                event
                    .sequence()
                    .expect("a delivered event is stamped")
                    .as_i64(),
            ));
        }
        Ok(self.outcome.clone())
    }
}

/// Records the `(topic, partition, len)` scope of every batch and flags any
/// batch that mixed topics or partitions - the invariant the runtime must keep.
/// An event names no topic, so a mixed topic shows as a mixed event type: each
/// topic of the fixture that uses this recorder has one event type.
#[derive(Default)]
pub(super) struct BatchScopeRecorder {
    pub(super) scopes: SharedScopes,
    pub(super) violations: SharedViolations,
}

#[async_trait]
impl BatchHandler<serde_json::Value> for BatchScopeRecorder {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        if let Some(first) = chunk.first() {
            if chunk.iter().any(|event| {
                event.event_type() != first.event_type()
                    || event.partition() != Some(delivery.partition())
            }) {
                self.violations
                    .lock()
                    .unwrap()
                    .push("batch mixed topic IDs or partitions".to_owned());
            }
            self.scopes.lock().unwrap().push((
                delivery.topic().to_string(),
                delivery.partition(),
                chunk.len(),
            ));
        }
        Ok(chunk
            .last()
            .map(|event| BatchHandlerOutcome::AdvanceThrough {
                offset: event.sequence().expect("a delivered event is stamped"),
            })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

/// A broker decorator over the real harness broker that injects seek/stream
/// faults, for exercising the consumer's topology-version-mismatch recovery,
/// its recovery from `PositionsNotSet` at open and its handling of injected frames.
/// Every operation delegates unchanged except the injected ones; the call
/// counters let a test observe the recovery path without racing timing.
pub(super) struct SeekFaultBroker {
    inner: Arc<dyn EventBrokerApi>,
    /// Reject the first `seek_mismatches` seeks with `TopologyVersionMismatch`
    /// before delegating (`usize::MAX` = always).
    seek_mismatches: usize,
    /// Reject the first stream open with `PositionsNotSet`.
    stream_positions_not_set_once: bool,
    /// Items the first opened stream yields ahead of the real broker's.
    stream_prefix: Mutex<Vec<Result<WireFrame, ConsumerError>>>,
    /// Items the first opened stream yields right after the real broker's first
    /// frame, so the break comes on a stream that is already open.
    stream_break: Mutex<Vec<Result<WireFrame, ConsumerError>>>,
    /// End the first opened stream after the real broker's first frame and its
    /// break items instead of going on to the real broker's later frames.
    end_first_stream: AtomicBool,
    /// Answer the first `stream` call with this fault instead of opening the real
    /// broker's stream, so the member never streams.
    open_fault: Mutex<Option<OpenFault>>,
    pub(super) seek_calls: Arc<AtomicUsize>,
    pub(super) get_subscription_calls: Arc<AtomicUsize>,
    pub(super) stream_calls: Arc<AtomicUsize>,
    /// Every subscription `leave` was called with, in order.
    pub(super) leave_calls: Arc<Mutex<Vec<SubscriptionId>>>,
}

/// How the first `stream` call fails without opening the real broker's stream.
enum OpenFault {
    /// `stream` itself returns the error, as an in-process open does.
    Failure(ConsumerError),
    /// `stream` returns a stream whose first item is the error, as a transport that
    /// opens lazily (REST) reports a refused open.
    Error(ConsumerError),
    /// `stream` returns a stream that ends before its first frame.
    End,
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
            stream_prefix: Mutex::new(Vec::new()),
            stream_break: Mutex::new(Vec::new()),
            end_first_stream: AtomicBool::new(false),
            open_fault: Mutex::new(None),
            seek_calls: Arc::new(AtomicUsize::new(0)),
            get_subscription_calls: Arc::new(AtomicUsize::new(0)),
            stream_calls: Arc::new(AtomicUsize::new(0)),
            leave_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Makes the first opened stream yield `frames` before the real broker's.
    pub(super) fn with_stream_prefix(self, frames: Vec<WireFrame>) -> Self {
        Self {
            stream_prefix: Mutex::new(frames.into_iter().map(Ok).collect()),
            ..self
        }
    }

    /// Makes the first opened stream yield `error` right after the real broker's
    /// first frame, then go on to its later frames.
    pub(super) fn with_stream_error(self, error: ConsumerError) -> Self {
        Self {
            stream_break: Mutex::new(vec![Err(error)]),
            ..self
        }
    }

    /// Makes the first opened stream end right after the real broker's first frame.
    /// The real broker's stream stays open, unpolled after that frame, until the
    /// consumer drops the ended one, as a transport keeps its session until the
    /// client lets go of the response.
    pub(super) fn with_stream_end(self) -> Self {
        Self {
            end_first_stream: AtomicBool::new(true),
            ..self
        }
    }

    /// Makes the first `stream` call return `error` without opening the real
    /// broker's stream.
    pub(super) fn with_open_failure(self, error: ConsumerError) -> Self {
        Self {
            open_fault: Mutex::new(Some(OpenFault::Failure(error))),
            ..self
        }
    }

    /// Makes the first `stream` call return a stream whose first item is `error`,
    /// without opening the real broker's stream.
    pub(super) fn with_open_error(self, error: ConsumerError) -> Self {
        Self {
            open_fault: Mutex::new(Some(OpenFault::Error(error))),
            ..self
        }
    }

    /// Makes the first `stream` call return a stream that ends before its first
    /// frame, without opening the real broker's stream.
    pub(super) fn with_open_end(self) -> Self {
        Self {
            open_fault: Mutex::new(Some(OpenFault::End)),
            ..self
        }
    }
}

/// The real broker's stream with a break injected after its first frame: the
/// `stream_break` items, then the end if `end` is set, else the real later frames.
/// The real stream lives as long as this one, so an ended stream keeps the real
/// session open until the consumer drops it.
struct BreakAfterFirstFrame {
    real: FrameStream,
    first_seen: bool,
    break_items: std::vec::IntoIter<Result<WireFrame, ConsumerError>>,
    end: bool,
}

impl futures_util::Stream for BreakAfterFirstFrame {
    type Item = Result<WireFrame, ConsumerError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if !self.first_seen {
            let first = std::task::ready!(self.real.poll_next_unpin(cx));
            self.first_seen = true;
            return std::task::Poll::Ready(first);
        }
        if let Some(item) = self.break_items.next() {
            return std::task::Poll::Ready(Some(item));
        }
        if self.end {
            return std::task::Poll::Ready(None);
        }
        self.real.poll_next_unpin(cx)
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
        event: &EventV1,
        ack: Ack,
    ) -> Result<IngestOutcome, ConsumerError> {
        self.inner.publish(ctx, event, ack).await
    }
    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        events: &[EventV1],
        ack: Ack,
    ) -> Result<IngestOutcome, ConsumerError> {
        self.inner.publish_batch(ctx, events, ack).await
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
        self.leave_calls.lock().unwrap().push(id);
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
        let open_fault = self.open_fault.lock().unwrap().take();
        match open_fault {
            Some(OpenFault::Failure(error)) => return Err(error),
            Some(OpenFault::Error(error)) => {
                return Ok(Box::pin(futures_util::stream::iter([Err(error)])));
            }
            Some(OpenFault::End) => return Ok(Box::pin(futures_util::stream::empty())),
            None => {}
        }
        let stream = self.inner.stream(ctx, id).await?;
        let prefix = std::mem::take(&mut *self.stream_prefix.lock().unwrap());
        let break_items = std::mem::take(&mut *self.stream_break.lock().unwrap());
        let end = self.end_first_stream.swap(false, Ordering::SeqCst);
        if break_items.is_empty() && !end {
            return Ok(Box::pin(futures_util::stream::iter(prefix).chain(stream)));
        }
        Ok(Box::pin(futures_util::stream::iter(prefix).chain(
            BreakAfterFirstFrame {
                real: stream,
                first_seen: false,
                break_items: break_items.into_iter(),
                end,
            },
        )))
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
impl BatchHandler<serde_json::Value> for AckAllBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        self.calls.lock().unwrap().extend(chunk.iter().map(|e| {
            (
                delivery.topic().to_string(),
                delivery.partition(),
                e.sequence().expect("a delivered event is stamped").as_i64(),
            )
        }));
        Ok(chunk
            .last()
            .map(|e| BatchHandlerOutcome::AdvanceThrough {
                offset: e.sequence().expect("a delivered event is stamped"),
            })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

/// Sleeps `delay` before acking - drives slow-consumer / handler-latency paths.
pub(super) struct SleepingBatchHandler {
    pub(super) calls: SharedPartitionCalls,
    pub(super) delay: Duration,
}

#[async_trait]
impl BatchHandler<serde_json::Value> for SleepingBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        tokio::time::sleep(self.delay).await;
        let chunk = batch.next_chunk(batch.len());
        self.calls.lock().unwrap().extend(chunk.iter().map(|e| {
            (
                delivery.topic().to_string(),
                delivery.partition(),
                e.sequence().expect("a delivered event is stamped").as_i64(),
            )
        }));
        Ok(chunk
            .last()
            .map(|e| BatchHandlerOutcome::AdvanceThrough {
                offset: e.sequence().expect("a delivered event is stamped"),
            })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

/// Fails its first `failures_remaining` batches, then records the attempt count.
pub(super) struct FailingThenCommitBatchHandler {
    pub(super) failures_remaining: Arc<Mutex<usize>>,
    pub(super) calls: SharedAttempts,
}

#[async_trait]
impl BatchHandler<serde_json::Value> for FailingThenCommitBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_, serde_json::Value>,
        delivery: &Delivery,
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
        self.calls.lock().unwrap().push(delivery.attempts());
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
impl BatchHandler<serde_json::Value> for SequencedBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
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
    EventSkipped,
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
        ConsumerRuntimeEvent::EventSkipped { .. } => RuntimeEventKind::EventSkipped,
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

/// Records each `WARN` event a test's thread emits as one
/// `WARN <message> <field>=<value>..` line, while it lives.
///
/// Capture is one global subscriber per test binary, routed by thread, never a scoped
/// `set_default`: while one dispatcher is registered, tracing-core asks the default
/// of whichever thread first hits a call site for its interest (`Rebuilder::JustOne`,
/// tracing-core 0.1.36 `callsite.rs:544-575`), so another test's thread with no
/// default caches `Interest::never` there and this test's line is never seen. A test's
/// consumer tasks run on its current-thread runtime, so routing by thread keeps
/// other tests' lines out.
pub(super) struct WarnCapture {
    lines: SharedWarnLines,
}

type SharedWarnLines = Arc<Mutex<Vec<String>>>;

thread_local! {
    static THREAD_CAPTURE: RefCell<Option<SharedWarnLines>> = const { RefCell::new(None) };
}

impl WarnCapture {
    /// Installs the global subscriber on first use and routes this thread's `WARN`
    /// events to the returned capture until it is dropped.
    pub(super) fn install() -> Self {
        static INSTALLED: OnceLock<()> = OnceLock::new();
        INSTALLED.get_or_init(|| {
            tracing::subscriber::set_global_default(WarnRouter)
                .expect("no other global subscriber in the consumer test binary");
            // A call site another test hit before the install cached the no-op
            // default's interest; this re-asks every call site.
            tracing::callsite::rebuild_interest_cache();
        });
        let lines = Arc::new(Mutex::new(Vec::new()));
        THREAD_CAPTURE.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&lines)));
        Self { lines }
    }

    pub(super) fn lines(&self) -> SharedWarnLines {
        Arc::clone(&self.lines)
    }
}

impl Drop for WarnCapture {
    fn drop(&mut self) {
        THREAD_CAPTURE.with(|slot| slot.borrow_mut().take());
    }
}

/// The global subscriber behind `WarnCapture`: admits `WARN` only and appends each
/// line to the capture of the thread that emitted it, if any.
struct WarnRouter;

impl tracing::Subscriber for WarnRouter {
    fn register_callsite(&self, metadata: &'static tracing::Metadata<'static>) -> Interest {
        if *metadata.level() == tracing::Level::WARN {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::WARN)
    }

    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::WARN
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        // `try_with`: an event during thread teardown has no capture to reach.
        let capture = THREAD_CAPTURE
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten();
        if let Some(lines) = capture {
            let mut line = WarnLine::default();
            event.record(&mut line);
            lines
                .lock()
                .unwrap()
                .push(format!("WARN {}{}", line.message, line.fields));
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[derive(Default)]
struct WarnLine {
    message: String,
    fields: String,
}

impl tracing::field::Visit for WarnLine {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields
                .push_str(&format!(" {}={value:?}", field.name()));
        }
    }
}
