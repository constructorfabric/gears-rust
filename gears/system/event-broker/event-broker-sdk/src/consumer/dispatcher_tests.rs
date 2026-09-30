use crate::sequence::Sequence;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use toolkit_gts::gts_id;

use chrono::Utc;
use tokio::sync::RwLock;
use uuid::Uuid;

use super::builder::{ConsumerRoute, ConsumerRouteHandlerKind};
use super::dispatcher::{
    PartitionSlowState, SlowConsumerReason, TopicPartitionKey, enqueue_partition_batch,
};
use super::runtime::RoutedBatchHandler;
use super::{
    BatchHandlerOutcome, ConsumerHandler, EventBatch, HandlerOutcome, RawEvent,
    SingleEventHandlerAdapter,
};
use crate::error::ConsumerError;
use crate::ids::TopicId;
use gts::{GtsIdPattern, GtsInstanceId, GtsTypeId};

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

fn raw_event(topic: &str, type_id: &str, offset: i64) -> RawEvent {
    raw_event_on(topic, type_id, 3, offset)
}

fn raw_event_on(topic: &str, type_id: &str, partition: u32, offset: i64) -> RawEvent {
    RawEvent {
        id: Uuid::new_v4(),
        type_id: gtype(type_id),
        topic: ginst(topic),
        tenant_id: Uuid::nil(),
        subject: format!("event-{offset}"),
        subject_type: gtype(SUBJECT_TYPE),
        partition,
        sequence: Sequence::assigned(offset),
        offset: Sequence::assigned(offset),
        occurred_at: Utc::now(),
        sequence_time: Utc::now(),
        trace_parent: None,
        data: serde_json::json!({ "offset": offset }),
    }
}

struct RecordingSingleHandler {
    calls: SharedOffsets,
    outcome: HandlerOutcome,
}

#[async_trait::async_trait]
impl super::SingleEventHandler for RecordingSingleHandler {
    async fn handle(
        &self,
        event: RawEvent,
        _attempts: u16,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push(event.offset.as_i64());
        Ok(self.outcome.clone())
    }
}

struct RecordingBatchHandler {
    name: &'static str,
    calls: SharedNamedOffsets,
    outcome: BatchHandlerOutcome,
}

#[async_trait::async_trait]
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

struct AckAllBatchHandler {
    calls: SharedPartitionCalls,
}

#[async_trait::async_trait]
impl ConsumerHandler for AckAllBatchHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_>,
        _attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        self.calls.lock().unwrap().extend(chunk.iter().map(|event| {
            (
                event.topic.as_ref().to_owned(),
                event.partition,
                event.offset.as_i64(),
            )
        }));
        Ok(chunk
            .last()
            .map(|event| BatchHandlerOutcome::AdvanceThrough {
                offset: event.offset,
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
        raw_event_on(topic, BUFFER_EVENT, 0, 10),
        1,
    )
    .await
    .expect("first event in partition 0 fits");
    assert_eq!(first_partition_zero.buffered_count, 1);

    let overflow_partition_zero = enqueue_partition_batch(
        &buffers,
        partition_zero,
        raw_event_on(topic, BUFFER_EVENT, 0, 11),
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
        raw_event_on(topic, BUFFER_EVENT, 1, 20),
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
    let handler = SingleEventHandlerAdapter::new(Arc::new(RecordingSingleHandler {
        calls: calls.clone(),
        outcome: HandlerOutcome::Success,
    }));
    let events = vec![
        raw_event_on(ORDERS_TOPIC, ORDER_CREATED, 7, 20),
        raw_event_on(ORDERS_TOPIC, ORDER_UPDATED, 7, 21),
    ];
    let batch = EventBatch::new(&events);

    let outcome = handler.handle_batch(&batch, 1).await.unwrap();

    assert_eq!(*calls.lock().unwrap(), vec![20]);
    assert!(matches!(
        outcome,
        BatchHandlerOutcome::AdvanceThrough { offset } if offset == Sequence::assigned(20)
    ));
    assert_eq!(
        batch.next_event().map(|event| event.offset),
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
        raw_event_on(ORDERS_TOPIC, ORDER_CREATED, 5, 30),
        raw_event_on(ORDERS_TOPIC, ORDER_UPDATED, 5, 31),
        raw_event_on(ORDERS_TOPIC, ORDER_CLOSED, 5, 32),
    ];
    let batch = EventBatch::new(&events);

    let outcome = handler.handle_batch(&batch, 1).await.unwrap();

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
        batch.next_event().map(|event| event.offset),
        Some(Sequence::assigned(30))
    );
}

#[tokio::test]
async fn routed_dispatch_prefers_exact_topic_and_event_type_route() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let routed = RoutedBatchHandler::new(
        Some(Arc::new(RecordingBatchHandler {
            name: "default",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })),
        vec![route(ORDERS_TOPIC, Some(ORDER_CREATED))],
        vec![Arc::new(RecordingBatchHandler {
            name: "orders-created",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })],
    )
    .unwrap();
    let events = vec![raw_event(ORDERS_TOPIC, ORDER_CREATED, 10)];
    let batch = EventBatch::new(&events);

    let outcome = routed.handle_batch(&batch, 1).await.unwrap();

    assert_eq!(*calls.lock().unwrap(), vec![("orders-created", 10)]);
    assert!(matches!(outcome, BatchHandlerOutcome::Success));
}

#[tokio::test]
async fn routed_dispatch_uses_topic_catch_all_before_default_handler() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let routed = RoutedBatchHandler::new(
        Some(Arc::new(RecordingBatchHandler {
            name: "default",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })),
        vec![route(ORDERS_TOPIC, None)],
        vec![Arc::new(RecordingBatchHandler {
            name: "orders-any",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })],
    )
    .unwrap();
    let events = vec![raw_event(ORDERS_TOPIC, ORDER_CANCELLED, 11)];
    let batch = EventBatch::new(&events);

    let outcome = routed.handle_batch(&batch, 1).await.unwrap();

    assert_eq!(*calls.lock().unwrap(), vec![("orders-any", 11)]);
    assert!(matches!(outcome, BatchHandlerOutcome::Success));
}

#[tokio::test]
async fn routed_dispatch_falls_back_to_default_handler() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let routed = RoutedBatchHandler::new(
        Some(Arc::new(RecordingBatchHandler {
            name: "default",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })),
        vec![route(PAYMENTS_TOPIC, None)],
        vec![Arc::new(RecordingBatchHandler {
            name: "payments-any",
            calls: calls.clone(),
            outcome: BatchHandlerOutcome::Success,
        })],
    )
    .unwrap();
    let events = vec![raw_event(ORDERS_TOPIC, ORDER_CANCELLED, 12)];
    let batch = EventBatch::new(&events);

    let outcome = routed.handle_batch(&batch, 1).await.unwrap();

    assert_eq!(*calls.lock().unwrap(), vec![("default", 12)]);
    assert!(matches!(outcome, BatchHandlerOutcome::Success));
}

#[tokio::test]
async fn routed_dispatch_fails_visibly_without_matching_route_or_default() {
    let routed = RoutedBatchHandler::new(
        None,
        vec![route(PAYMENTS_TOPIC, None)],
        vec![Arc::new(RecordingBatchHandler {
            name: "payments-any",
            calls: Arc::new(Mutex::new(Vec::new())),
            outcome: BatchHandlerOutcome::Success,
        })],
    )
    .unwrap();
    let events = vec![raw_event(ORDERS_TOPIC, ORDER_CANCELLED, 13)];
    let batch = EventBatch::new(&events);

    let err = routed.handle_batch(&batch, 1).await.unwrap_err();

    assert!(
        err.to_string().contains("no consumer route matched"),
        "unexpected error: {err:?}"
    );
    assert_eq!(
        batch.next_event().map(|event| event.offset),
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
            Arc::new(RecordingBatchHandler {
                name: "created",
                calls: calls.clone(),
                outcome: BatchHandlerOutcome::Success,
            }),
            Arc::new(RecordingBatchHandler {
                name: "cancelled",
                calls: calls.clone(),
                outcome: BatchHandlerOutcome::Success,
            }),
        ],
    )
    .unwrap();
    let events = vec![
        raw_event_on(ORDERS_TOPIC, ORDER_CREATED, 3, 40),
        raw_event_on(ORDERS_TOPIC, ORDER_CANCELLED, 3, 41),
    ];
    let batch = EventBatch::new(&events);
    let second_batch = EventBatch::new(&events[1..]);

    routed.handle_batch(&batch, 1).await.unwrap();
    routed.handle_batch(&second_batch, 1).await.unwrap();

    assert_eq!(
        *calls.lock().unwrap(),
        vec![("created", 40), ("cancelled", 41)]
    );
    assert_eq!(
        batch.next_event().map(|event| event.offset),
        Some(Sequence::assigned(40))
    );
    assert_eq!(
        second_batch.next_event().map(|event| event.offset),
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
            Arc::new(RecordingBatchHandler {
                name: "created",
                calls: calls.clone(),
                outcome: BatchHandlerOutcome::Retry {
                    reason: "try later".to_owned(),
                },
            }),
            Arc::new(RecordingBatchHandler {
                name: "cancelled",
                calls: calls.clone(),
                outcome: BatchHandlerOutcome::Success,
            }),
        ],
    )
    .unwrap();
    let events = vec![
        raw_event_on(ORDERS_TOPIC, ORDER_CREATED, 3, 50),
        raw_event_on(ORDERS_TOPIC, ORDER_CANCELLED, 3, 51),
    ];
    let batch = EventBatch::new(&events);

    let outcome = routed.handle_batch(&batch, 1).await.unwrap();

    assert!(matches!(outcome, BatchHandlerOutcome::Retry { .. }));
    assert_eq!(*calls.lock().unwrap(), vec![("created", 50)]);
    assert_eq!(
        batch.next_event().map(|event| event.offset),
        Some(Sequence::assigned(50))
    );
}
