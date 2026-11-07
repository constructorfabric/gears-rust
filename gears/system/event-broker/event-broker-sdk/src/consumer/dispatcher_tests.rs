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
    Buffered, PartitionSlowState, SlotDispatcher, SlowConsumerReason, TopicPartitionKey,
    enqueue_partition_batch,
};
use super::runtime::RoutedBatchHandler;
use super::type_cache::ConsumerTypeCache;
#[cfg(feature = "db")]
use super::types::DeliverTxBatch;
use super::types::{BatchHandlerAdapter, DeliverBatch, EventHandlerAdapter};
use super::{
    BatchHandler, BatchHandlerOutcome, Delivery, EventBatch, EventHandler, HandlerOutcome,
};
#[cfg(feature = "db")]
use super::{CommitOffsetInTx, OffsetStore, Position, TxCommitHandle};
use super::{ConsumerCommitMode, ConsumerGroupRef, Fallback, InMemoryOffsetManager};
use crate::api::{
    BarrierMode, EventBrokerApi, FrameStream, JoinRequest, SubscriptionAssignment,
    TenantTraversalDepth, WireFrame,
};
use crate::broker_stub::BrokerStub;
use crate::error::ConsumerError;
use crate::error::EventBrokerError;
#[cfg(feature = "db")]
use crate::error::OffsetManagerError;
use crate::gts::{EventV1, StoredPosition};
use crate::ids::TopicId;
use crate::ids::{ConsumerGroupId, SubscriptionId};
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
        .tenant_id(Uuid::nil())
        .subject(format!("event-{offset}"))
        .subject_type_id(gtype(SUBJECT_TYPE))
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
        Ok(chunk
            .last()
            .map(|event| BatchHandlerOutcome::AdvanceThrough {
                offset: event.sequence().expect("a delivered event is stamped"),
            })
            .unwrap_or(BatchHandlerOutcome::Success))
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
    let topic_id = TopicId::from_gts(topic);
    let partition_zero = TopicPartitionKey::new(ginst(topic), topic_id, 0);
    let partition_one = TopicPartitionKey::new(ginst(topic), topic_id, 1);

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
            subscription_id: joined_subscription(*joins),
            topology_version: 1,
            assigned: Vec::new(),
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
            subscription_id: joined_subscription(*joins),
            topology_version: 1,
            assigned: Vec::new(),
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

/// A slot on `broker` with `max_rejoin_attempts: 2`, joining a fixed group.
fn slot_dispatcher(
    broker: Arc<dyn EventBrokerApi>,
) -> SlotDispatcher<BatchHandlerAdapter<AckAllBatchHandler>, InMemoryOffsetManager> {
    SlotDispatcher {
        slot_idx: 0,
        broker,
        type_cache: Arc::new(ConsumerTypeCache::default()),
        offset_manager: Arc::new(InMemoryOffsetManager::new(Fallback::Earliest)),
        handler: Arc::new(BatchHandlerAdapter::new(AckAllBatchHandler {
            calls: SharedPartitionCalls::default(),
        })),
        group_ref: ConsumerGroupRef::Id(ConsumerGroupId(Uuid::from_u128(0x1801))),
        topics: vec![ginst(ORDERS_TOPIC)],
        subscription_interests: Vec::new(),
        tenant_id: None,
        tenant_depth: TenantTraversalDepth::default(),
        barrier_mode: BarrierMode::default(),
        event_type_patterns: Vec::new(),
        client_agent: "budget-test".to_owned(),
        session_timeout: None,
        filter: None,
        heartbeat_drop_threshold: 3,
        retry_base: Duration::from_millis(10),
        retry_max: Duration::from_millis(10),
        commit_mode: ConsumerCommitMode::Manual,
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

/// A stream that fails before its first frame never opened, so it does not reset
/// the re-JOIN budget: with `max_rejoin_attempts: 2` the third failed open ends the
/// run. Each failed open leaves its subscription first.
#[tokio::test]
async fn a_stream_that_fails_before_its_first_frame_every_time_exhausts_the_rejoin_budget() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
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
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
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
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
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
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
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

/// An offset store the transactional slot tests never reach: nothing is assigned,
/// so nothing is loaded or committed.
#[cfg(feature = "db")]
struct UnusedTxOffsets;

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl OffsetStore for UnusedTxOffsets {
    async fn load_position(
        &self,
        _group: &ConsumerGroupId,
        _topic: &TopicId,
        _partition: u32,
    ) -> Result<Position, OffsetManagerError> {
        Ok(Fallback::Earliest.into())
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl CommitOffsetInTx for UnusedTxOffsets {
    async fn commit_in_tx<TX>(
        &self,
        _txn: &TX,
        _group: &ConsumerGroupId,
        _topic: &TopicId,
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
impl DeliverTxBatch<UnusedTxOffsets> for UnusedTxHandler {
    async fn deliver(
        &self,
        _batch: &EventBatch<'_, serde_json::Value>,
        _positions: &[StoredPosition],
        _delivery: &Delivery,
        _commit: TxCommitHandle<UnusedTxOffsets>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

/// The transactional twin of `slot_dispatcher`.
#[cfg(feature = "db")]
fn tx_slot_dispatcher(
    broker: Arc<dyn EventBrokerApi>,
) -> TxSlotDispatcher<UnusedTxHandler, UnusedTxOffsets> {
    TxSlotDispatcher {
        slot_idx: 0,
        broker,
        type_cache: Arc::new(ConsumerTypeCache::default()),
        offset_manager: Arc::new(UnusedTxOffsets),
        handler: Arc::new(UnusedTxHandler),
        group_ref: ConsumerGroupRef::Id(ConsumerGroupId(Uuid::from_u128(0x1801))),
        topics: vec![ginst(ORDERS_TOPIC)],
        subscription_interests: Vec::new(),
        tenant_id: None,
        tenant_depth: TenantTraversalDepth::default(),
        barrier_mode: BarrierMode::default(),
        event_type_patterns: Vec::new(),
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

/// The transactional twin of
/// `a_stream_that_fails_before_its_first_frame_every_time_exhausts_the_rejoin_budget`.
#[cfg(feature = "db")]
#[tokio::test]
async fn a_transactional_stream_that_fails_before_its_first_frame_every_time_exhausts_the_rejoin_budget()
 {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
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
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
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
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
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
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
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
