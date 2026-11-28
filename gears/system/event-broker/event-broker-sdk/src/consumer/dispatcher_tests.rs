use crate::sequence::Sequence;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use toolkit_gts::gts_id;

use tokio::sync::RwLock;
use uuid::Uuid;

use super::builder::{ConsumerRoute, ConsumerRouteHandlerKind};
#[cfg(feature = "db")]
use super::dispatcher::TxSlotDispatcher;
use super::dispatcher::{
    Buffered, PartitionSlowState, SlotDispatcher, SlotSettings, SlowConsumerReason,
    TopicPartitionKey, enqueue_partition_batch,
};
use super::runtime::RoutedBatchHandler;
use super::type_cache::ConsumerTypeCache;
#[cfg(feature = "db")]
use super::types::DeliverTxBatch;
use super::types::{BatchHandlerAdapter, DeliverBatch, EventHandlerAdapter};
use super::{
    BatchHandler, BatchHandlerOutcome, ConsumerRuntimeEvent, ConsumerRuntimeListener, Delivery,
    EventBatch, EventHandler, HandlerOutcome, PartitionProgress,
};
#[cfg(feature = "db")]
use super::{CommitOffsetInTx, OffsetStore, Position, TxCommitHandle};
use super::{ConsumerCommitMode, Fallback, InMemoryOffsetManager};
use crate::api::{
    AssignedPartition, BarrierMode, EventBrokerApi, FrameStream, InterestTenant, JoinRequest,
    OwnershipFilter, PartitionPosition, SeekPosition, SeekResult, SubscriptionAssignment,
    UnmatchedTypes, WireFrame,
};
use crate::broker_stub::BrokerStub;
use crate::error::ConsumerError;
use crate::error::EventBrokerError;
#[cfg(feature = "db")]
use crate::error::OffsetManagerError;
use crate::gts::{EventV1, StoredPosition};
use crate::ids::{ConsumerGroupId, SubscriptionId};
use crate::models::EventType;
use crate::warn_capture::WarnCapture;
use gts::{GtsIdPattern, GtsInstanceId, GtsTypeId};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;

/// A GTS type id used for the `subject_type` of fabricated events (never asserted).
const SUBJECT_TYPE: &str = "gts.x.eb.test.subject.v1~";

fn gtype(id: &str) -> GtsTypeId {
    GtsTypeId::new(id)
}

fn ginst(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("valid GTS instance id")
}

fn gpat(id: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(id).expect("valid GTS pattern")
}

// Valid GTS ids standing in for the routing tests' former short labels.
const ORDERS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.route.orders.v1");
const PAYMENTS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.route.payments.v1");
const ORDER_CREATED: &str = gts_id!("cf.core.events.event.v1~example.mock.route.order_created.v1~");
const ORDER_UPDATED: &str = gts_id!("cf.core.events.event.v1~example.mock.route.order_updated.v1~");
const ORDER_CANCELLED: &str =
    gts_id!("cf.core.events.event.v1~example.mock.route.order_cancelled.v1~");
const ORDER_CLOSED: &str = gts_id!("cf.core.events.event.v1~example.mock.route.order_closed.v1~");
const BUFFER_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.route.buffer_event.v1~");

type SharedOffsets = Arc<Mutex<Vec<i64>>>;
type SharedNamedOffsets = Arc<Mutex<Vec<(&'static str, i64)>>>;
type PartitionCall = (String, u32, i64);
type SharedPartitionCalls = Arc<Mutex<Vec<PartitionCall>>>;
type TestPartitionBuffers =
    Arc<RwLock<HashMap<TopicPartitionKey, super::dispatcher::PartitionEventBuffer>>>;

fn raw_event(type_id: &str, offset: i64) -> EventV1 {
    raw_event_on(type_id, 3, offset)
}

fn position_on(partition: u32, offset: i64) -> StoredPosition {
    StoredPosition {
        partition,
        sequence: Sequence::assigned(offset),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    }
}

fn raw_event_on(type_id: &str, partition: u32, offset: i64) -> EventV1 {
    EventV1::builder_for(gtype(type_id), serde_json::json!({ "offset": offset }))
        .id(Uuid::from_u128(u128::try_from(offset).unwrap()))
        .subject_tenant(Uuid::nil())
        .subject(format!("event-{offset}"))
        .subject_type_of(gtype(SUBJECT_TYPE))
        .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
        .build()
        .unwrap()
        .stamp_position(position_on(partition, offset))
}

fn positions_of(events: &[EventV1]) -> Vec<StoredPosition> {
    events
        .iter()
        .map(|event| StoredPosition {
            partition: event.partition().expect("a delivered event is stamped"),
            sequence: event.sequence().expect("a delivered event is stamped"),
            sequence_time: event.sequence_time().expect("a delivered event is stamped"),
        })
        .collect()
}

/// The delivery context of a batch read from `topic` partition 3, on its first call.
fn delivery(topic: &str) -> Delivery {
    Delivery::new(ginst(topic), 3, 1)
}

fn sequence_of(event: &EventV1) -> i64 {
    event
        .sequence()
        .expect("a delivered event is stamped")
        .as_i64()
}

fn batch_handler(handler: impl BatchHandler<serde_json::Value> + 'static) -> Arc<dyn DeliverBatch> {
    Arc::new(BatchHandlerAdapter::new(handler))
}

struct RecordingSingleHandler {
    calls: SharedOffsets,
    outcome: HandlerOutcome,
}

#[async_trait::async_trait]
impl EventHandler<serde_json::Value> for RecordingSingleHandler {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push(sequence_of(&event));
        Ok(self.outcome.clone())
    }
}

struct RecordingBatchHandler {
    name: &'static str,
    calls: SharedNamedOffsets,
    outcome: BatchHandlerOutcome,
}

#[async_trait::async_trait]
impl BatchHandler<serde_json::Value> for RecordingBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        if let Some(event) = batch.next_event() {
            self.calls
                .lock()
                .unwrap()
                .push((self.name, sequence_of(event)));
        }
        Ok(self.outcome.clone())
    }
}

struct AckAllBatchHandler {
    calls: SharedPartitionCalls,
}

#[async_trait::async_trait]
impl BatchHandler<serde_json::Value> for AckAllBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        self.calls.lock().unwrap().extend(chunk.iter().map(|event| {
            (
                delivery.topic().as_ref().to_owned(),
                delivery.partition(),
                sequence_of(event),
            )
        }));
        Ok(chunk.last().map_or(BatchHandlerOutcome::Success, |event| {
            BatchHandlerOutcome::AdvanceThrough {
                offset: event.sequence().expect("a delivered event is stamped"),
            }
        }))
    }
}

#[test]
fn slow_state_detects_buffer_high_watermark_once() {
    let mut state = PartitionSlowState::default();

    assert!(
        state
            .observe_enqueue(3, Sequence::assigned(10), 4)
            .is_none()
    );
    let signal = state
        .observe_enqueue(4, Sequence::assigned(11), 4)
        .expect("high watermark triggers");

    assert_eq!(signal.reason, SlowConsumerReason::BufferHighWatermark);
    assert_eq!(signal.buffered_count, 4);
    assert_eq!(signal.latest_observed_offset, Some(Sequence::assigned(11)));
    assert!(
        state
            .observe_enqueue(5, Sequence::assigned(12), 4)
            .is_none()
    );
}

#[test]
fn slow_state_detects_handler_latency_strikes_and_resets_on_fast_completion() {
    let mut state = PartitionSlowState::default();
    let threshold = Duration::from_millis(50);

    assert!(
        state
            .observe_handler_completion(
                Duration::from_millis(75),
                threshold,
                2,
                Sequence::assigned(20)
            )
            .is_none()
    );
    assert!(
        state
            .observe_handler_completion(
                Duration::from_millis(10),
                threshold,
                2,
                Sequence::assigned(21)
            )
            .is_none()
    );

    assert!(
        state
            .observe_handler_completion(
                Duration::from_millis(75),
                threshold,
                2,
                Sequence::assigned(22)
            )
            .is_none()
    );
    let signal = state
        .observe_handler_completion(
            Duration::from_millis(80),
            threshold,
            2,
            Sequence::assigned(23),
        )
        .expect("latency strikes trigger");

    assert_eq!(signal.reason, SlowConsumerReason::HandlerLatencyStrikes);
    assert_eq!(signal.consecutive_slow_handlers, 2);
    assert_eq!(signal.last_delivered_offset, Some(Sequence::assigned(23)));
}

#[tokio::test]
async fn partition_buffer_capacity_is_isolated_per_topic_partition() {
    let buffers: TestPartitionBuffers = Arc::new(RwLock::new(HashMap::new()));
    let topic = gts_id!("cf.core.events.topic.v1~example.dispatcher.buffer.x.v1");
    let partition_zero = TopicPartitionKey::new(ginst(topic), 0);
    let partition_one = TopicPartitionKey::new(ginst(topic), 1);

    let first_partition_zero = enqueue_partition_batch(
        &buffers,
        partition_zero.clone(),
        Buffered {
            event: raw_event_on(BUFFER_EVENT, 0, 10),
            at: position_on(0, 10),
        },
        1,
    )
    .await
    .expect("first event in partition 0 fits");
    assert_eq!(first_partition_zero.buffered_count, 1);

    let overflow_partition_zero = enqueue_partition_batch(
        &buffers,
        partition_zero,
        Buffered {
            event: raw_event_on(BUFFER_EVENT, 0, 11),
            at: position_on(0, 11),
        },
        1,
    )
    .await
    .expect_err("partition 0 capacity is exhausted");
    assert!(
        overflow_partition_zero
            .to_string()
            .contains("partition buffer capacity 1 exceeded"),
        "unexpected overflow error: {overflow_partition_zero}"
    );

    let first_partition_one = enqueue_partition_batch(
        &buffers,
        partition_one,
        Buffered {
            event: raw_event_on(BUFFER_EVENT, 1, 20),
            at: position_on(1, 20),
        },
        1,
    )
    .await
    .expect("partition 1 has independent capacity");
    assert_eq!(first_partition_one.buffered_count, 1);
}

fn route(topic: &str, event_type: Option<&str>) -> ConsumerRoute {
    ConsumerRoute {
        topic: ginst(topic),
        event_type: event_type.map(gpat),
        handler_kind: ConsumerRouteHandlerKind::Batch,
    }
}

#[tokio::test]
async fn single_handler_adapter_dispatches_one_event_batch() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = EventHandlerAdapter::new(RecordingSingleHandler {
        calls: calls.clone(),
        outcome: HandlerOutcome::Success,
    });
    let events = vec![
        raw_event_on(ORDER_CREATED, 7, 20),
        raw_event_on(ORDER_UPDATED, 7, 21),
    ];
    let batch = EventBatch::new(&events);

    let outcome = handler
        .deliver(
            &batch,
            &positions_of(&events),
            &Delivery::new(ginst(ORDERS_TOPIC), 7, 1),
        )
        .await
        .unwrap()
        .outcome;

    assert_eq!(*calls.lock().unwrap(), vec![20]);
    assert!(matches!(
        outcome,
        BatchHandlerOutcome::AdvanceThrough { offset } if offset == Sequence::assigned(20)
    ));
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(20))
    );
}

#[tokio::test]
async fn native_batch_handler_dispatches_multiple_events_from_one_partition() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = AckAllBatchHandler {
        calls: calls.clone(),
    };
    let events = vec![
        raw_event_on(ORDER_CREATED, 5, 30),
        raw_event_on(ORDER_UPDATED, 5, 31),
        raw_event_on(ORDER_CLOSED, 5, 32),
    ];
    let batch = EventBatch::new(&events);

    let outcome = handler
        .handle_batch(&batch, &Delivery::new(ginst(ORDERS_TOPIC), 5, 1))
        .await
        .unwrap();

    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            (ORDERS_TOPIC.to_owned(), 5, 30),
            (ORDERS_TOPIC.to_owned(), 5, 31),
            (ORDERS_TOPIC.to_owned(), 5, 32),
        ]
    );
    assert!(matches!(
        outcome,
        BatchHandlerOutcome::AdvanceThrough { offset } if offset == Sequence::assigned(32)
    ));
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(30))
    );
}

#[tokio::test]
async fn routed_dispatch_prefers_exact_topic_and_event_type_route() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let routed = RoutedBatchHandler::new(
        Some(batch_handler(RecordingBatchHandler {
            name: "default",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })),
        vec![route(ORDERS_TOPIC, Some(ORDER_CREATED))],
        vec![batch_handler(RecordingBatchHandler {
            name: "orders-created",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })],
    )
    .unwrap();
    let events = vec![raw_event(ORDER_CREATED, 10)];
    let batch = EventBatch::new(&events);

    let outcome = routed
        .deliver(&batch, &positions_of(&events), &delivery(ORDERS_TOPIC))
        .await
        .unwrap()
        .outcome;

    assert_eq!(*calls.lock().unwrap(), vec![("orders-created", 10)]);
    assert!(matches!(outcome, BatchHandlerOutcome::Success));
}

#[tokio::test]
async fn routed_dispatch_uses_topic_catch_all_before_default_handler() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let routed = RoutedBatchHandler::new(
        Some(batch_handler(RecordingBatchHandler {
            name: "default",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })),
        vec![route(ORDERS_TOPIC, None)],
        vec![batch_handler(RecordingBatchHandler {
            name: "orders-any",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })],
    )
    .unwrap();
    let events = vec![raw_event(ORDER_CANCELLED, 11)];
    let batch = EventBatch::new(&events);

    let outcome = routed
        .deliver(&batch, &positions_of(&events), &delivery(ORDERS_TOPIC))
        .await
        .unwrap()
        .outcome;

    assert_eq!(*calls.lock().unwrap(), vec![("orders-any", 11)]);
    assert!(matches!(outcome, BatchHandlerOutcome::Success));
}

#[tokio::test]
async fn routed_dispatch_falls_back_to_default_handler() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let routed = RoutedBatchHandler::new(
        Some(batch_handler(RecordingBatchHandler {
            name: "default",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })),
        vec![route(PAYMENTS_TOPIC, None)],
        vec![batch_handler(RecordingBatchHandler {
            name: "payments-any",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })],
    )
    .unwrap();
    let events = vec![raw_event(ORDER_CANCELLED, 12)];
    let batch = EventBatch::new(&events);

    let outcome = routed
        .deliver(&batch, &positions_of(&events), &delivery(ORDERS_TOPIC))
        .await
        .unwrap()
        .outcome;

    assert_eq!(*calls.lock().unwrap(), vec![("default", 12)]);
    assert!(matches!(outcome, BatchHandlerOutcome::Success));
}

#[tokio::test]
async fn routed_dispatch_fails_visibly_without_matching_route_or_default() {
    let routed = RoutedBatchHandler::new(
        None,
        vec![route(PAYMENTS_TOPIC, None)],
        vec![batch_handler(RecordingBatchHandler {
            name: "payments-any",
            calls: Arc::new(Mutex::new(Vec::new())),
            outcome: BatchHandlerOutcome::Success,
        })],
    )
    .unwrap();
    let events = vec![raw_event(ORDER_CANCELLED, 13)];
    let batch = EventBatch::new(&events);

    let err = routed
        .deliver(&batch, &positions_of(&events), &delivery(ORDERS_TOPIC))
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("no consumer route matched"),
        "unexpected error: {err:?}"
    );
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(13))
    );
}

#[tokio::test]
async fn routed_dispatch_preserves_adjacent_event_order_across_routes() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let routed = RoutedBatchHandler::new(
        None,
        vec![
            route(ORDERS_TOPIC, Some(ORDER_CREATED)),
            route(ORDERS_TOPIC, Some(ORDER_CANCELLED)),
        ],
        vec![
            batch_handler(RecordingBatchHandler {
                name: "created",
                calls: calls.clone(),
                outcome: BatchHandlerOutcome::Success,
            }),
            batch_handler(RecordingBatchHandler {
                name: "cancelled",
                calls: calls.clone(),
                outcome: BatchHandlerOutcome::Success,
            }),
        ],
    )
    .unwrap();
    let events = vec![
        raw_event_on(ORDER_CREATED, 3, 40),
        raw_event_on(ORDER_CANCELLED, 3, 41),
    ];
    let batch = EventBatch::new(&events);
    let second_batch = EventBatch::new(&events[1..]);

    routed
        .deliver(&batch, &positions_of(&events), &delivery(ORDERS_TOPIC))
        .await
        .unwrap();
    routed
        .deliver(
            &second_batch,
            &positions_of(&events[1..]),
            &delivery(ORDERS_TOPIC),
        )
        .await
        .unwrap();

    assert_eq!(
        *calls.lock().unwrap(),
        vec![("created", 40), ("cancelled", 41)]
    );
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(40))
    );
    assert_eq!(
        second_batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(41))
    );
}

#[tokio::test]
async fn routed_dispatch_retry_does_not_advance_past_earlier_unprocessed_event() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let routed = RoutedBatchHandler::new(
        None,
        vec![
            route(ORDERS_TOPIC, Some(ORDER_CREATED)),
            route(ORDERS_TOPIC, Some(ORDER_CANCELLED)),
        ],
        vec![
            batch_handler(RecordingBatchHandler {
                name: "created",
                calls: calls.clone(),
                outcome: BatchHandlerOutcome::Retry {
                    reason: "try later".to_owned(),
                },
            }),
            batch_handler(RecordingBatchHandler {
                name: "cancelled",
                calls: calls.clone(),
                outcome: BatchHandlerOutcome::Success,
            }),
        ],
    )
    .unwrap();
    let events = vec![
        raw_event_on(ORDER_CREATED, 3, 50),
        raw_event_on(ORDER_CANCELLED, 3, 51),
    ];
    let batch = EventBatch::new(&events);

    let outcome = routed
        .deliver(&batch, &positions_of(&events), &delivery(ORDERS_TOPIC))
        .await
        .unwrap()
        .outcome;

    assert!(matches!(outcome, BatchHandlerOutcome::Retry { .. }));
    assert_eq!(*calls.lock().unwrap(), vec![("created", 50)]);
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(50))
    );
}

/// How long a slot run may take to end. The slot tests below sleep at most 750 ms
/// of re-JOIN backoff in all, so this bound is far above that.
const BOUND: Duration = Duration::from_secs(5);

/// Where a broker injects its stream failure.
#[derive(Clone, Copy)]
enum FailsAt {
    /// `stream()` itself returns the error.
    Open,
    /// `stream()` succeeds and the stream's first item is the error, as a lazily
    /// opened transport (REST) reports a refused open.
    FirstItem,
}

/// A broker whose every stream fails before its first frame. Each JOIN hands out
/// the next subscription id with nothing assigned, so no seek is made. The failure
/// is a transport error at the first item unless built with `failing_with`.
struct OpenFailsBeforeFirstFrameBroker {
    joins: Mutex<u128>,
    leave_calls: Mutex<Vec<SubscriptionId>>,
    /// Cancelled as each stream's first item is taken. A run handed another token
    /// never sees it.
    cancel_at_first_item: CancellationToken,
    /// Builds the injected error, afresh for each stream.
    error: fn() -> EventBrokerError,
    fails_at: FailsAt,
}

impl Default for OpenFailsBeforeFirstFrameBroker {
    fn default() -> Self {
        Self::failing_with(
            || EventBrokerError::Transport("injected".to_owned()),
            FailsAt::FirstItem,
        )
    }
}

/// The `PositionsNotSet` the transports build for a refused open.
fn positions_not_set() -> EventBrokerError {
    EventBrokerError::PositionsNotSet {
        unseeded: Vec::new(),
        detail: "injected".to_owned(),
    }
}

impl OpenFailsBeforeFirstFrameBroker {
    /// A broker whose every stream fails with `error`, at `fails_at`.
    fn failing_with(error: fn() -> EventBrokerError, fails_at: FailsAt) -> Self {
        Self {
            joins: Mutex::new(0),
            leave_calls: Mutex::new(Vec::new()),
            cancel_at_first_item: CancellationToken::new(),
            error,
            fails_at,
        }
    }

    /// A broker that cancels `cancel` while the run awaits a stream's first item.
    fn cancelling(cancel: CancellationToken) -> Self {
        Self {
            cancel_at_first_item: cancel,
            ..Self::default()
        }
    }
}

/// The id of the `n`th JOIN's subscription, counting from 1.
fn joined_subscription(n: u128) -> SubscriptionId {
    SubscriptionId(Uuid::from_u128(0x1800 + n))
}

#[async_trait::async_trait]
impl BrokerStub for OpenFailsBeforeFirstFrameBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let mut joins = self.joins.lock().unwrap();
        *joins += 1;
        Ok(SubscriptionAssignment {
            id: joined_subscription(*joins),
            topology_version: 1,
            assigned: Vec::new(),
            unmatched_types: Vec::new(),
        })
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        self.leave_calls.lock().unwrap().push(id);
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        let cancel = self.cancel_at_first_item.clone();
        let error = self.error;
        match self.fails_at {
            FailsAt::Open => Err(error()),
            FailsAt::FirstItem => Ok(Box::pin(futures_util::stream::once(async move {
                cancel.cancel();
                Err(error())
            }))),
        }
    }
}

/// A broker whose every stream yields one heartbeat and then ends, so each stream
/// opens and each end re-JOINs. Each JOIN hands out the next subscription id with
/// nothing assigned, so no seek is made; the `cancel_at_join`th JOIN cancels the run.
struct OpensEveryTimeBroker {
    joins: Mutex<u128>,
    leave_calls: Mutex<Vec<SubscriptionId>>,
    cancel_at_join: u128,
    cancel: CancellationToken,
}

impl OpensEveryTimeBroker {
    fn cancelling_at_join(cancel_at_join: u128, cancel: CancellationToken) -> Self {
        Self {
            joins: Mutex::new(0),
            leave_calls: Mutex::new(Vec::new()),
            cancel_at_join,
            cancel,
        }
    }
}

#[async_trait::async_trait]
impl BrokerStub for OpensEveryTimeBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let mut joins = self.joins.lock().unwrap();
        *joins += 1;
        if *joins == self.cancel_at_join {
            self.cancel.cancel();
        }
        Ok(SubscriptionAssignment {
            id: joined_subscription(*joins),
            topology_version: 1,
            assigned: Vec::new(),
            unmatched_types: Vec::new(),
        })
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        self.leave_calls.lock().unwrap().push(id);
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Ok(Box::pin(futures_util::stream::iter([Ok(
            WireFrame::Heartbeat {
                at: "2026-11-07T00:00:00Z".to_owned(),
            },
        )])))
    }
}

/// The settings of a slot on `broker` with `max_rejoin_attempts: 2`, joining a fixed group
/// in a fixed tenant.
fn slot_settings(broker: Arc<dyn EventBrokerApi>) -> SlotSettings {
    SlotSettings {
        slot_idx: 0,
        broker,
        type_cache: Arc::new(ConsumerTypeCache::default()),
        group: ConsumerGroupId(Uuid::from_u128(0x1801)),
        topics: vec![ginst(ORDERS_TOPIC)],
        subscription_interests: Vec::new(),
        subject_tenant: InterestTenant::Tenant(Uuid::from_u128(0x1926)),
        ownership_filter: OwnershipFilter::default(),
        barrier_mode: BarrierMode::default(),
        event_type_patterns: Vec::new(),
        subject_types: Vec::new(),
        client_agent: "budget-test".to_owned(),
        session_timeout: None,
        filter: None,
        heartbeat_drop_threshold: 3,
        retry_base: Duration::from_millis(10),
        retry_max: Duration::from_millis(10),
        partition_buffer_capacity: 16,
        buffer_high_watermark: 12,
        buffer_low_watermark: 4,
        batch_max_events: 1,
        handler_latency: Duration::from_secs(1),
        handler_strikes: 3,
        listeners: Vec::new(),
        listener_timeout: Duration::from_secs(1),
        max_rejoin_attempts: 2,
        subscription_id: Arc::new(tokio::sync::Mutex::new(None)),
    }
}

/// A slot on `broker` with `max_rejoin_attempts: 2`, joining a fixed group.
fn slot_dispatcher(
    broker: Arc<dyn EventBrokerApi>,
) -> SlotDispatcher<BatchHandlerAdapter<AckAllBatchHandler>, InMemoryOffsetManager> {
    SlotDispatcher {
        settings: slot_settings(broker),
        offset_manager: Arc::new(InMemoryOffsetManager::new(Fallback::Earliest)),
        handler: Arc::new(BatchHandlerAdapter::new(AckAllBatchHandler {
            calls: SharedPartitionCalls::default(),
        })),
        commit_mode: ConsumerCommitMode::Manual,
    }
}

/// A stream that fails before its first frame never opened, so it does not reset
/// the re-JOIN budget: with `max_rejoin_attempts: 2` the third failed open ends the
/// run. Each failed open leaves its subscription first.
#[tokio::test]
async fn a_stream_that_fails_before_its_first_frame_every_time_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::default());
    let dispatcher = slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(
            Arc::new(SecurityContext::anonymous()),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the run ends within the bound");

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (3, "max re-JOIN attempts exceeded")
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![
            joined_subscription(1),
            joined_subscription(2),
            joined_subscription(3),
        ]
    );
}

/// `PositionsNotSet` from `stream()` on every attempt is a failed open: it spends the
/// re-JOIN budget like any other, so the run ends at the third failure.
#[tokio::test]
async fn positions_not_set_on_every_open_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::failing_with(
        positions_not_set,
        FailsAt::Open,
    ));
    let dispatcher = slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(
            Arc::new(SecurityContext::anonymous()),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the run ends within the bound");

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (3, "max re-JOIN attempts exceeded")
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![
            joined_subscription(1),
            joined_subscription(2),
            joined_subscription(3),
        ]
    );
}

/// `PositionsNotSet` as the first item of every stream is a failed open too, and
/// ends the run at the third failure.
#[tokio::test]
async fn positions_not_set_as_every_first_item_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::failing_with(
        positions_not_set,
        FailsAt::FirstItem,
    ));
    let dispatcher = slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(
            Arc::new(SecurityContext::anonymous()),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the run ends within the bound");

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (3, "max re-JOIN attempts exceeded")
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![
            joined_subscription(1),
            joined_subscription(2),
            joined_subscription(3),
        ]
    );
}

/// A run cancelled while it awaits a stream's first item stops there: it does not
/// re-JOIN on that failed open, and leaves its one subscription on the way out.
#[tokio::test]
async fn a_run_cancelled_while_awaiting_the_first_frame_ends_without_a_rejoin() {
    let cancel = CancellationToken::new();
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::cancelling(cancel.clone()));
    let dispatcher = slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel),
    )
    .await
    .expect("the run ends within the bound");

    let Ok(()) = result else {
        panic!("expected the cancelled run to end cleanly, got {result:?}");
    };
    assert_eq!(*broker.joins.lock().unwrap(), 1);
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(1)]
    );
}

/// Only an open stream starts the re-JOIN budget over, and every stream here opens,
/// so with `max_rejoin_attempts: 2` the run outlives three re-JOINs in a row. The
/// stub cancels it at its fourth JOIN; the run then ends at the top of its loop
/// without opening a fifth stream, so the count is exact.
#[tokio::test]
async fn a_stream_that_opens_every_time_never_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let cancel = CancellationToken::new();
    let broker = Arc::new(OpensEveryTimeBroker::cancelling_at_join(4, cancel.clone()));
    let dispatcher = slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel),
    )
    .await
    .expect("the run ends within the bound");

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    assert_eq!(*broker.joins.lock().unwrap(), 4);
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(4)]
    );
}

/// A broker that answers every JOIN with `unmatched_types` and nothing assigned,
/// and whose every stream yields one heartbeat and ends, so each stream end
/// re-JOINs. The `cancel_at_join`th JOIN cancels the run.
struct UnmatchedTypesBroker {
    joins: Mutex<u128>,
    cancel_at_join: u128,
    cancel: CancellationToken,
    unmatched_types: Vec<UnmatchedTypes>,
}

#[async_trait::async_trait]
impl BrokerStub for UnmatchedTypesBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let mut joins = self.joins.lock().unwrap();
        *joins += 1;
        if *joins == self.cancel_at_join {
            self.cancel.cancel();
        }
        Ok(SubscriptionAssignment {
            id: joined_subscription(*joins),
            topology_version: 1,
            assigned: Vec::new(),
            unmatched_types: self.unmatched_types.clone(),
        })
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Ok(Box::pin(futures_util::stream::iter([Ok(
            WireFrame::Heartbeat {
                at: "2026-11-07T00:00:00Z".to_owned(),
            },
        )])))
    }
}

/// Runs a slot on `ORDERS_TOPIC` and `PAYMENTS_TOPIC` against an
/// [`UnmatchedTypesBroker`] cancelled at its `cancel_at_join`th JOIN, and returns
/// the `WARN` and `ERROR` lines the run logged.
async fn warn_lines_of_a_run(
    unmatched_types: Vec<UnmatchedTypes>,
    cancel_at_join: u128,
) -> Vec<String> {
    let capture = WarnCapture::install();
    let cancel = CancellationToken::new();
    let broker = Arc::new(UnmatchedTypesBroker {
        joins: Mutex::new(0),
        cancel_at_join,
        cancel: cancel.clone(),
        unmatched_types,
    });
    let mut dispatcher = slot_dispatcher(broker.clone());
    dispatcher.settings.topics = vec![ginst(ORDERS_TOPIC), ginst(PAYMENTS_TOPIC)];

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel),
    )
    .await
    .expect("the run ends within the bound");

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    assert_eq!(*broker.joins.lock().unwrap(), cancel_at_join);
    capture.lines()
}

/// Every entry of every interest goes into one line, by interest position, topic
/// and pattern.
#[tokio::test]
async fn a_join_with_unmatched_types_logs_one_warn_listing_every_entry() {
    let lines = warn_lines_of_a_run(
        vec![
            UnmatchedTypes {
                interest: 0,
                types: vec![gpat(ORDER_CREATED)],
            },
            UnmatchedTypes {
                interest: 1,
                types: vec![
                    gpat(ORDER_UPDATED),
                    gpat("gts.cf.core.events.event.v1~example.mock.route.*"),
                ],
            },
        ],
        1,
    )
    .await;

    assert_eq!(
        lines,
        vec![format!(
            "WARN a JOIN interest names event types that match no registered event type \
             unmatched=interest=0 topic={ORDERS_TOPIC} pattern={ORDER_CREATED}, \
             interest=1 topic={PAYMENTS_TOPIC} pattern={ORDER_UPDATED}, \
             interest=1 topic={PAYMENTS_TOPIC} \
             pattern=gts.cf.core.events.event.v1~example.mock.route.*"
        )]
    );
}

#[tokio::test]
async fn a_join_without_unmatched_types_logs_no_warn() {
    let lines = warn_lines_of_a_run(Vec::new(), 1).await;

    assert_eq!(lines, Vec::<String>::new());
}

/// The re-JOIN after a stream end reports the entries again.
#[tokio::test]
async fn a_rejoin_logs_the_warn_again() {
    let lines = warn_lines_of_a_run(
        vec![UnmatchedTypes {
            interest: 1,
            types: vec![gpat(ORDER_UPDATED)],
        }],
        2,
    )
    .await;

    let warn = format!(
        "WARN a JOIN interest names event types that match no registered event type \
         unmatched=interest=1 topic={PAYMENTS_TOPIC} pattern={ORDER_UPDATED}"
    );
    assert_eq!(lines, vec![warn.clone(), warn]);
}

/// A broker naming an interest the JOIN did not send still gets its entry
/// logged, with no topic to name.
#[tokio::test]
async fn an_entry_past_the_joined_interests_logs_an_unknown_topic() {
    let lines = warn_lines_of_a_run(
        vec![UnmatchedTypes {
            interest: 2,
            types: vec![gpat(ORDER_UPDATED)],
        }],
        1,
    )
    .await;

    assert_eq!(
        lines,
        vec![format!(
            "WARN a JOIN interest names event types that match no registered event type \
             unmatched=interest=2 topic=? pattern={ORDER_UPDATED}"
        )]
    );
}

/// An offset store for the transactional slot tests: it loads `Earliest` and accepts
/// every commit without writing anything.
#[cfg(feature = "db")]
struct NoopTxOffsets;

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl OffsetStore for NoopTxOffsets {
    async fn load_position(
        &self,
        _group: &ConsumerGroupId,
        _topic: &GtsInstanceId,
        _partition: u32,
    ) -> Result<Position, OffsetManagerError> {
        Ok(Fallback::Earliest.into())
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl CommitOffsetInTx for NoopTxOffsets {
    async fn commit_in_tx<TX>(
        &self,
        _txn: &TX,
        _group: &ConsumerGroupId,
        _topic: &GtsInstanceId,
        _partition: u32,
        _offset: Sequence,
    ) -> Result<(), OffsetManagerError>
    where
        TX: toolkit_db::secure::DBRunner + Sync,
    {
        Ok(())
    }
}

/// A transactional handler the slot tests never reach: no stream yields an event.
#[cfg(feature = "db")]
struct UnusedTxHandler;

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl DeliverTxBatch<NoopTxOffsets> for UnusedTxHandler {
    async fn deliver(
        &self,
        _batch: &EventBatch<'_, serde_json::Value>,
        _positions: &[StoredPosition],
        _delivery: &Delivery,
        _commit: TxCommitHandle<NoopTxOffsets>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

/// The transactional twin of `slot_dispatcher`.
#[cfg(feature = "db")]
fn tx_slot_dispatcher(
    broker: Arc<dyn EventBrokerApi>,
) -> TxSlotDispatcher<UnusedTxHandler, NoopTxOffsets> {
    TxSlotDispatcher {
        settings: slot_settings(broker),
        offset_manager: Arc::new(NoopTxOffsets),
        handler: Arc::new(UnusedTxHandler),
    }
}

/// The transactional twin of
/// `a_stream_that_fails_before_its_first_frame_every_time_exhausts_the_rejoin_budget`.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_transactional_stream_that_fails_before_its_first_frame_every_time_exhausts_the_rejoin_budget()
 {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::default());
    let dispatcher = tx_slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(
            Arc::new(SecurityContext::anonymous()),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the run ends within the bound");

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (3, "max transactional re-JOIN attempts exceeded")
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![
            joined_subscription(1),
            joined_subscription(2),
            joined_subscription(3),
        ]
    );
}

/// The transactional twin of
/// `positions_not_set_on_every_open_exhausts_the_rejoin_budget`.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_transactional_positions_not_set_on_every_open_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::failing_with(
        positions_not_set,
        FailsAt::Open,
    ));
    let dispatcher = tx_slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(
            Arc::new(SecurityContext::anonymous()),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the run ends within the bound");

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (3, "max transactional re-JOIN attempts exceeded")
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![
            joined_subscription(1),
            joined_subscription(2),
            joined_subscription(3),
        ]
    );
}

/// The transactional twin of
/// `positions_not_set_as_every_first_item_exhausts_the_rejoin_budget`.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_transactional_positions_not_set_as_every_first_item_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::failing_with(
        positions_not_set,
        FailsAt::FirstItem,
    ));
    let dispatcher = tx_slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(
            Arc::new(SecurityContext::anonymous()),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the run ends within the bound");

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (3, "max transactional re-JOIN attempts exceeded")
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![
            joined_subscription(1),
            joined_subscription(2),
            joined_subscription(3),
        ]
    );
}

/// The transactional twin of
/// `a_run_cancelled_while_awaiting_the_first_frame_ends_without_a_rejoin`.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_transactional_run_cancelled_while_awaiting_the_first_frame_ends_without_a_rejoin() {
    let cancel = CancellationToken::new();
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::cancelling(cancel.clone()));
    let dispatcher = tx_slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel),
    )
    .await
    .expect("the run ends within the bound");

    let Ok(()) = result else {
        panic!("expected the cancelled run to end cleanly, got {result:?}");
    };
    assert_eq!(*broker.joins.lock().unwrap(), 1);
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(1)]
    );
}

/// The transactional twin of
/// `a_stream_that_opens_every_time_never_exhausts_the_rejoin_budget`.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_transactional_stream_that_opens_every_time_never_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let cancel = CancellationToken::new();
    let broker = Arc::new(OpensEveryTimeBroker::cancelling_at_join(4, cancel.clone()));
    let dispatcher = tx_slot_dispatcher(broker.clone());

    let result = tokio::time::timeout(
        BOUND,
        dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel),
    )
    .await
    .expect("the run ends within the bound");

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    assert_eq!(*broker.joins.lock().unwrap(), 4);
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(4)]
    );
}

/// Awaits a slot's `run` within `BOUND`.
async fn within_bound<T>(run: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, run)
        .await
        .expect("the run ends within the bound")
}

/// The `SubscriptionRecoveryExhausted` a transport reports for a stream open.
fn recovery_exhausted() -> EventBrokerError {
    EventBrokerError::SubscriptionRecoveryExhausted {
        attempts: 7,
        detail: "injected".to_owned(),
    }
}

/// A plain stream open refused with `SubscriptionRecoveryExhausted` ends the run with
/// the exhaustion, without a leave or a re-JOIN.
#[tokio::test]
async fn a_recovery_exhausted_stream_open_ends_the_plain_run() {
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::failing_with(
        recovery_exhausted,
        FailsAt::Open,
    ));
    let dispatcher = slot_dispatcher(broker.clone());

    let result = within_bound(dispatcher.run(
        Arc::new(SecurityContext::anonymous()),
        CancellationToken::new(),
    ))
    .await;

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (0, "stream open: recovery exhausted")
    );
    assert_eq!(*broker.joins.lock().unwrap(), 1);
    assert_eq!(*broker.leave_calls.lock().unwrap(), Vec::new());
}

/// A transactional stream open refused with `SubscriptionRecoveryExhausted` is a
/// failed open like any other: the run leaves and re-JOINs until its re-JOIN budget
/// is spent.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_recovery_exhausted_stream_open_rejoins_the_transactional_run() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::failing_with(
        recovery_exhausted,
        FailsAt::Open,
    ));
    let dispatcher = tx_slot_dispatcher(broker.clone());

    let result = within_bound(dispatcher.run(
        Arc::new(SecurityContext::anonymous()),
        CancellationToken::new(),
    ))
    .await;

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (3, "max transactional re-JOIN attempts exceeded")
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![
            joined_subscription(1),
            joined_subscription(2),
            joined_subscription(3),
        ]
    );
}

/// A broker whose JOIN hands out the next subscription id with nothing assigned, so
/// no seek is made; whose stream yields one `ORDER_CREATED` event on partition 3 at
/// sequence 10, then a heartbeat; and which resolves `ORDER_CREATED` to
/// `ORDERS_TOPIC`, cancelling `cancel_at_type_lookup` as it does. Built with
/// `failing_every_rejoin`, every JOIN after the first fails with `Transport("injected")`.
struct OneEventBroker {
    joins: Mutex<u128>,
    leave_calls: Mutex<Vec<SubscriptionId>>,
    cancel_at_type_lookup: CancellationToken,
    rejoins_fail: bool,
}

impl OneEventBroker {
    fn new(cancel_at_type_lookup: CancellationToken) -> Self {
        Self {
            joins: Mutex::new(0),
            leave_calls: Mutex::new(Vec::new()),
            cancel_at_type_lookup,
            rejoins_fail: false,
        }
    }

    /// A broker whose every JOIN after the first fails.
    fn failing_every_rejoin() -> Self {
        Self {
            rejoins_fail: true,
            ..Self::new(CancellationToken::new())
        }
    }
}

#[async_trait::async_trait]
impl BrokerStub for OneEventBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let mut joins = self.joins.lock().unwrap();
        *joins += 1;
        if self.rejoins_fail && *joins > 1 {
            return Err(EventBrokerError::Transport("injected".to_owned()));
        }
        Ok(SubscriptionAssignment {
            id: joined_subscription(*joins),
            topology_version: 1,
            assigned: Vec::new(),
            unmatched_types: Vec::new(),
        })
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        self.leave_calls.lock().unwrap().push(id);
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Ok(Box::pin(futures_util::stream::iter([
            Ok(WireFrame::Event(raw_event_on(ORDER_CREATED, 3, 10))),
            Ok(WireFrame::Heartbeat {
                at: "2026-11-07T00:00:00Z".to_owned(),
            }),
        ])))
    }

    async fn get_event_type(
        &self,
        _ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError> {
        self.cancel_at_type_lookup.cancel();
        Ok(EventType {
            id: gtype(id),
            topic: ginst(ORDERS_TOPIC),
            description: None,
            allowed_subject_types: Vec::new(),
            partition_key: "/source".to_owned(),
            data_schema: serde_json::json!({}),
        })
    }
}

type SharedRuntimeEvents = Arc<Mutex<Vec<ConsumerRuntimeEvent>>>;

/// Records every runtime event the slot emits.
#[derive(Default)]
struct RecordingRuntimeListener {
    events: SharedRuntimeEvents,
}

#[async_trait::async_trait]
impl ConsumerRuntimeListener for RecordingRuntimeListener {
    async fn on_consumer_event(&self, event: &ConsumerRuntimeEvent) -> Result<(), ConsumerError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

/// The settings of a `slot_settings` slot on `broker` that reports to `listener`.
fn listened_slot_settings(
    broker: Arc<dyn EventBrokerApi>,
    listener: RecordingRuntimeListener,
) -> SlotSettings {
    SlotSettings {
        listeners: vec![Arc::new(listener)],
        ..slot_settings(broker)
    }
}

/// Acks every batch through its last event, recording each event's sequence, then
/// cancels `cancel`, so the run ends before it reads another frame.
struct AckThenCancelHandler {
    calls: SharedOffsets,
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl BatchHandler<serde_json::Value> for AckThenCancelHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        self.calls
            .lock()
            .unwrap()
            .extend(chunk.iter().map(sequence_of));
        self.cancel.cancel();
        Ok(BatchHandlerOutcome::AdvanceThrough {
            offset: chunk
                .last()
                .and_then(EventV1::sequence)
                .expect("a delivered batch is not empty"),
        })
    }
}

/// A plain slot on `broker` in `Manual` commit mode reporting to `listener`, whose
/// handler acks and then cancels `cancel`.
fn ack_then_cancel_dispatcher(
    broker: Arc<dyn EventBrokerApi>,
    listener: RecordingRuntimeListener,
    calls: SharedOffsets,
    cancel: CancellationToken,
) -> SlotDispatcher<BatchHandlerAdapter<AckThenCancelHandler>, InMemoryOffsetManager> {
    SlotDispatcher {
        settings: listened_slot_settings(broker, listener),
        offset_manager: Arc::new(InMemoryOffsetManager::new(Fallback::Earliest)),
        handler: Arc::new(BatchHandlerAdapter::new(AckThenCancelHandler {
            calls,
            cancel,
        })),
        commit_mode: ConsumerCommitMode::Manual,
    }
}

/// Commits the last offset of every batch in a transaction of its own, recording
/// each event's sequence, then cancels `cancel`, so the run ends before it reads
/// another frame.
#[cfg(feature = "db")]
struct CommitThenCancelTxHandler {
    db: toolkit_db::Db,
    calls: SharedOffsets,
    cancel: CancellationToken,
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl DeliverTxBatch<NoopTxOffsets> for CommitThenCancelTxHandler {
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        _delivery: &Delivery,
        commit: TxCommitHandle<NoopTxOffsets>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls
            .lock()
            .unwrap()
            .extend(batch.iter().map(sequence_of));
        let offset = positions
            .last()
            .expect("a delivered batch is not empty")
            .sequence;
        self.db
            .transaction_ref(move |tx| {
                Box::pin(async move {
                    commit.commit_offset_in_tx(tx, offset).await.map_err(|err| {
                        toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(err.to_string()))
                    })
                })
            })
            .await
            .map_err(|err| ConsumerError::Internal(err.to_string()))?;
        self.cancel.cancel();
        Ok(HandlerOutcome::Success)
    }
}

/// A transactional slot on `broker` reporting to `listener`, whose handler commits
/// and then cancels `cancel`.
#[cfg(feature = "db")]
async fn commit_then_cancel_tx_dispatcher(
    broker: Arc<dyn EventBrokerApi>,
    listener: RecordingRuntimeListener,
    calls: SharedOffsets,
    cancel: CancellationToken,
) -> TxSlotDispatcher<CommitThenCancelTxHandler, NoopTxOffsets> {
    let db = toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default())
        .await
        .expect("sqlite db");
    TxSlotDispatcher {
        settings: listened_slot_settings(broker, listener),
        offset_manager: Arc::new(NoopTxOffsets),
        handler: Arc::new(CommitThenCancelTxHandler { db, calls, cancel }),
    }
}

/// The events of `events` that report a batch's handling and the progress it made.
/// Their other emitters are silent in these runs: `Manual` mode starts no auto-commit
/// timer, and the stream carries no control or topology frame.
fn batch_outcome_events(events: &SharedRuntimeEvents) -> Vec<ConsumerRuntimeEvent> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::HandlerBatchCompleted { .. }
                    | ConsumerRuntimeEvent::OffsetCommitted { .. }
                    | ConsumerRuntimeEvent::ProgressAdvanced { .. }
            )
        })
        .cloned()
        .collect()
}

/// The `HandlerBatchStarted` events of `events`.
fn batch_started_events(events: &SharedRuntimeEvents) -> Vec<ConsumerRuntimeEvent> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, ConsumerRuntimeEvent::HandlerBatchStarted { .. }))
        .cloned()
        .collect()
}

/// A settled plain batch advances the in-memory cursor and reports the progress; the
/// offset is committed later (by the timer in `Auto` mode), so no `OffsetCommitted`
/// comes with it.
#[tokio::test]
async fn a_settled_plain_batch_reports_its_completion_then_its_progress() {
    let cancel = CancellationToken::new();
    let broker = Arc::new(OneEventBroker::new(CancellationToken::new()));
    let listener = RecordingRuntimeListener::default();
    let events = listener.events.clone();
    let calls = SharedOffsets::default();
    let dispatcher =
        ack_then_cancel_dispatcher(broker.clone(), listener, calls.clone(), cancel.clone());

    let result = within_bound(dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel)).await;

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    assert_eq!(*calls.lock().unwrap(), vec![10]);
    assert_eq!(
        format!("{:#?}", batch_outcome_events(&events)),
        format!(
            "{:#?}",
            vec![
                ConsumerRuntimeEvent::HandlerBatchCompleted {
                    topic: ginst(ORDERS_TOPIC),
                    partition: 3,
                    outcome: BatchHandlerOutcome::AdvanceThrough {
                        offset: Sequence::assigned(10),
                    },
                },
                ConsumerRuntimeEvent::ProgressAdvanced {
                    subscription: joined_subscription(1),
                    progress: vec![PartitionProgress {
                        topic: ginst(ORDERS_TOPIC),
                        partition: 3,
                        offset: Sequence::assigned(10),
                    }],
                },
            ]
        )
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(1)]
    );
}

/// A settled transactional batch was committed by its handler, so the runtime reports
/// the commit before the progress.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_settled_transactional_batch_reports_its_completion_its_commit_then_its_progress() {
    let cancel = CancellationToken::new();
    let broker = Arc::new(OneEventBroker::new(CancellationToken::new()));
    let listener = RecordingRuntimeListener::default();
    let events = listener.events.clone();
    let calls = SharedOffsets::default();
    let dispatcher =
        commit_then_cancel_tx_dispatcher(broker.clone(), listener, calls.clone(), cancel.clone())
            .await;

    let result = within_bound(dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel)).await;

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    assert_eq!(*calls.lock().unwrap(), vec![10]);
    assert_eq!(
        format!("{:#?}", batch_outcome_events(&events)),
        format!(
            "{:#?}",
            vec![
                ConsumerRuntimeEvent::HandlerBatchCompleted {
                    topic: ginst(ORDERS_TOPIC),
                    partition: 3,
                    outcome: BatchHandlerOutcome::Success,
                },
                ConsumerRuntimeEvent::OffsetCommitted {
                    topic: ginst(ORDERS_TOPIC),
                    partition: 3,
                    offset: Sequence::assigned(10),
                },
                ConsumerRuntimeEvent::ProgressAdvanced {
                    subscription: joined_subscription(1),
                    progress: vec![PartitionProgress {
                        topic: ginst(ORDERS_TOPIC),
                        partition: 3,
                        offset: Sequence::assigned(10),
                    }],
                },
            ]
        )
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(1)]
    );
}

/// A plain slot cancelled after it took an event but before the handler call (here
/// while it resolves the event's topic) does not call the handler.
#[tokio::test]
async fn a_plain_slot_cancelled_before_a_handler_call_does_not_deliver() {
    let cancel = CancellationToken::new();
    let broker = Arc::new(OneEventBroker::new(cancel.clone()));
    let listener = RecordingRuntimeListener::default();
    let events = listener.events.clone();
    let calls = SharedOffsets::default();
    let dispatcher = ack_then_cancel_dispatcher(
        broker.clone(),
        listener,
        calls.clone(),
        CancellationToken::new(),
    );

    let result = within_bound(dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel)).await;

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    assert_eq!(*calls.lock().unwrap(), Vec::<i64>::new());
    assert_eq!(
        format!("{:#?}", batch_started_events(&events)),
        format!("{:#?}", Vec::<ConsumerRuntimeEvent>::new())
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(1)]
    );
}

/// A transactional slot does not check cancel before a handler call: cancelled at the
/// same point, it still delivers the event it took.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_transactional_slot_cancelled_before_a_handler_call_still_delivers() {
    let cancel = CancellationToken::new();
    let broker = Arc::new(OneEventBroker::new(cancel.clone()));
    let listener = RecordingRuntimeListener::default();
    let events = listener.events.clone();
    let calls = SharedOffsets::default();
    let dispatcher = commit_then_cancel_tx_dispatcher(
        broker.clone(),
        listener,
        calls.clone(),
        CancellationToken::new(),
    )
    .await;

    let result = within_bound(dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel)).await;

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    assert_eq!(*calls.lock().unwrap(), vec![10]);
    assert_eq!(
        format!("{:#?}", batch_started_events(&events)),
        format!(
            "{:#?}",
            vec![ConsumerRuntimeEvent::HandlerBatchStarted {
                topic: ginst(ORDERS_TOPIC),
                partition: 3,
                len: 1,
            }]
        )
    );
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(1)]
    );
}

/// A broker whose JOIN hands out the next subscription id with nothing assigned, so
/// no seek is made; whose stream yields two `ORDER_CREATED` events on partition 3, at
/// sequences 10 and 11, both queued at once, then ends; and which resolves
/// `ORDER_CREATED` to `ORDERS_TOPIC`.
#[cfg(feature = "db")]
#[derive(Default)]
struct TwoEventsBroker {
    joins: Mutex<u128>,
    leave_calls: Mutex<Vec<SubscriptionId>>,
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl BrokerStub for TwoEventsBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let mut joins = self.joins.lock().unwrap();
        *joins += 1;
        Ok(SubscriptionAssignment {
            id: joined_subscription(*joins),
            topology_version: 1,
            assigned: Vec::new(),
            unmatched_types: Vec::new(),
        })
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        self.leave_calls.lock().unwrap().push(id);
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Ok(Box::pin(futures_util::stream::iter([
            Ok(WireFrame::Event(raw_event_on(ORDER_CREATED, 3, 10))),
            Ok(WireFrame::Event(raw_event_on(ORDER_CREATED, 3, 11))),
        ])))
    }

    async fn get_event_type(
        &self,
        _ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError> {
        Ok(EventType {
            id: gtype(id),
            topic: ginst(ORDERS_TOPIC),
            description: None,
            allowed_subject_types: Vec::new(),
            partition_key: "/source".to_owned(),
            data_schema: serde_json::json!({}),
        })
    }
}

/// A cancel already set wins over a frame that is ready in the same poll. The slot is
/// transactional because that flavour still delivers an event it reads after a cancel,
/// so a second event read would reach the handler. The handler commits the first
/// event and cancels while the second is already queued: the next frame wait finds
/// both the cancel and that event ready. Without that priority the wait would take the
/// event about half the time, so all 20 runs here would deliver only the first event
/// by chance about once in 2^20.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_cancel_set_before_a_ready_frame_wins_over_it() {
    for run in 1..=20 {
        let cancel = CancellationToken::new();
        let broker = Arc::new(TwoEventsBroker::default());
        let listener = RecordingRuntimeListener::default();
        let events = listener.events.clone();
        let calls = SharedOffsets::default();
        let dispatcher = commit_then_cancel_tx_dispatcher(
            broker.clone(),
            listener,
            calls.clone(),
            cancel.clone(),
        )
        .await;

        let result =
            within_bound(dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel)).await;

        let Ok(()) = result else {
            panic!("run {run}: expected the run to end cleanly at the cancel, got {result:?}");
        };
        assert_eq!(*calls.lock().unwrap(), vec![10], "run {run}");
        assert_eq!(
            format!("{:#?}", batch_started_events(&events)),
            format!(
                "{:#?}",
                vec![ConsumerRuntimeEvent::HandlerBatchStarted {
                    topic: ginst(ORDERS_TOPIC),
                    partition: 3,
                    len: 1,
                }]
            ),
            "run {run}"
        );
        assert_eq!(
            *broker.leave_calls.lock().unwrap(),
            vec![joined_subscription(1)],
            "run {run}"
        );
    }
}

/// The tenant and ownership filter of one JOIN interest.
type JoinedTenant = (InterestTenant, OwnershipFilter);

/// A broker that records the interest tenants and ownership filters of each JOIN
/// and cancels the run at its first JOIN. Every stream yields one heartbeat and
/// ends.
struct RecordsJoinTenantsBroker {
    tenants: Mutex<Vec<Vec<JoinedTenant>>>,
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl BrokerStub for RecordsJoinTenantsBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        self.tenants.lock().unwrap().push(
            req.interests
                .iter()
                .map(|interest| (interest.subject_tenant(), interest.ownership_filter()))
                .collect(),
        );
        self.cancel.cancel();
        Ok(SubscriptionAssignment {
            id: joined_subscription(1),
            topology_version: 1,
            assigned: Vec::new(),
            unmatched_types: Vec::new(),
        })
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Ok(Box::pin(futures_util::stream::iter([Ok(
            WireFrame::Heartbeat {
                at: "2026-11-12T00:00:00Z".to_owned(),
            },
        )])))
    }
}

/// The run's result and the interest tenants and ownership filters of each JOIN
/// a slot with `tenant` and `ownership_filter` makes, run as `ctx`.
async fn joined_tenants(
    tenant: InterestTenant,
    ownership_filter: OwnershipFilter,
    ctx: SecurityContext,
) -> (Result<(), EventBrokerError>, Vec<Vec<JoinedTenant>>) {
    let cancel = CancellationToken::new();
    let broker = Arc::new(RecordsJoinTenantsBroker {
        tenants: Mutex::new(Vec::new()),
        cancel: cancel.clone(),
    });
    let mut dispatcher = slot_dispatcher(broker.clone());
    dispatcher.settings.subject_tenant = tenant;
    dispatcher.settings.ownership_filter = ownership_filter;

    let result = tokio::time::timeout(BOUND, dispatcher.run(Arc::new(ctx), cancel))
        .await
        .expect("the run ends within the bound");
    let tenants = broker.tenants.lock().unwrap().clone();
    (result, tenants)
}

/// A caller in tenant `0x1818`.
fn tenant_caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x1819))
        .subject_tenant_id(Uuid::from_u128(0x1818))
        .build()
        .unwrap()
}

/// The broker resolves the caller's tenant itself, so the SDK names none.
#[tokio::test]
async fn a_consumer_without_a_tenant_leaves_the_tenant_to_the_broker() {
    let (result, tenants) = joined_tenants(
        InterestTenant::Caller,
        OwnershipFilter::OnlyMine,
        tenant_caller(),
    )
    .await;

    assert!(matches!(result, Ok(())), "expected Ok, got {result:?}");
    assert_eq!(
        tenants,
        vec![vec![(InterestTenant::Caller, OwnershipFilter::OnlyMine)]]
    );
}

#[tokio::test]
async fn an_explicit_consumer_tenant_and_ownership_filter_are_sent_as_set() {
    let (result, tenants) = joined_tenants(
        InterestTenant::Tenant(Uuid::from_u128(0x1820)),
        OwnershipFilter::EntireTenant,
        tenant_caller(),
    )
    .await;

    assert!(matches!(result, Ok(())), "expected Ok, got {result:?}");
    assert_eq!(
        tenants,
        vec![vec![(
            InterestTenant::Tenant(Uuid::from_u128(0x1820)),
            OwnershipFilter::EntireTenant
        )]]
    );
}

/// Over REST the bearer token alone identifies the caller, whose tenant the
/// broker resolves: a token-only context joins with no tenant named.
#[tokio::test]
async fn a_token_only_consumer_without_a_tenant_joins_with_the_tenant_left_to_the_broker() {
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x1921))
        .subject_tenant_id(Uuid::nil())
        .bearer_token("a-token-the-stub-does-not-read".to_owned())
        .build()
        .unwrap();

    let (result, tenants) =
        joined_tenants(InterestTenant::Caller, OwnershipFilter::OnlyMine, ctx).await;

    assert!(matches!(result, Ok(())), "expected Ok, got {result:?}");
    assert_eq!(
        tenants,
        vec![vec![(InterestTenant::Caller, OwnershipFilter::OnlyMine)]]
    );
}

#[tokio::test]
async fn a_consumer_with_an_explicit_nil_tenant_is_refused_before_any_join_is_sent() {
    let (result, tenants) = joined_tenants(
        InterestTenant::Tenant(Uuid::nil()),
        OwnershipFilter::EntireTenant,
        tenant_caller(),
    )
    .await;

    let Err(EventBrokerError::InvalidConsumerOptions { detail }) = result else {
        panic!("expected InvalidConsumerOptions, got {result:?}");
    };
    assert_eq!(
        detail,
        "subscription interest subject_tenant must not be the nil UUID"
    );
    assert_eq!(tenants, Vec::<Vec<JoinedTenant>>::new());
}

/// The interests of each JOIN, as `(topic, subject-type selector)` pairs.
type JoinedSelectors = Vec<Vec<(String, Vec<String>)>>;

/// A broker that records the topic and subject-type selector of every interest of
/// each JOIN and cancels the run at its first JOIN. Every stream yields one
/// heartbeat and ends.
struct RecordsJoinSelectorsBroker {
    selectors: Mutex<JoinedSelectors>,
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl BrokerStub for RecordsJoinSelectorsBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        self.selectors.lock().unwrap().push(
            req.interests
                .iter()
                .map(|interest| {
                    (
                        interest.topic().as_ref().to_owned(),
                        interest
                            .subject_types()
                            .iter()
                            .map(|pattern| pattern.pattern().to_owned())
                            .collect(),
                    )
                })
                .collect(),
        );
        self.cancel.cancel();
        Ok(SubscriptionAssignment {
            id: joined_subscription(1),
            topology_version: 1,
            assigned: Vec::new(),
            unmatched_types: Vec::new(),
        })
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Ok(Box::pin(futures_util::stream::iter([Ok(
            WireFrame::Heartbeat {
                at: "2026-11-12T00:00:00Z".to_owned(),
            },
        )])))
    }
}

const SELECTED_SUBJECT: &str = "gts.x.eb.test.customer.v1~";
const SECOND_TOPIC: &str = gts_id!("cf.core.events.topic.v1~x.eb.test.payments.v1");

/// The interests of each JOIN a consumer built by `configure` makes: the slot runs
/// with the settings the consumer builder hands its slots.
async fn joined_selectors(
    configure: impl FnOnce(super::ConsumerBuilder<()>) -> super::ConsumerBuilder<()>,
) -> JoinedSelectors {
    let cancel = CancellationToken::new();
    let broker = Arc::new(RecordsJoinSelectorsBroker {
        selectors: Mutex::new(Vec::new()),
        cancel: cancel.clone(),
    });
    let builder = configure(
        super::ConsumerBuilder::new(broker.clone())
            .subject_tenant(Uuid::from_u128(0x1924))
            .subject_types([gpat(SELECTED_SUBJECT)]),
    )
    .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest));
    let settings = builder
        .effective_settings()
        .expect("the default profile resolves");
    let dispatcher = SlotDispatcher {
        settings: SlotSettings::for_consumer(
            &builder,
            &settings,
            broker.clone(),
            Arc::new(ConsumerTypeCache::default()),
            ConsumerGroupId(Uuid::from_u128(0x1923)),
        ),
        offset_manager: Arc::new(InMemoryOffsetManager::new(Fallback::Earliest)),
        handler: Arc::new(BatchHandlerAdapter::new(AckAllBatchHandler {
            calls: SharedPartitionCalls::default(),
        })),
        commit_mode: ConsumerCommitMode::Manual,
    };
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x1925))
        .subject_tenant_id(Uuid::from_u128(0x1924))
        .build()
        .unwrap();

    let result = tokio::time::timeout(BOUND, dispatcher.run(Arc::new(ctx), cancel))
        .await
        .expect("the run ends within the bound");

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    broker.selectors.lock().unwrap().clone()
}

#[tokio::test]
async fn a_consumers_subject_types_reach_every_joined_interest() {
    assert_eq!(
        joined_selectors(|builder| builder.topics([ginst(ORDERS_TOPIC), ginst(SECOND_TOPIC)]))
            .await,
        vec![vec![
            (ORDERS_TOPIC.to_owned(), vec![SELECTED_SUBJECT.to_owned()]),
            (SECOND_TOPIC.to_owned(), vec![SELECTED_SUBJECT.to_owned()]),
        ]]
    );
}

#[tokio::test]
async fn a_consumers_subject_types_reach_every_explicit_subscription_interest() {
    let interest = |topic: &str| {
        super::SubscriptionInterest::builder()
            .topic(ginst(topic))
            .types([gpat("gts.cf.core.events.event.v1~*")])
            .build()
            .unwrap()
    };

    assert_eq!(
        joined_selectors(|builder| {
            builder.subscription_interests([interest(ORDERS_TOPIC), interest(SECOND_TOPIC)])
        })
        .await,
        vec![vec![
            (ORDERS_TOPIC.to_owned(), vec![SELECTED_SUBJECT.to_owned()]),
            (SECOND_TOPIC.to_owned(), vec![SELECTED_SUBJECT.to_owned()]),
        ]]
    );
}

/// A broker whose one JOIN assigns partitions 0-3 of `ORDERS_TOPIC`, whose seek
/// accepts any position, and whose stream yields `frames`, then cancels `cancel` and
/// stays open, so the run ends only after it has taken every frame.
struct ScriptedTopologyBroker {
    frames: Mutex<Vec<WireFrame>>,
    cancel: CancellationToken,
}

impl ScriptedTopologyBroker {
    fn new(frames: Vec<WireFrame>, cancel: CancellationToken) -> Self {
        Self {
            frames: Mutex::new(frames),
            cancel,
        }
    }
}

#[async_trait::async_trait]
impl BrokerStub for ScriptedTopologyBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        Ok(SubscriptionAssignment {
            id: joined_subscription(1),
            topology_version: 1,
            assigned: orders_partitions(&[0, 1, 2, 3]),
            unmatched_types: Vec::new(),
        })
    }

    async fn seek(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
        _topology_version: i64,
        _positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        Ok(Vec::new())
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        _id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        let frames = std::mem::take(&mut *self.frames.lock().unwrap());
        let cancel = self.cancel.clone();
        Ok(Box::pin(futures_util::StreamExt::chain(
            futures_util::stream::iter(frames.into_iter().map(Ok)),
            futures_util::stream::once(async move {
                cancel.cancel();
                std::future::pending::<Result<WireFrame, EventBrokerError>>().await
            }),
        )))
    }
}

/// `ORDERS_TOPIC`'s `partitions`, in that order.
fn orders_partitions(partitions: &[u32]) -> Vec<AssignedPartition> {
    partitions
        .iter()
        .map(|&partition| AssignedPartition {
            topic: ginst(ORDERS_TOPIC),
            partition,
        })
        .collect()
}

/// A topology frame carrying `ORDERS_TOPIC`'s `partitions`, in that order.
fn orders_topology(partitions: &[u32]) -> WireFrame {
    WireFrame::Topology {
        topology_version: 2,
        assigned: partitions
            .iter()
            .map(|&partition| PartitionPosition {
                topic: ginst(ORDERS_TOPIC),
                partition,
                offset: Sequence::NONE,
                last_examined: Sequence::NONE,
            })
            .collect(),
    }
}

/// A topology frame carrying the `(topic, partition)` pairs of `partitions`, in that
/// order.
fn topology_of(partitions: &[(&str, u32)]) -> WireFrame {
    WireFrame::Topology {
        topology_version: 2,
        assigned: partitions
            .iter()
            .map(|&(topic, partition)| PartitionPosition {
                topic: ginst(topic),
                partition,
                offset: Sequence::NONE,
                last_examined: Sequence::NONE,
            })
            .collect(),
    }
}

/// The `SubscriptionStarted` and `AssignmentChanged` events of the joined subscription
/// in a plain slot run on a `ScriptedTopologyBroker` yielding `frames`. The run JOINs
/// once, so its one `join_subscription` emits one `SubscriptionStarted` and one
/// `AssignmentChanged` before the stream opens; every later `AssignmentChanged` comes
/// from the stream's topology frames.
async fn assignment_events_of_a_run(frames: Vec<WireFrame>) -> Vec<ConsumerRuntimeEvent> {
    let cancel = CancellationToken::new();
    let broker = Arc::new(ScriptedTopologyBroker::new(frames, cancel.clone()));
    let listener = RecordingRuntimeListener::default();
    let events = listener.events.clone();
    let dispatcher = SlotDispatcher {
        settings: listened_slot_settings(broker, listener),
        offset_manager: Arc::new(InMemoryOffsetManager::new(Fallback::Earliest)),
        handler: Arc::new(BatchHandlerAdapter::new(AckAllBatchHandler {
            calls: SharedPartitionCalls::default(),
        })),
        commit_mode: ConsumerCommitMode::Manual,
    };

    let result = within_bound(dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel)).await;

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::SubscriptionStarted { subscription, .. }
                    | ConsumerRuntimeEvent::AssignmentChanged { subscription, .. }
                    if *subscription == joined_subscription(1)
            )
        })
        .cloned()
        .collect()
}

/// A rebalance that takes partitions from an open stream arrives as a topology frame
/// with the partitions the slot keeps; the slot reports them as its new assignment.
#[tokio::test]
async fn a_topology_loss_reports_the_partitions_the_slot_keeps() {
    let events = assignment_events_of_a_run(vec![
        orders_topology(&[0, 1, 2, 3]),
        orders_topology(&[0, 1]),
    ])
    .await;

    assert_eq!(
        format!("{events:#?}"),
        format!(
            "{:#?}",
            vec![
                ConsumerRuntimeEvent::SubscriptionStarted {
                    consumer_group: ConsumerGroupId(Uuid::from_u128(0x1801)),
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1]),
                },
            ]
        )
    );
}

/// A rebalance between the JOIN and the stream's open shows as an open frame smaller
/// than the JOIN's assignment; the slot reports that frame's partitions.
#[tokio::test]
async fn a_loss_before_the_stream_opens_is_reported_at_its_first_frame() {
    let events = assignment_events_of_a_run(vec![orders_topology(&[0, 1])]).await;

    assert_eq!(
        format!("{events:#?}"),
        format!(
            "{:#?}",
            vec![
                ConsumerRuntimeEvent::SubscriptionStarted {
                    consumer_group: ConsumerGroupId(Uuid::from_u128(0x1801)),
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1]),
                },
            ]
        )
    );
}

/// A topology frame that only moves the topology version carries the partitions the
/// slot already holds, in whatever order; the slot reports nothing beyond its JOIN.
#[tokio::test]
async fn a_topology_frame_with_the_same_partitions_reports_nothing() {
    for partitions in [[0, 1, 2, 3], [3, 2, 1, 0]] {
        let events = assignment_events_of_a_run(vec![orders_topology(&partitions)]).await;

        assert_eq!(
            format!("{events:#?}"),
            format!(
                "{:#?}",
                vec![
                    ConsumerRuntimeEvent::SubscriptionStarted {
                        consumer_group: ConsumerGroupId(Uuid::from_u128(0x1801)),
                        subscription: joined_subscription(1),
                        assigned: orders_partitions(&[0, 1, 2, 3]),
                    },
                    ConsumerRuntimeEvent::AssignmentChanged {
                        subscription: joined_subscription(1),
                        assigned: orders_partitions(&[0, 1, 2, 3]),
                    },
                ]
            ),
            "a topology frame of partitions {partitions:?}"
        );
    }
}

/// Each loss is compared with the partitions the slot holds after the previous one, so
/// a repeated frame reports nothing and a further loss reports what is left.
#[tokio::test]
async fn successive_losses_each_report_the_current_partitions() {
    let events = assignment_events_of_a_run(vec![
        orders_topology(&[0, 1, 2]),
        orders_topology(&[0, 1, 2]),
        orders_topology(&[0]),
    ])
    .await;

    assert_eq!(
        format!("{events:#?}"),
        format!(
            "{:#?}",
            vec![
                ConsumerRuntimeEvent::SubscriptionStarted {
                    consumer_group: ConsumerGroupId(Uuid::from_u128(0x1801)),
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0]),
                },
            ]
        )
    );
}

/// A topology frame's partitions are reported in the frame's order, not in the order
/// the slot held them.
#[tokio::test]
async fn a_topology_loss_reports_the_kept_partitions_in_frame_order() {
    let events = assignment_events_of_a_run(vec![
        orders_topology(&[0, 1, 2, 3]),
        orders_topology(&[1, 0]),
    ])
    .await;

    assert_eq!(
        format!("{events:#?}"),
        format!(
            "{:#?}",
            vec![
                ConsumerRuntimeEvent::SubscriptionStarted {
                    consumer_group: ConsumerGroupId(Uuid::from_u128(0x1801)),
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[1, 0]),
                },
            ]
        )
    );
}

/// A partition is told apart by its topic as well as its number: a frame with the
/// JOIN's partition numbers, one of them on another topic, carries a different set,
/// and the slot reports it.
#[tokio::test]
async fn a_topology_frame_that_moves_a_partition_to_another_topic_reports_it() {
    let events = assignment_events_of_a_run(vec![topology_of(&[
        (ORDERS_TOPIC, 0),
        (ORDERS_TOPIC, 1),
        (ORDERS_TOPIC, 2),
        (PAYMENTS_TOPIC, 3),
    ])])
    .await;

    assert_eq!(
        format!("{events:#?}"),
        format!(
            "{:#?}",
            vec![
                ConsumerRuntimeEvent::SubscriptionStarted {
                    consumer_group: ConsumerGroupId(Uuid::from_u128(0x1801)),
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: vec![
                        AssignedPartition {
                            topic: ginst(ORDERS_TOPIC),
                            partition: 0,
                        },
                        AssignedPartition {
                            topic: ginst(ORDERS_TOPIC),
                            partition: 1,
                        },
                        AssignedPartition {
                            topic: ginst(ORDERS_TOPIC),
                            partition: 2,
                        },
                        AssignedPartition {
                            topic: ginst(PAYMENTS_TOPIC),
                            partition: 3,
                        },
                    ],
                },
            ]
        )
    );
}

/// One offset write recorded by [`RecordingCommits`]: group, topic, partition and offset.
type RecordedFrameCommit = (ConsumerGroupId, GtsInstanceId, u32, Sequence);

/// An offset store that records every `commit` and starts every partition at the earliest
/// offset.
#[derive(Default)]
struct RecordingCommits {
    commits: Mutex<Vec<RecordedFrameCommit>>,
}

#[async_trait::async_trait]
impl super::OffsetStore for RecordingCommits {
    async fn load_position(
        &self,
        _group: &ConsumerGroupId,
        _topic: &GtsInstanceId,
        _partition: u32,
    ) -> Result<crate::api::Position, crate::error::OffsetManagerError> {
        Ok(Fallback::Earliest.into())
    }
}

#[async_trait::async_trait]
impl super::CommitOffset for RecordingCommits {
    async fn commit(
        &self,
        group: &ConsumerGroupId,
        topic: &GtsInstanceId,
        partition: u32,
        offset: Sequence,
    ) -> Result<(), crate::error::OffsetManagerError> {
        self.commits
            .lock()
            .unwrap()
            .push((*group, topic.clone(), partition, offset));
        Ok(())
    }
}

/// A topology frame's positions are committed each under its own topic, so two topics
/// sharing a partition number keep separate offsets.
#[tokio::test]
async fn frame_positions_are_committed_under_each_positions_topic() {
    let cancel = CancellationToken::new();
    let frame = WireFrame::Topology {
        topology_version: 2,
        assigned: vec![
            PartitionPosition {
                topic: ginst(ORDERS_TOPIC),
                partition: 0,
                offset: Sequence::assigned(5),
                last_examined: Sequence::assigned(5),
            },
            PartitionPosition {
                topic: ginst(PAYMENTS_TOPIC),
                partition: 0,
                offset: Sequence::assigned(9),
                last_examined: Sequence::assigned(9),
            },
        ],
    };
    let broker = Arc::new(ScriptedTopologyBroker::new(vec![frame], cancel.clone()));
    let offsets = Arc::new(RecordingCommits::default());
    let dispatcher = SlotDispatcher {
        settings: slot_settings(broker),
        offset_manager: offsets.clone(),
        handler: Arc::new(BatchHandlerAdapter::new(AckAllBatchHandler {
            calls: SharedPartitionCalls::default(),
        })),
        commit_mode: ConsumerCommitMode::Manual,
    };

    let result = within_bound(dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel)).await;

    let Ok(()) = result else {
        panic!("expected the run to end cleanly at the cancel, got {result:?}");
    };
    assert_eq!(
        *offsets.commits.lock().unwrap(),
        vec![
            (
                ConsumerGroupId(Uuid::from_u128(0x1801)),
                ginst(ORDERS_TOPIC),
                0,
                Sequence::assigned(5)
            ),
            (
                ConsumerGroupId(Uuid::from_u128(0x1801)),
                ginst(PAYMENTS_TOPIC),
                0,
                Sequence::assigned(9)
            ),
        ]
    );
}

/// A rebalance between the JOIN and the stream's open can add partitions as well as
/// take them: the open frame then carries more than the JOIN's assignment, and the
/// slot reports that frame's partitions.
#[tokio::test]
async fn a_larger_first_frame_reports_the_gained_partitions() {
    let events = assignment_events_of_a_run(vec![orders_topology(&[0, 1, 2, 3, 4])]).await;

    assert_eq!(
        format!("{events:#?}"),
        format!(
            "{:#?}",
            vec![
                ConsumerRuntimeEvent::SubscriptionStarted {
                    consumer_group: ConsumerGroupId(Uuid::from_u128(0x1801)),
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3]),
                },
                ConsumerRuntimeEvent::AssignmentChanged {
                    subscription: joined_subscription(1),
                    assigned: orders_partitions(&[0, 1, 2, 3, 4]),
                },
            ]
        )
    );
}

/// A broker whose `n`th JOIN, counting from 1, assigns partition 0 of `ORDERS_TOPIC`
/// to `joined_subscription(n)` unless `join_fails(n)`; whose `n`th seek, counting from
/// 1, fails when `seek_fails(n)`; and whose stream yields one heartbeat, then ends for
/// the subscriptions in `ending_streams`, or else cancels `cancel` and stays open. With
/// `streams_fail` set, every stream open fails instead. Every injected failure is
/// `Transport("injected")`.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent fault switches of a test double, set per test"
)]
struct SeekOrJoinFailsBroker {
    joins: Mutex<u128>,
    seeks: Mutex<Vec<SubscriptionId>>,
    leave_calls: Mutex<Vec<SubscriptionId>>,
    join_fails: fn(u128) -> bool,
    seek_fails: fn(usize) -> bool,
    ending_streams: Vec<SubscriptionId>,
    streams_fail: bool,
    /// Also cancelled inside the first seek when `cancel_in_first_seek` is set, and at
    /// the run's first `SubscriptionRejoining` when `cancel_at_first_rejoining` is set.
    cancel: CancellationToken,
    cancel_in_first_seek: bool,
    cancel_at_first_rejoining: bool,
}

impl SeekOrJoinFailsBroker {
    /// A broker on which nothing fails and no stream ends.
    fn new() -> Self {
        Self {
            joins: Mutex::new(0),
            seeks: Mutex::new(Vec::new()),
            leave_calls: Mutex::new(Vec::new()),
            join_fails: |_| false,
            seek_fails: |_| false,
            ending_streams: Vec::new(),
            streams_fail: false,
            cancel: CancellationToken::new(),
            cancel_in_first_seek: false,
            cancel_at_first_rejoining: false,
        }
    }
}

#[async_trait::async_trait]
impl BrokerStub for SeekOrJoinFailsBroker {
    async fn join(
        &self,
        _ctx: &SecurityContext,
        _req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let n = {
            let mut joins = self.joins.lock().unwrap();
            *joins += 1;
            *joins
        };
        if (self.join_fails)(n) {
            return Err(EventBrokerError::Transport("injected".to_owned()));
        }
        Ok(SubscriptionAssignment {
            id: joined_subscription(n),
            topology_version: 1,
            assigned: orders_partitions(&[0]),
            unmatched_types: Vec::new(),
        })
    }

    async fn seek(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
        _topology_version: i64,
        _positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        let n = {
            let mut seeks = self.seeks.lock().unwrap();
            seeks.push(id);
            seeks.len()
        };
        if n == 1 && self.cancel_in_first_seek {
            self.cancel.cancel();
        }
        if (self.seek_fails)(n) {
            return Err(EventBrokerError::Transport("injected".to_owned()));
        }
        Ok(Vec::new())
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        self.leave_calls.lock().unwrap().push(id);
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        if self.streams_fail {
            return Err(EventBrokerError::Transport("injected".to_owned()));
        }
        let heartbeat = futures_util::stream::iter([Ok(WireFrame::Heartbeat {
            at: "2026-11-07T00:00:00Z".to_owned(),
        })]);
        if self.ending_streams.contains(&id) {
            return Ok(Box::pin(heartbeat));
        }
        let cancel = self.cancel.clone();
        Ok(Box::pin(futures_util::StreamExt::chain(
            heartbeat,
            futures_util::stream::once(async move {
                cancel.cancel();
                std::future::pending::<Result<WireFrame, EventBrokerError>>().await
            }),
        )))
    }
}

/// Which slot runs on a `SeekOrJoinFailsBroker`.
enum SlotKind {
    Plain,
    #[cfg(feature = "db")]
    Transactional,
}

/// What a slot run on a `SeekOrJoinFailsBroker` did.
struct SeekOrJoinRun {
    result: Result<(), EventBrokerError>,
    joins: u128,
    seeks: Vec<SubscriptionId>,
    leaves: Vec<SubscriptionId>,
    /// The `(consumer_group, previous_subscription)` of each `SubscriptionRejoining`,
    /// in order. The re-JOIN loop is the event's only emitter.
    rejoining: Vec<(ConsumerGroupId, SubscriptionId)>,
    warns: Vec<String>,
}

/// Cancels `cancel` at a `SubscriptionRejoining`. The re-JOIN loop emits that event
/// before its backoff, so the backoff sees the stop at once.
struct CancelOnRejoiningListener {
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl ConsumerRuntimeListener for CancelOnRejoiningListener {
    async fn on_consumer_event(&self, event: &ConsumerRuntimeEvent) -> Result<(), ConsumerError> {
        if matches!(event, ConsumerRuntimeEvent::SubscriptionRejoining { .. }) {
            self.cancel.cancel();
        }
        Ok(())
    }
}

/// Runs a `kind` slot on `broker` with the `slot_settings` budget
/// (`max_rejoin_attempts: 2`) and `broker.cancel` as the run's token, within `BOUND`.
async fn seek_or_join_run(broker: SeekOrJoinFailsBroker, kind: SlotKind) -> SeekOrJoinRun {
    let capture = WarnCapture::install();
    let broker = Arc::new(broker);
    let listener = RecordingRuntimeListener::default();
    let events = listener.events.clone();
    let mut settings = listened_slot_settings(broker.clone(), listener);
    if broker.cancel_at_first_rejoining {
        settings.listeners.push(Arc::new(CancelOnRejoiningListener {
            cancel: broker.cancel.clone(),
        }));
    }
    let ctx = Arc::new(SecurityContext::anonymous());
    let cancel = broker.cancel.clone();
    let result = match kind {
        SlotKind::Plain => {
            let dispatcher = SlotDispatcher {
                settings,
                offset_manager: Arc::new(InMemoryOffsetManager::new(Fallback::Earliest)),
                handler: Arc::new(BatchHandlerAdapter::new(AckAllBatchHandler {
                    calls: SharedPartitionCalls::default(),
                })),
                commit_mode: ConsumerCommitMode::Manual,
            };
            within_bound(dispatcher.run(ctx, cancel)).await
        }
        #[cfg(feature = "db")]
        SlotKind::Transactional => {
            let dispatcher = TxSlotDispatcher {
                settings,
                offset_manager: Arc::new(NoopTxOffsets),
                handler: Arc::new(UnusedTxHandler),
            };
            within_bound(dispatcher.run(ctx, cancel)).await
        }
    };
    let rejoining = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            ConsumerRuntimeEvent::SubscriptionRejoining {
                consumer_group,
                previous_subscription,
            } => Some((*consumer_group, *previous_subscription)),
            _ => None,
        })
        .collect();
    SeekOrJoinRun {
        result,
        joins: *broker.joins.lock().unwrap(),
        seeks: broker.seeks.lock().unwrap().clone(),
        leaves: broker.leave_calls.lock().unwrap().clone(),
        rejoining,
        warns: capture.lines(),
    }
}

/// The group every `slot_settings` slot joins.
const SLOT_GROUP: ConsumerGroupId = ConsumerGroupId(Uuid::from_u128(0x1801));

/// A SEEK that fails right after the run's first JOIN leaves that subscription and
/// re-JOINs within the re-JOIN budget; the new subscription seeks, streams, and is left
/// on the way out.
#[tokio::test]
async fn a_failed_first_seek_leaves_and_joins_again() {
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            seek_fails: |n| n == 1,
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Plain,
    )
    .await;

    let Ok(()) = run.result else {
        panic!(
            "expected the run to end cleanly at the cancel, got {:?}",
            run.result
        );
    };
    assert_eq!(run.joins, 2);
    assert_eq!(
        run.seeks,
        vec![joined_subscription(1), joined_subscription(2)]
    );
    assert_eq!(
        run.leaves,
        vec![joined_subscription(1), joined_subscription(2)]
    );
    assert_eq!(run.rejoining, vec![(SLOT_GROUP, joined_subscription(1))]);
    assert_eq!(
        run.warns,
        vec!["WARN seek after JOIN failed; re-joining slot_idx=0 error=transport: injected"]
    );
}

/// The transactional slot shares the re-JOIN loop: a failed first SEEK leaves and
/// re-JOINs, with the transactional warn.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_failed_first_seek_leaves_and_joins_again_in_a_transactional_slot() {
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            seek_fails: |n| n == 1,
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Transactional,
    )
    .await;

    let Ok(()) = run.result else {
        panic!(
            "expected the run to end cleanly at the cancel, got {:?}",
            run.result
        );
    };
    assert_eq!(run.joins, 2);
    assert_eq!(
        run.seeks,
        vec![joined_subscription(1), joined_subscription(2)]
    );
    assert_eq!(
        run.leaves,
        vec![joined_subscription(1), joined_subscription(2)]
    );
    assert_eq!(run.rejoining, vec![(SLOT_GROUP, joined_subscription(1))]);
    assert_eq!(
        run.warns,
        vec![
            "WARN transactional seek after JOIN failed; re-joining slot_idx=0 \
             error=transport: injected"
        ]
    );
}

/// A SEEK that fails after a re-JOIN leaves that subscription and re-JOINs again. The
/// first subscription's stream ended without a drain, so it is never left: the slot
/// closes the stream instead.
#[tokio::test]
async fn a_failed_seek_after_a_rejoin_joins_again() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            seek_fails: |n| n == 2,
            ending_streams: vec![joined_subscription(1)],
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Plain,
    )
    .await;

    let Ok(()) = run.result else {
        panic!(
            "expected the run to end cleanly at the cancel, got {:?}",
            run.result
        );
    };
    assert_eq!(run.joins, 3);
    assert_eq!(
        run.seeks,
        vec![
            joined_subscription(1),
            joined_subscription(2),
            joined_subscription(3),
        ]
    );
    assert_eq!(
        run.leaves,
        vec![joined_subscription(2), joined_subscription(3)]
    );
    assert_eq!(
        run.rejoining,
        vec![
            (SLOT_GROUP, joined_subscription(1)),
            (SLOT_GROUP, joined_subscription(2)),
        ]
    );
    assert_eq!(
        run.warns,
        vec!["WARN seek after JOIN failed; re-joining slot_idx=0 error=transport: injected"]
    );
}

/// A re-JOIN that fails is tried again after the next backoff, within the same budget.
/// Nothing was joined, so nothing is left, and the next attempt still re-joins from the
/// subscription whose stream ended.
#[tokio::test]
async fn a_failed_rejoin_joins_again() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            join_fails: |n| n == 2,
            ending_streams: vec![joined_subscription(1)],
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Plain,
    )
    .await;

    let Ok(()) = run.result else {
        panic!(
            "expected the run to end cleanly at the cancel, got {:?}",
            run.result
        );
    };
    assert_eq!(run.joins, 3);
    assert_eq!(
        run.seeks,
        vec![joined_subscription(1), joined_subscription(3)]
    );
    assert_eq!(run.leaves, vec![joined_subscription(3)]);
    assert_eq!(
        run.rejoining,
        vec![
            (SLOT_GROUP, joined_subscription(1)),
            (SLOT_GROUP, joined_subscription(1)),
        ]
    );
    assert_eq!(
        run.warns,
        vec!["WARN re-JOIN failed; trying again slot_idx=0 error=transport: injected"]
    );
}

/// A failed SEEK counts against the re-JOIN budget like a failed open: with
/// `max_rejoin_attempts: 2` the third failed SEEK ends the run, each subscription left
/// first.
#[tokio::test]
async fn a_seek_that_fails_every_time_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            seek_fails: |_| true,
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Plain,
    )
    .await;

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = &run.result
    else {
        panic!(
            "expected SubscriptionRecoveryExhausted, got {:?}",
            run.result
        );
    };
    assert_eq!(
        (*attempts, detail.as_str()),
        (3, "max re-JOIN attempts exceeded")
    );
    assert_eq!(run.joins, 3);
    let subscriptions = vec![
        joined_subscription(1),
        joined_subscription(2),
        joined_subscription(3),
    ];
    assert_eq!(run.seeks, subscriptions);
    assert_eq!(run.leaves, subscriptions);
    assert_eq!(
        run.rejoining,
        vec![
            (SLOT_GROUP, joined_subscription(1)),
            (SLOT_GROUP, joined_subscription(2)),
        ]
    );
    let warn = "WARN seek after JOIN failed; re-joining slot_idx=0 error=transport: injected";
    assert_eq!(run.warns, vec![warn, warn, warn]);
}

/// The re-JOIN budget is the run's from its start, not only from its stream loop: a
/// failed first SEEK spends one attempt, and since no stream opens after it, each
/// failed stream open spends another. With `max_rejoin_attempts: 2` the second failed
/// open ends the run, after three JOINs, each subscription left first.
#[tokio::test]
async fn a_failed_first_seek_and_failed_opens_share_one_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            seek_fails: |n| n == 1,
            streams_fail: true,
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Plain,
    )
    .await;

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = &run.result
    else {
        panic!(
            "expected SubscriptionRecoveryExhausted, got {:?}",
            run.result
        );
    };
    assert_eq!(
        (*attempts, detail.as_str()),
        (3, "max re-JOIN attempts exceeded")
    );
    assert_eq!(run.joins, 3);
    let subscriptions = vec![
        joined_subscription(1),
        joined_subscription(2),
        joined_subscription(3),
    ];
    assert_eq!(run.seeks, subscriptions);
    assert_eq!(run.leaves, subscriptions);
    assert_eq!(
        run.rejoining,
        vec![
            (SLOT_GROUP, joined_subscription(1)),
            (SLOT_GROUP, joined_subscription(2)),
        ]
    );
    let open_failed = "WARN stream open failed; re-joining slot_idx=0 error=transport: injected";
    assert_eq!(
        run.warns,
        vec![
            "WARN seek after JOIN failed; re-joining slot_idx=0 error=transport: injected",
            open_failed,
            open_failed,
        ]
    );
}

/// A stop while the slot waits out its re-JOIN backoff ends the run there, without
/// another JOIN. The one subscription the run joined was left when its SEEK failed, so
/// the run leaves nothing more on the way out. On the paused clock a backoff waited out
/// would advance time by 250 ms; the stop ends the wait at once, so no time passes.
#[tokio::test(start_paused = true)]
async fn a_stop_during_the_rejoin_backoff_ends_the_run_without_another_join() {
    let started = tokio::time::Instant::now();
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            seek_fails: |_| true,
            cancel_in_first_seek: true,
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Plain,
    )
    .await;

    let Ok(()) = run.result else {
        panic!(
            "expected the stopped run to end cleanly, got {:?}",
            run.result
        );
    };
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(run.joins, 1);
    assert_eq!(run.seeks, vec![joined_subscription(1)]);
    assert_eq!(run.leaves, vec![joined_subscription(1)]);
    assert_eq!(run.rejoining, vec![(SLOT_GROUP, joined_subscription(1))]);
    assert_eq!(
        run.warns,
        vec!["WARN seek after JOIN failed; re-joining slot_idx=0 error=transport: injected"]
    );
}

/// A re-JOIN that fails every time spends the re-JOIN budget like a failed open or SEEK:
/// with `max_rejoin_attempts: 2` the third attempt ends the run, after two failed JOINs
/// that each re-join from the subscription whose stream ended. That stream is closed
/// but its subscription not left, so the run leaves it on the way out.
#[tokio::test]
async fn a_rejoin_that_fails_every_time_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            join_fails: |n| n >= 2,
            ending_streams: vec![joined_subscription(1)],
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Plain,
    )
    .await;

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = &run.result
    else {
        panic!(
            "expected SubscriptionRecoveryExhausted, got {:?}",
            run.result
        );
    };
    assert_eq!(
        (*attempts, detail.as_str()),
        (3, "max re-JOIN attempts exceeded")
    );
    assert_eq!(run.joins, 3);
    assert_eq!(run.seeks, vec![joined_subscription(1)]);
    assert_eq!(run.leaves, vec![joined_subscription(1)]);
    assert_eq!(
        run.rejoining,
        vec![
            (SLOT_GROUP, joined_subscription(1)),
            (SLOT_GROUP, joined_subscription(1)),
        ]
    );
    let warn = "WARN re-JOIN failed; trying again slot_idx=0 error=transport: injected";
    assert_eq!(run.warns, vec![warn, warn]);
}

/// A stop during the re-JOIN backoff after a stream ended ends the run without another
/// JOIN. That stream is closed but its subscription not left, so the run leaves it on
/// the way out.
#[tokio::test]
async fn a_stop_during_the_rejoin_backoff_after_a_stream_end_leaves_the_ended_subscription() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let run = seek_or_join_run(
        SeekOrJoinFailsBroker {
            ending_streams: vec![joined_subscription(1)],
            cancel_at_first_rejoining: true,
            ..SeekOrJoinFailsBroker::new()
        },
        SlotKind::Plain,
    )
    .await;

    let Ok(()) = run.result else {
        panic!(
            "expected the stopped run to end cleanly, got {:?}",
            run.result
        );
    };
    assert_eq!(run.joins, 1);
    assert_eq!(run.seeks, vec![joined_subscription(1)]);
    assert_eq!(run.leaves, vec![joined_subscription(1)]);
    assert_eq!(run.rejoining, vec![(SLOT_GROUP, joined_subscription(1))]);
    assert_eq!(run.warns, Vec::<String>::new());
}

/// A stop during the re-JOIN backoff after a failed open ends the run without another
/// JOIN. The failed open left its subscription before the re-JOIN, so the run leaves
/// nothing more on the way out.
#[tokio::test]
async fn a_stop_during_the_rejoin_backoff_after_a_failed_open_leaves_once() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let capture = WarnCapture::install();
    let cancel = CancellationToken::new();
    let broker = Arc::new(OpenFailsBeforeFirstFrameBroker::default());
    let listener = RecordingRuntimeListener::default();
    let events = listener.events.clone();
    let mut dispatcher = slot_dispatcher(broker.clone());
    dispatcher.settings.listeners.push(Arc::new(listener));
    dispatcher
        .settings
        .listeners
        .push(Arc::new(CancelOnRejoiningListener {
            cancel: cancel.clone(),
        }));

    let result = within_bound(dispatcher.run(Arc::new(SecurityContext::anonymous()), cancel)).await;

    let Ok(()) = result else {
        panic!("expected the stopped run to end cleanly, got {result:?}");
    };
    assert_eq!(*broker.joins.lock().unwrap(), 1);
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(1)]
    );
    let rejoining: Vec<(ConsumerGroupId, SubscriptionId)> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            ConsumerRuntimeEvent::SubscriptionRejoining {
                consumer_group,
                previous_subscription,
            } => Some((*consumer_group, *previous_subscription)),
            _ => None,
        })
        .collect();
    assert_eq!(rejoining, vec![(SLOT_GROUP, joined_subscription(1))]);
    assert_eq!(
        capture.lines(),
        vec!["WARN stream open failed; re-joining slot_idx=0 error=transport: injected"]
    );
}

/// A batch the handler's outcome cannot apply to drops the stream: the slot drains,
/// leaves that subscription and re-JOINs. When every re-JOIN fails, the third attempt
/// ends the run, and the subscription the drain left is not left a second time.
#[tokio::test]
async fn a_rejoin_budget_spent_after_a_drain_leaves_the_drained_subscription_once() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending #5819.
    let capture = WarnCapture::install();
    let broker = Arc::new(OneEventBroker::failing_every_rejoin());
    let calls = SharedNamedOffsets::default();
    let dispatcher = SlotDispatcher {
        settings: slot_settings(broker.clone()),
        offset_manager: Arc::new(InMemoryOffsetManager::new(Fallback::Earliest)),
        handler: Arc::new(BatchHandlerAdapter::new(RecordingBatchHandler {
            name: "unusable",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::AdvanceThrough {
                offset: Sequence::assigned(99),
            },
        })),
        commit_mode: ConsumerCommitMode::Manual,
    };

    let result = within_bound(dispatcher.run(
        Arc::new(SecurityContext::anonymous()),
        CancellationToken::new(),
    ))
    .await;

    let Err(EventBrokerError::SubscriptionRecoveryExhausted { attempts, detail }) = result else {
        panic!("expected SubscriptionRecoveryExhausted, got {result:?}");
    };
    assert_eq!(
        (attempts, detail.as_str()),
        (3, "max re-JOIN attempts exceeded")
    );
    assert_eq!(*calls.lock().unwrap(), vec![("unusable", 10)]);
    assert_eq!(*broker.joins.lock().unwrap(), 3);
    assert_eq!(
        *broker.leave_calls.lock().unwrap(),
        vec![joined_subscription(1)]
    );
    let warn = "WARN re-JOIN failed; trying again slot_idx=0 error=transport: injected";
    assert_eq!(capture.lines(), vec![warn, warn]);
}
