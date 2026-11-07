//! The typed routes: the single-event and batch adapters that decode delivered events
//! as the route's payload type and apply its [`Undecodable`] policy, and the route
//! builder that sets the type and the policy.

use std::sync::{Arc, Mutex};

use gts::{GtsIdPattern, GtsInstanceId, GtsSchema, GtsTypeId};
use serde::{Deserialize, Serialize};
use toolkit_gts::gts_id;
use uuid::Uuid;

use super::types::{
    DeliverBatch, Delivered, SkippedEvent, TypedBatchHandlerAdapter, TypedEventHandlerAdapter,
};
use super::{
    BatchHandler, BatchHandlerOutcome, ConsumerBuilder, ConsumerGroupRef, ConsumerRoute,
    ConsumerRouteHandlerKind, Delivery, EventBatch, EventHandler, Fallback, HandlerOutcome,
    InMemoryOffsetManager, Undecodable,
};
use crate::error::{ConsumerError, DecodeCategory, EventBrokerError};
use crate::gts::{EventV1, StoredPosition};
use crate::sequence::Sequence;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.orders.x.x.v1");
const ORDER_PLACED: &str = gts_id!("cf.core.events.event.v1~example.orders.order_placed.x.v1~");
const ORDER_CREATED: &str = gts_id!("cf.core.events.event.v1~example.orders.order_created.x.v1~");

/// The typed payload of the route under test; `order_id` and `quantity` are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OrderPlacedV1 {
    order_id: String,
    quantity: u32,
}

impl GtsSchema for OrderPlacedV1 {
    const TYPE_ID: &'static str = ORDER_PLACED;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

fn ginst(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("valid GTS instance id")
}

fn gpat(id: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(id).expect("valid GTS pattern")
}

/// Partition 7 at `sequence`.
fn position_at(sequence: i64) -> StoredPosition {
    StoredPosition {
        partition: 7,
        sequence: Sequence::assigned(sequence),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    }
}

fn position() -> StoredPosition {
    position_at(40)
}

/// An `ORDER_PLACED` event with id `id` carrying `data`, stamped at `sequence`.
fn order_event(id: u128, sequence: i64, data: serde_json::Value) -> EventV1 {
    EventV1::builder_for(GtsTypeId::new(ORDER_PLACED), data)
        .id(Uuid::from_u128(id))
        .tenant_id(Uuid::nil())
        .subject("order-1")
        .subject_type_id(GtsTypeId::new("gts.x.eb.test.subject.v1~"))
        .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
        .build()
        .unwrap()
        .stamp_position(position_at(sequence))
}

/// An `ORDER_PLACED` event with id 1 carrying `data`, stamped at [`position`].
fn placed_event(data: serde_json::Value) -> EventV1 {
    order_event(1, 40, data)
}

fn good_event() -> EventV1 {
    placed_event(serde_json::json!({ "order_id": "o-1", "quantity": 3 }))
}

/// Misses the required `quantity`, so it does not decode as `OrderPlacedV1`.
fn bad_event() -> EventV1 {
    placed_event(serde_json::json!({ "order_id": "o-1" }))
}

/// Topic, partition and attempts of the delivery the adapter is called with.
fn delivery() -> Delivery {
    Delivery::new(ginst(TOPIC), 7, 2)
}

/// What a handler saw of one call: the event id, (raw handler only) its type, its data,
/// and the delivery's topic, partition and attempts.
type TypedCall = (Uuid, Option<OrderPlacedV1>, String, u32, u16);
type RawCall = (Uuid, GtsTypeId, Option<serde_json::Value>, String, u32, u16);

struct TypedRecorder {
    calls: Arc<Mutex<Vec<TypedCall>>>,
    outcome: Result<HandlerOutcome, ConsumerError>,
}

impl TypedRecorder {
    fn returning(outcome: Result<HandlerOutcome, ConsumerError>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outcome,
        }
    }
}

#[async_trait::async_trait]
impl EventHandler<OrderPlacedV1> for TypedRecorder {
    async fn handle(
        &self,
        event: EventV1<OrderPlacedV1>,
        delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push((
            event.id(),
            event.data().cloned(),
            delivery.topic().to_string(),
            delivery.partition(),
            delivery.attempts(),
        ));
        self.outcome.clone()
    }
}

struct RawRecorder {
    calls: Arc<Mutex<Vec<RawCall>>>,
    outcome: Result<HandlerOutcome, ConsumerError>,
}

impl RawRecorder {
    fn returning(outcome: Result<HandlerOutcome, ConsumerError>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outcome,
        }
    }
}

#[async_trait::async_trait]
impl EventHandler<serde_json::Value> for RawRecorder {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push((
            event.id(),
            event.event_type().clone(),
            event.data().cloned(),
            delivery.topic().to_string(),
            delivery.partition(),
            delivery.attempts(),
        ));
        self.outcome.clone()
    }
}

/// Calls the typed adapter for `policy` with a batch of `events`.
async fn deliver(
    handler: TypedRecorder,
    policy: Undecodable,
    events: &[EventV1],
) -> Result<Delivered, ConsumerError> {
    let positions: Vec<StoredPosition> = events.iter().map(|_| position()).collect();
    TypedEventHandlerAdapter::<_, OrderPlacedV1>::new(handler, policy)
        .deliver(&EventBatch::new(events), &positions, &delivery())
        .await
}

fn not_yet() -> HandlerOutcome {
    HandlerOutcome::Retry {
        reason: "not yet".to_owned(),
    }
}

#[tokio::test]
async fn a_decoded_event_that_succeeds_advances_through_it() {
    let handler = TypedRecorder::returning(Ok(HandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver(handler, Undecodable::Stall, &[good_event()])
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(40));
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(1),
            Some(OrderPlacedV1 {
                order_id: "o-1".to_owned(),
                quantity: 3,
            }),
            TOPIC.to_owned(),
            7,
            2,
        )]
    );
}

#[tokio::test]
async fn a_decoded_event_whose_handler_retries_is_retried() {
    let handler = TypedRecorder::returning(Ok(not_yet()));
    let calls = handler.calls.clone();

    let delivered = deliver(handler, Undecodable::Skip, &[good_event()])
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Retry { reason },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(reason, "not yet");
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(1),
            Some(OrderPlacedV1 {
                order_id: "o-1".to_owned(),
                quantity: 3,
            }),
            TOPIC.to_owned(),
            7,
            2,
        )]
    );
}

#[tokio::test]
async fn an_undecodable_event_on_a_stall_route_is_retried_without_the_handler() {
    let handler = TypedRecorder::returning(Ok(HandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver(handler, Undecodable::Stall, &[bad_event()])
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Retry { reason },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(
        reason,
        "event data of gts.cf.core.events.event.v1~example.orders.order_placed.x.v1~ \
         does not decode as the payload type (data)"
    );
    assert_eq!(*skipped, None);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn an_undecodable_event_on_a_skip_route_advances_through_it_and_reports_the_skip() {
    let handler = TypedRecorder::returning(Ok(HandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver(handler, Undecodable::Skip, &[bad_event()])
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(40));
    assert_eq!(
        *skipped,
        Some(SkippedEvent {
            event_id: Uuid::from_u128(1),
            sequence: Sequence::assigned(40),
            type_id: GtsTypeId::new(ORDER_PLACED),
            category: DecodeCategory::Data,
        })
    );
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn an_undecodable_event_on_a_fallback_route_that_succeeds_advances_through_it() {
    let handler = TypedRecorder::returning(Ok(HandlerOutcome::Success));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(HandlerOutcome::Success));
    let fallback_calls = fallback.calls.clone();

    let delivered = deliver(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &[bad_event()],
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(40));
    assert_eq!(*skipped, None);
    assert_eq!(*calls.lock().unwrap(), vec![]);
    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(1),
            GtsTypeId::new(ORDER_PLACED),
            Some(serde_json::json!({ "order_id": "o-1" })),
            TOPIC.to_owned(),
            7,
            2,
        )]
    );
}

#[tokio::test]
async fn an_undecodable_event_on_a_fallback_route_that_retries_is_retried() {
    let handler = TypedRecorder::returning(Ok(HandlerOutcome::Success));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(not_yet()));
    let fallback_calls = fallback.calls.clone();

    let delivered = deliver(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &[bad_event()],
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Retry { reason },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(reason, "not yet");
    assert_eq!(*skipped, None);
    assert_eq!(*calls.lock().unwrap(), vec![]);
    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(1),
            GtsTypeId::new(ORDER_PLACED),
            Some(serde_json::json!({ "order_id": "o-1" })),
            TOPIC.to_owned(),
            7,
            2,
        )]
    );
}

#[tokio::test]
async fn an_empty_batch_succeeds_without_calling_the_handler() {
    let handler = TypedRecorder::returning(Ok(HandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver(handler, Undecodable::Skip, &[]).await.unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Success,
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*skipped, None);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_typed_handler_error_passes_through() {
    let handler = TypedRecorder::returning(Err(EventBrokerError::Internal(
        "typed handler failed".to_owned(),
    )));

    let err = deliver(handler, Undecodable::Skip, &[good_event()])
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "internal: typed handler failed");
}

#[tokio::test]
async fn a_fallback_handler_error_passes_through() {
    let handler = TypedRecorder::returning(Ok(HandlerOutcome::Success));
    let fallback = RawRecorder::returning(Err(EventBrokerError::Internal(
        "fallback handler failed".to_owned(),
    )));

    let err = deliver(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &[bad_event()],
    )
    .await
    .unwrap_err();

    assert_eq!(err.to_string(), "internal: fallback handler failed");
}

#[tokio::test]
async fn a_body_less_event_reaches_the_typed_handler_without_data() {
    let handler = TypedRecorder::returning(Ok(HandlerOutcome::Success));
    let calls = handler.calls.clone();
    let event = EventV1::builder_without_data(GtsTypeId::new(ORDER_PLACED))
        .id(Uuid::from_u128(2))
        .tenant_id(Uuid::nil())
        .subject("order-2")
        .subject_type_id(GtsTypeId::new("gts.x.eb.test.subject.v1~"))
        .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
        .build()
        .unwrap()
        .stamp_position(position());

    let delivered = deliver(handler, Undecodable::Stall, &[event])
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(40));
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(Uuid::from_u128(2), None, TOPIC.to_owned(), 7, 2)]
    );
}

#[test]
fn undecodable_prints_its_variant_name_only() {
    let fallback = Undecodable::Fallback(Arc::new(RawRecorder::returning(Ok(
        HandlerOutcome::Success,
    ))));

    assert_eq!(format!("{:?}", Undecodable::Stall), "Stall");
    assert_eq!(format!("{:?}", Undecodable::Skip), "Skip");
    assert_eq!(format!("{fallback:?}"), "Fallback");
}

#[test]
fn a_typed_route_builder_prints_its_topic_type_and_policy() {
    let route = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("typed-route-debug"))
        .topics([ginst(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(ginst(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Skip);

    assert_eq!(
        format!("{route:?}"),
        "TypedRouteBuilder { \
         topic: \"gts.cf.core.events.topic.v1~example.orders.x.x.v1\", \
         event_type: \"gts.cf.core.events.event.v1~example.orders.order_placed.x.v1~\", \
         undecodable: Skip }"
    );
}

#[test]
fn a_typed_route_replaces_an_earlier_event_type_with_its_payload_type() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("typed-route-pattern"))
        .topics([ginst(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(ginst(TOPIC))
        .event_type(gpat(ORDER_CREATED))
        .event::<OrderPlacedV1>(Undecodable::Skip)
        .handler(TypedRecorder::returning(Ok(HandlerOutcome::Success)));

    assert_eq!(
        ready.routes,
        vec![ConsumerRoute {
            topic: ginst(TOPIC),
            event_type: Some(gpat(ORDER_PLACED)),
            handler_kind: ConsumerRouteHandlerKind::Single,
        }]
    );
}

/// An untyped handler on `serde_json::Value`, whose `GtsSchema::TYPE_ID` is empty.
struct ValueHandler;

#[async_trait::async_trait]
impl EventHandler<serde_json::Value> for ValueHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

/// A payload type whose id is no GTS type id cannot set a route's type, so `start()`
/// refuses the consumer before it calls the broker.
#[tokio::test]
async fn a_typed_route_whose_payload_type_id_is_not_a_gts_id_fails_start() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("typed-route-invalid"))
        .topics([ginst(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(ValueHandler)
        .route()
        .topic(ginst(TOPIC))
        .event::<serde_json::Value>(Undecodable::Skip)
        .handler(ValueHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("a typed route without a GTS type id must fail start"),
        Err(err) => err,
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        err.to_string(),
        "invalid consumer options: typed route payload type id \"\" is not a GTS type id: \
         Invalid GTS identifier: : must start with 'gts.'"
    );
}

// ---------------------------------------------------------------------------
// Typed batch routes: `TypedBatchHandlerAdapter` hands the decoded prefix of each
// batch to a `BatchHandler<P>` and applies the policy to the first event that does
// not decode.
// ---------------------------------------------------------------------------

/// The payload `good_at(id, ..)` carries.
fn order(id: u128) -> OrderPlacedV1 {
    OrderPlacedV1 {
        order_id: format!("o-{id}"),
        quantity: 3,
    }
}

/// An event with id `id` at `sequence` that decodes as [`order`]`(id)`.
fn good_at(id: u128, sequence: i64) -> EventV1 {
    order_event(
        id,
        sequence,
        serde_json::json!({ "order_id": format!("o-{id}"), "quantity": 3 }),
    )
}

/// An event with id `id` at `sequence` that misses the required `quantity`.
fn bad_at(id: u128, sequence: i64) -> EventV1 {
    order_event(
        id,
        sequence,
        serde_json::json!({ "order_id": format!("o-{id}") }),
    )
}

/// `[ok, ok, bad, ok]` at sequences 41 to 44: `k = 2`, with a tail after the bad one.
fn mixed_batch() -> Vec<EventV1> {
    vec![
        good_at(1, 41),
        good_at(2, 42),
        bad_at(3, 43),
        good_at(4, 44),
    ]
}

/// `[bad, ok]` at sequences 41 and 42: `k = 0`.
fn bad_first_batch() -> Vec<EventV1> {
    vec![bad_at(1, 41), good_at(2, 42)]
}

/// `[ok, ok, ok]` at sequences 41 to 43: `k = n`.
fn good_batch() -> Vec<EventV1> {
    vec![good_at(1, 41), good_at(2, 42), good_at(3, 43)]
}

/// What the batch handler saw of one call: each event's id and data, and the
/// delivery's topic, partition and attempts.
type BatchCall = (Vec<(Uuid, Option<OrderPlacedV1>)>, String, u32, u16);

struct BatchRecorder {
    calls: Arc<Mutex<Vec<BatchCall>>>,
    outcome: Result<BatchHandlerOutcome, ConsumerError>,
}

impl BatchRecorder {
    fn returning(outcome: Result<BatchHandlerOutcome, ConsumerError>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outcome,
        }
    }
}

#[async_trait::async_trait]
impl BatchHandler<OrderPlacedV1> for BatchRecorder {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, OrderPlacedV1>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push((
            batch
                .iter()
                .map(|event| (event.id(), event.data().cloned()))
                .collect(),
            delivery.topic().to_string(),
            delivery.partition(),
            delivery.attempts(),
        ));
        self.outcome.clone()
    }
}

/// Calls the typed batch adapter for `policy` with `events`, each at its own position.
async fn deliver_batch(
    handler: BatchRecorder,
    policy: Undecodable,
    events: &[EventV1],
) -> Result<Delivered, ConsumerError> {
    let positions: Vec<StoredPosition> = events
        .iter()
        .map(|event| StoredPosition {
            partition: 7,
            sequence: event.sequence().unwrap(),
            sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
        })
        .collect();
    TypedBatchHandlerAdapter::<_, OrderPlacedV1>::new(handler, policy)
        .deliver(&EventBatch::new(events), &positions, &delivery())
        .await
}

fn advance_through(sequence: i64) -> BatchHandlerOutcome {
    BatchHandlerOutcome::AdvanceThrough {
        offset: Sequence::assigned(sequence),
    }
}

fn batch_not_yet() -> BatchHandlerOutcome {
    BatchHandlerOutcome::Retry {
        reason: "not yet".to_owned(),
    }
}

#[tokio::test]
async fn an_empty_typed_batch_succeeds_without_calling_the_batch_handler() {
    let handler = BatchRecorder::returning(Ok(batch_not_yet()));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Stall, &[])
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Success,
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*skipped, None);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_fully_decoded_batch_goes_whole_to_the_handler_and_its_success_passes_through() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Skip, &good_batch())
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Success,
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2))),
                (Uuid::from_u128(3), Some(order(3)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_fully_decoded_batch_passes_the_batch_handlers_partial_advance_through() {
    let handler = BatchRecorder::returning(Ok(advance_through(42)));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Skip, &good_batch())
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(42));
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2))),
                (Uuid::from_u128(3), Some(order(3)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_fully_decoded_batch_passes_the_batch_handlers_retry_through() {
    let handler = BatchRecorder::returning(Ok(batch_not_yet()));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Skip, &good_batch())
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Retry { reason },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(reason, "not yet");
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2))),
                (Uuid::from_u128(3), Some(order(3)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_batch_handler_error_on_a_fully_decoded_batch_passes_through() {
    let handler = BatchRecorder::returning(Err(EventBrokerError::Internal(
        "batch handler failed".to_owned(),
    )));
    let calls = handler.calls.clone();

    let err = deliver_batch(handler, Undecodable::Skip, &good_batch())
        .await
        .unwrap_err();

    assert_eq!(err.to_string(), "internal: batch handler failed");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2))),
                (Uuid::from_u128(3), Some(order(3)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_typed_prefix_whose_handler_retries_is_retried_without_the_policy() {
    let handler = BatchRecorder::returning(Ok(batch_not_yet()));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(HandlerOutcome::Success));
    let fallback_calls = fallback.calls.clone();

    let delivered = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &mixed_batch(),
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Retry { reason },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(reason, "not yet");
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
    assert_eq!(*fallback_calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_typed_prefix_advanced_short_of_its_last_event_is_returned_without_the_policy() {
    let handler = BatchRecorder::returning(Ok(advance_through(41)));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(HandlerOutcome::Success));
    let fallback_calls = fallback.calls.clone();

    let delivered = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &mixed_batch(),
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(41));
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
    assert_eq!(*fallback_calls.lock().unwrap(), vec![]);
}

/// The typed handler never saw the undecodable event, so advancing onto it is
/// the handler's error, not progress.
#[tokio::test]
async fn an_advance_onto_the_undecodable_event_is_a_handler_error() {
    let handler = BatchRecorder::returning(Ok(advance_through(43)));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(HandlerOutcome::Success));
    let fallback_calls = fallback.calls.clone();

    let err = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &mixed_batch(),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        err.to_string(),
        "invalid consumer options: offset 43 is not present in delivered offsets \
         [Sequence(41), Sequence(42)]"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
    assert_eq!(*fallback_calls.lock().unwrap(), vec![]);
}

/// An offset that is in no delivered position is the handler's error too.
#[tokio::test]
async fn an_advance_to_an_offset_outside_the_batch_is_a_handler_error() {
    let handler = BatchRecorder::returning(Ok(advance_through(99)));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(HandlerOutcome::Success));
    let fallback_calls = fallback.calls.clone();

    let err = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &mixed_batch(),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        err.to_string(),
        "invalid consumer options: offset 99 is not present in delivered offsets \
         [Sequence(41), Sequence(42)]"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
    assert_eq!(*fallback_calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_stall_route_advances_through_a_handled_typed_prefix() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Stall, &mixed_batch())
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(42));
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_stall_route_retries_an_undecodable_first_event_without_the_batch_handler() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Stall, &bad_first_batch())
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Retry { reason },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(
        reason,
        "event data of gts.cf.core.events.event.v1~example.orders.order_placed.x.v1~ \
         does not decode as the payload type (data)"
    );
    assert_eq!(*skipped, None);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_skip_route_advances_past_the_undecodable_event_and_reports_it() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Skip, &mixed_batch())
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(43));
    assert_eq!(
        *skipped,
        Some(SkippedEvent {
            event_id: Uuid::from_u128(3),
            sequence: Sequence::assigned(43),
            type_id: GtsTypeId::new(ORDER_PLACED),
            category: DecodeCategory::Data,
        })
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_skip_route_goes_on_after_an_advance_through_the_whole_typed_prefix() {
    let handler = BatchRecorder::returning(Ok(advance_through(42)));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Skip, &mixed_batch())
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(43));
    assert_eq!(
        *skipped,
        Some(SkippedEvent {
            event_id: Uuid::from_u128(3),
            sequence: Sequence::assigned(43),
            type_id: GtsTypeId::new(ORDER_PLACED),
            category: DecodeCategory::Data,
        })
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_skip_route_skips_an_undecodable_first_event_without_the_batch_handler() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, Undecodable::Skip, &bad_first_batch())
        .await
        .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(41));
    assert_eq!(
        *skipped,
        Some(SkippedEvent {
            event_id: Uuid::from_u128(1),
            sequence: Sequence::assigned(41),
            type_id: GtsTypeId::new(ORDER_PLACED),
            category: DecodeCategory::Data,
        })
    );
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_fallback_that_succeeds_advances_past_the_undecodable_event() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(HandlerOutcome::Success));
    let fallback_calls = fallback.calls.clone();

    let delivered = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &mixed_batch(),
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(43));
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(3),
            GtsTypeId::new(ORDER_PLACED),
            Some(serde_json::json!({ "order_id": "o-3" })),
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_fallback_that_retries_after_a_typed_prefix_advances_through_the_prefix() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(not_yet()));
    let fallback_calls = fallback.calls.clone();

    let delivered = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &mixed_batch(),
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(42));
    assert_eq!(*skipped, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(3),
            GtsTypeId::new(ORDER_PLACED),
            Some(serde_json::json!({ "order_id": "o-3" })),
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_fallback_that_retries_an_undecodable_first_event_is_retried() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(not_yet()));
    let fallback_calls = fallback.calls.clone();

    let delivered = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &bad_first_batch(),
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Retry { reason },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(reason, "not yet");
    assert_eq!(*skipped, None);
    assert_eq!(*calls.lock().unwrap(), vec![]);
    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(1),
            GtsTypeId::new(ORDER_PLACED),
            Some(serde_json::json!({ "order_id": "o-1" })),
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_batch_handler_error_passes_through_without_the_policy() {
    let handler = BatchRecorder::returning(Err(EventBrokerError::Internal(
        "batch handler failed".to_owned(),
    )));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Ok(HandlerOutcome::Success));
    let fallback_calls = fallback.calls.clone();

    let err = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &mixed_batch(),
    )
    .await
    .unwrap_err();

    assert_eq!(err.to_string(), "internal: batch handler failed");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
    assert_eq!(*fallback_calls.lock().unwrap(), vec![]);
}

/// The error is reported and the whole batch retried, so the handled typed prefix is
/// delivered again (at-least-once).
#[tokio::test]
async fn a_fallback_error_after_a_typed_prefix_passes_through() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Err(EventBrokerError::Internal(
        "fallback handler failed".to_owned(),
    )));
    let fallback_calls = fallback.calls.clone();

    let err = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &mixed_batch(),
    )
    .await
    .unwrap_err();

    assert_eq!(err.to_string(), "internal: fallback handler failed");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![
                (Uuid::from_u128(1), Some(order(1))),
                (Uuid::from_u128(2), Some(order(2)))
            ],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(3),
            GtsTypeId::new(ORDER_PLACED),
            Some(serde_json::json!({ "order_id": "o-3" })),
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

#[tokio::test]
async fn a_fallback_error_on_an_undecodable_first_event_passes_through() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();
    let fallback = RawRecorder::returning(Err(EventBrokerError::Internal(
        "fallback handler failed".to_owned(),
    )));
    let fallback_calls = fallback.calls.clone();

    let err = deliver_batch(
        handler,
        Undecodable::Fallback(Arc::new(fallback)),
        &bad_first_batch(),
    )
    .await
    .unwrap_err();

    assert_eq!(err.to_string(), "internal: fallback handler failed");
    assert_eq!(*calls.lock().unwrap(), vec![]);
    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(
            Uuid::from_u128(1),
            GtsTypeId::new(ORDER_PLACED),
            Some(serde_json::json!({ "order_id": "o-1" })),
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

/// `[ok, bad, ok]`: decoding stops at the bad event, so the good event after it does
/// not join the typed batch.
#[tokio::test]
async fn the_batch_handler_gets_only_the_prefix_before_the_first_undecodable_event() {
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(
        handler,
        Undecodable::Skip,
        &[good_at(1, 41), bad_at(2, 42), good_at(3, 43)],
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::AdvanceThrough { offset },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(*offset, Sequence::assigned(42));
    assert_eq!(
        *skipped,
        Some(SkippedEvent {
            event_id: Uuid::from_u128(2),
            sequence: Sequence::assigned(42),
            type_id: GtsTypeId::new(ORDER_PLACED),
            category: DecodeCategory::Data,
        })
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec![(
            vec![(Uuid::from_u128(1), Some(order(1)))],
            TOPIC.to_owned(),
            7,
            2
        )]
    );
}

/// The stall retry reason names the type and the decode category, never the data.
#[tokio::test]
async fn the_stall_retry_reason_of_a_typed_batch_carries_no_payload_value() {
    let planted = serde_json::json!({ "order_id": "o-1", "quantity": "PLANTED-quantity-7" });
    let message = serde_json::from_value::<OrderPlacedV1>(planted.clone())
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("PLANTED-quantity-7"),
        "serde's message quotes the planted value: {message}"
    );
    let handler = BatchRecorder::returning(Ok(BatchHandlerOutcome::Success));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(
        handler,
        Undecodable::Stall,
        &[order_event(1, 41, planted), good_at(2, 42)],
    )
    .await
    .unwrap();

    let Delivered {
        outcome: BatchHandlerOutcome::Retry { reason },
        skipped,
    } = &delivered
    else {
        panic!("unexpected delivery result: {delivered:?}");
    };
    assert_eq!(
        reason,
        "event data of gts.cf.core.events.event.v1~example.orders.order_placed.x.v1~ \
         does not decode as the payload type (data)"
    );
    assert!(
        !reason.contains("PLANTED"),
        "a retry reason quotes the payload: {reason}"
    );
    assert_eq!(*skipped, None);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[test]
fn a_typed_batch_route_registers_a_batch_route_for_its_payload_type() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "typed-batch-route-pattern",
        ))
        .topics([ginst(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(ginst(TOPIC))
        .event_type(gpat(ORDER_CREATED))
        .event::<OrderPlacedV1>(Undecodable::Skip)
        .batch_handler(BatchRecorder::returning(Ok(BatchHandlerOutcome::Success)));

    assert_eq!(
        ready.routes,
        vec![ConsumerRoute {
            topic: ginst(TOPIC),
            event_type: Some(gpat(ORDER_PLACED)),
            handler_kind: ConsumerRouteHandlerKind::Batch,
        }]
    );
}

/// An untyped batch handler on `serde_json::Value`, whose `GtsSchema::TYPE_ID` is empty.
struct ValueBatchHandler;

#[async_trait::async_trait]
impl BatchHandler<serde_json::Value> for ValueBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        Ok(BatchHandlerOutcome::Success)
    }
}

/// As for a typed single-event route, a payload type whose id is no GTS type id
/// cannot set the route's type, so `start()` refuses the consumer.
#[tokio::test]
async fn a_typed_batch_route_whose_payload_type_id_is_not_a_gts_id_fails_start() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "typed-batch-route-invalid",
        ))
        .topics([ginst(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(ValueHandler)
        .route()
        .topic(ginst(TOPIC))
        .event::<serde_json::Value>(Undecodable::Skip)
        .batch_handler(ValueBatchHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("a typed batch route without a GTS type id must fail start"),
        Err(err) => err,
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        err.to_string(),
        "invalid consumer options: typed route payload type id \"\" is not a GTS type id: \
         Invalid GTS identifier: : must start with 'gts.'"
    );
}
