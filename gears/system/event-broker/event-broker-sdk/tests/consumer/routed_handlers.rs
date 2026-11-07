use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::{
    Ack, BatchHandler, BatchHandlerOutcome, ConsumerBuilder, ConsumerError, ConsumerGroupRef,
    ConsumerRetry, ConsumerRuntimeEvent, DecodeCategory, Delivery, EventBatch, EventHandler,
    EventV1, Fallback, GtsTypeId, HandlerOutcome, InMemoryOffsetManager, Sequence, TopicId,
    Undecodable, gts_id,
};
use gts::GtsSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use event_broker::test_support::StaticTypesRegistry;
use serde_json::json;

use super::common::{
    SHOWCASE_SUBJECT_TYPE, TopicFixture, event_pattern, fixture_from_catalog, publish_json, topic,
    wait_until,
};
use super::doubles::{RecordingRuntimeListener, WarnCapture};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.routed.v1");
const CREATED: &str = gts_id!("cf.core.events.event.v1~example.mock.routed.created.v1~");
const UPDATED: &str = gts_id!("cf.core.events.event.v1~example.mock.routed.updated.v1~");

struct NamedHandler {
    name: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl EventHandler<serde_json::Value> for NamedHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push(self.name);
        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn if_i_need_topic_type_routing_i_can_register_specific_and_default_handlers() {
    // Both event types are declared on the one topic up front - a real broker
    // resolves types from its registered catalog, not a runtime registration.
    let fixture = fixture_from_catalog(StaticTypesRegistry::of(json!([
        { "id": TOPIC, "partitions": 1 },
        { "id": CREATED, "topic": TOPIC, "data_schema": { "type": "object" }, "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE], "partition_key": "/source" },
        { "id": UPDATED, "topic": TOPIC, "data_schema": { "type": "object" }, "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE], "partition_key": "/source" },
    ])))
    .await;

    let calls = Arc::new(Mutex::new(Vec::new()));
    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-routed"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NamedHandler {
            name: "default",
            calls: calls.clone(),
        })
        .route()
        .topic(topic(TOPIC))
        .event_type(event_pattern(CREATED))
        .handler(NamedHandler {
            name: "created",
            calls: calls.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        CREATED,
        "created-1",
        None,
        serde_json::json!({ "route": "created" }),
    )
    .await;
    publish_json(
        &fixture.broker,
        &fixture.ctx,
        UPDATED,
        "updated-1",
        None,
        serde_json::json!({ "route": "default" }),
    )
    .await;

    wait_until(|| calls.lock().unwrap().len() == 2).await;
    handle.stop().await.expect("consumer stops");

    assert_eq!(calls.lock().unwrap().as_slice(), ["created", "default"]);
}

// ---------------------------------------------------------------------------
// Typed routes: `.event::<P>(Undecodable)` decodes each event as `P` and applies
// the policy to an event that does not decode.
// ---------------------------------------------------------------------------

const ORDER_PLACED: &str = gts_id!("cf.core.events.event.v1~example.mock.routed.order_placed.v1~");

/// The typed route's payload; `order_id` and `quantity` are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OrderPlacedV1 {
    order_id: String,
    quantity: u32,
}

impl GtsSchema for OrderPlacedV1 {
    const TYPE_ID: &'static str = ORDER_PLACED;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

/// Bounds `wait_within_bound` and each `stop()`.
const BOUND: Duration = Duration::from_secs(5);

const BAD_ID: Uuid = Uuid::from_u128(0xBAD);
const GOOD_ID: Uuid = Uuid::from_u128(0x600D);

/// One topic of one partition whose `ORDER_PLACED` type admits any object, so the
/// broker stores a body that does not decode as `OrderPlacedV1`.
async fn typed_fixture() -> TopicFixture {
    fixture_from_catalog(StaticTypesRegistry::of(json!([
        { "id": TOPIC, "partitions": 1 },
        { "id": ORDER_PLACED, "topic": TOPIC, "data_schema": { "type": "object" }, "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE], "partition_key": "/source" },
    ])))
    .await
}

/// Publishes an `ORDER_PLACED` event with a fixed `id` carrying `data`.
async fn publish_order(fixture: &TopicFixture, id: Uuid, data: serde_json::Value) {
    let event = EventV1::builder_for(GtsTypeId::new(ORDER_PLACED), data)
        .id(id)
        .tenant_id(fixture.ctx.subject_tenant_id())
        .source("typed-route")
        .subject(id.to_string())
        .subject_type_id(GtsTypeId::new(SHOWCASE_SUBJECT_TYPE))
        .build()
        .expect("a valid event");
    fixture
        .broker
        .publish(&fixture.ctx, &event, Ack::Accepted)
        .await
        .expect("the broker accepts a body its type's schema admits");
}

type TypedCalls = Arc<Mutex<Vec<(Uuid, Option<OrderPlacedV1>)>>>;
type RawCalls = Arc<Mutex<Vec<(Uuid, Option<serde_json::Value>)>>>;

/// Records each typed event as `(id, data)`.
#[derive(Clone, Default)]
struct TypedRecorder {
    calls: TypedCalls,
}

#[async_trait]
impl EventHandler<OrderPlacedV1> for TypedRecorder {
    async fn handle(
        &self,
        event: EventV1<OrderPlacedV1>,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls
            .lock()
            .unwrap()
            .push((event.id(), event.data().cloned()));
        Ok(HandlerOutcome::Success)
    }
}

/// Records each raw event as `(id, data)`.
#[derive(Clone, Default)]
struct RawRecorder {
    calls: RawCalls,
}

#[async_trait]
impl EventHandler<serde_json::Value> for RawRecorder {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls
            .lock()
            .unwrap()
            .push((event.id(), event.data().cloned()));
        Ok(HandlerOutcome::Success)
    }
}

/// Polls every 10 ms until `predicate` holds or `BOUND` passes; a miss shows in the
/// test's asserts.
async fn wait_within_bound(mut predicate: impl FnMut() -> bool) {
    let _ = tokio::time::timeout(BOUND, async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

fn good_order() -> OrderPlacedV1 {
    OrderPlacedV1 {
        order_id: "o-2".to_owned(),
        quantity: 2,
    }
}

#[tokio::test]
async fn a_typed_route_hands_the_decoded_payload_to_its_handler() {
    let fixture = typed_fixture().await;
    let typed = TypedRecorder::default();
    let typed_calls = typed.calls.clone();
    let default = RawRecorder::default();
    let default_calls = default.calls.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("typed-route-decoded"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(default)
        .route()
        .topic(topic(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Stall)
        .handler(typed)
        .start()
        .await
        .expect("consumer starts");

    publish_order(
        &fixture,
        GOOD_ID,
        json!({ "order_id": "o-2", "quantity": 2 }),
    )
    .await;

    wait_until(|| typed_calls.lock().unwrap().len() == 1).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *typed_calls.lock().unwrap(),
        vec![(GOOD_ID, Some(good_order()))]
    );
    assert_eq!(*default_calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn an_undecodable_event_on_a_skip_route_is_skipped_and_reported() {
    let fixture = typed_fixture().await;
    let typed = TypedRecorder::default();
    let typed_calls = typed.calls.clone();
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("typed-route-skip"))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(RawRecorder::default())
        .route()
        .topic(topic(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Skip)
        .handler(typed)
        .start()
        .await
        .expect("consumer starts");

    publish_order(&fixture, BAD_ID, json!({ "order_id": "o-1" })).await;
    publish_order(
        &fixture,
        GOOD_ID,
        json!({ "order_id": "o-2", "quantity": 2 }),
    )
    .await;

    // The positions the handler path advanced the partition to, in order. The handler
    // path emits `HandlerBatchCompleted`, then `ProgressAdvanced`, for the same
    // partition; only a `ProgressAdvanced` of that partition after such a completion
    // counts. The other emitters are left out: control-frame and topology-frame commits
    // run when the broker sends a frame, and the auto-commit timer runs on its
    // interval, so when and how often they emit depends on timing.
    let progress = || -> Vec<Sequence> {
        let mut completed_partition = None;
        let mut reached = Vec::new();
        for event in runtime_events.lock().unwrap().iter() {
            match event {
                ConsumerRuntimeEvent::HandlerBatchCompleted { partition, .. } => {
                    completed_partition = Some(*partition);
                }
                ConsumerRuntimeEvent::ProgressAdvanced { progress, .. } => {
                    if let [only] = progress.as_slice()
                        && completed_partition == Some(only.partition)
                    {
                        reached.push(only.offset);
                        completed_partition = None;
                    }
                }
                _ => {}
            }
        }
        reached
    };
    wait_within_bound(|| progress().contains(&Sequence::assigned(2))).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *typed_calls.lock().unwrap(),
        vec![(GOOD_ID, Some(good_order()))]
    );
    // The bad event is sequence 1: the consumer moved past it without a handler, before
    // it reached the good event.
    assert_eq!(
        progress(),
        vec![Sequence::assigned(1), Sequence::assigned(2)]
    );
    let events = runtime_events.lock().unwrap();
    let skipped: Vec<&ConsumerRuntimeEvent> = events
        .iter()
        .filter(|event| matches!(event, ConsumerRuntimeEvent::EventSkipped { .. }))
        .collect();
    let [
        ConsumerRuntimeEvent::EventSkipped {
            topic: skipped_topic,
            partition,
            sequence,
            type_id,
            category,
        },
    ] = skipped.as_slice()
    else {
        panic!("expected one EventSkipped, got {skipped:?}");
    };
    assert_eq!(*skipped_topic, topic(TOPIC));
    assert_eq!(*partition, 0);
    assert_eq!(*sequence, Sequence::assigned(1));
    assert_eq!(*type_id, GtsTypeId::new(ORDER_PLACED));
    assert_eq!(*category, DecodeCategory::Data);
}

#[tokio::test]
async fn an_undecodable_event_on_a_stall_route_is_retried_and_never_handed_over() {
    let fixture = typed_fixture().await;
    let typed = TypedRecorder::default();
    let typed_calls = typed.calls.clone();
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("typed-route-stall"))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .retry(ConsumerRetry {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(RawRecorder::default())
        .route()
        .topic(topic(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Stall)
        .handler(typed)
        .start()
        .await
        .expect("consumer starts");

    publish_order(&fixture, BAD_ID, json!({ "order_id": "o-1" })).await;
    publish_order(
        &fixture,
        GOOD_ID,
        json!({ "order_id": "o-2", "quantity": 2 }),
    )
    .await;

    let retries = || -> Vec<(TopicId, String, u32, u16, Duration)> {
        runtime_events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                ConsumerRuntimeEvent::RetryScheduled {
                    topic_id,
                    topic,
                    partition,
                    attempt,
                    delay,
                } => Some((*topic_id, topic.to_string(), *partition, *attempt, *delay)),
                _ => None,
            })
            .collect()
    };
    wait_within_bound(|| retries().iter().any(|retry| retry.3 >= 3)).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    let retries = retries();
    let [first, second, third, ..] = retries.as_slice() else {
        panic!("expected at least three RetryScheduled, got {retries:?}");
    };
    let retry = |attempt| {
        (
            TopicId::from_gts(TOPIC),
            TOPIC.to_owned(),
            0,
            attempt,
            Duration::from_millis(1),
        )
    };
    assert_eq!([first, second, third], [&retry(1), &retry(2), &retry(3)]);
    assert_eq!(*typed_calls.lock().unwrap(), vec![]);
    let events = runtime_events.lock().unwrap();
    let reasons: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            ConsumerRuntimeEvent::HandlerBatchCompleted {
                outcome: BatchHandlerOutcome::Retry { reason },
                ..
            } => Some(reason.as_str()),
            _ => None,
        })
        .take(1)
        .collect();
    assert_eq!(
        reasons,
        vec![
            "event data of gts.cf.core.events.event.v1~example.mock.routed.order_placed.v1~ \
             does not decode as the payload type (data)"
        ]
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ConsumerRuntimeEvent::EventSkipped { .. })),
        "a stall route skips nothing"
    );
}

#[tokio::test]
async fn an_undecodable_event_on_a_fallback_route_goes_to_the_raw_handler() {
    let fixture = typed_fixture().await;
    let typed = TypedRecorder::default();
    let typed_calls = typed.calls.clone();
    let fallback = RawRecorder::default();
    let fallback_calls = fallback.calls.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("typed-route-fallback"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(RawRecorder::default())
        .route()
        .topic(topic(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Fallback(Arc::new(fallback)))
        .handler(typed)
        .start()
        .await
        .expect("consumer starts");

    publish_order(&fixture, BAD_ID, json!({ "order_id": "o-1" })).await;
    publish_order(
        &fixture,
        GOOD_ID,
        json!({ "order_id": "o-2", "quantity": 2 }),
    )
    .await;

    wait_until(|| typed_calls.lock().unwrap().len() == 1).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(BAD_ID, Some(json!({ "order_id": "o-1" })))]
    );
    assert_eq!(
        *typed_calls.lock().unwrap(),
        vec![(GOOD_ID, Some(good_order()))]
    );
}

/// A body whose `quantity` is a string: serde's message for it quotes the string.
fn planted_body() -> serde_json::Value {
    json!({ "order_id": "o-1", "quantity": "PLANTED-quantity-7" })
}

/// The skip warn names the event, its type and the decode category, never its data.
#[tokio::test]
async fn the_skip_warn_carries_no_payload_value() {
    let message = serde_json::from_value::<OrderPlacedV1>(planted_body())
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("PLANTED-quantity-7"),
        "serde's message quotes the planted value: {message}"
    );
    let capture = WarnCapture::install();
    let lines = capture.lines();
    let fixture = typed_fixture().await;
    let typed = TypedRecorder::default();
    let typed_calls = typed.calls.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("typed-route-skip-warn"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(RawRecorder::default())
        .route()
        .topic(topic(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Skip)
        .handler(typed)
        .start()
        .await
        .expect("consumer starts");

    publish_order(&fixture, BAD_ID, planted_body()).await;
    publish_order(
        &fixture,
        GOOD_ID,
        json!({ "order_id": "o-2", "quantity": 2 }),
    )
    .await;

    // The warn is written before the good event is read, so once the good event is
    // handled the warn is captured.
    wait_until(|| typed_calls.lock().unwrap().len() == 1).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    let lines = lines.lock().unwrap();
    let skips: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("skipping it"))
        .collect();
    assert_eq!(
        skips,
        vec![
            "WARN a delivered event does not decode as its route's payload type; skipping it \
             event_id=00000000-0000-0000-0000-000000000bad \
             type_id=gts.cf.core.events.event.v1~example.mock.routed.order_placed.v1~ \
             category=data"
        ]
    );
    assert!(
        !lines.iter().any(|line| line.contains("PLANTED")),
        "a warn quotes the payload: {lines:?}"
    );
}

/// The stall retry reason names the type and the decode category, never the data.
#[tokio::test]
async fn the_stall_retry_reason_carries_no_payload_value() {
    let message = serde_json::from_value::<OrderPlacedV1>(planted_body())
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("PLANTED-quantity-7"),
        "serde's message quotes the planted value: {message}"
    );
    let fixture = typed_fixture().await;
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("typed-route-stall-reason"))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .retry(ConsumerRetry {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(RawRecorder::default())
        .route()
        .topic(topic(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Stall)
        .handler(TypedRecorder::default())
        .start()
        .await
        .expect("consumer starts");

    publish_order(&fixture, BAD_ID, planted_body()).await;

    let reasons = || -> Vec<String> {
        runtime_events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                ConsumerRuntimeEvent::HandlerBatchCompleted {
                    outcome: BatchHandlerOutcome::Retry { reason },
                    ..
                } => Some(reason.clone()),
                _ => None,
            })
            .collect()
    };
    wait_within_bound(|| !reasons().is_empty()).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    let reasons = reasons();
    let [first, ..] = reasons.as_slice() else {
        panic!("expected a Retry outcome, got none");
    };
    assert_eq!(
        first,
        "event data of gts.cf.core.events.event.v1~example.mock.routed.order_placed.v1~ \
         does not decode as the payload type (data)"
    );
    assert!(
        !reasons.iter().any(|reason| reason.contains("PLANTED")),
        "a retry reason quotes the payload: {reasons:?}"
    );
}

// ---------------------------------------------------------------------------
// Typed batch routes: `.event::<P>(Undecodable).batch_handler(h)`. The default profile
// batches one event at a time, so these reach a fully decoded batch and an undecodable
// first event; the typed-prefix cases are covered at the adapter in
// `src/consumer/typed_tests.rs`.
// ---------------------------------------------------------------------------

const SECOND_GOOD_ID: Uuid = Uuid::from_u128(0x600E);

/// Records each event of each typed batch as `(id, data)`.
#[derive(Clone, Default)]
struct TypedBatchRecorder {
    calls: TypedCalls,
}

#[async_trait]
impl BatchHandler<OrderPlacedV1> for TypedBatchRecorder {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, OrderPlacedV1>,
        _delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().extend(
            batch
                .iter()
                .map(|event| (event.id(), event.data().cloned())),
        );
        Ok(BatchHandlerOutcome::Success)
    }
}

#[tokio::test]
async fn a_typed_batch_route_hands_decoded_batches_to_its_handler() {
    let fixture = typed_fixture().await;
    let typed = TypedBatchRecorder::default();
    let typed_calls = typed.calls.clone();
    let default = RawRecorder::default();
    let default_calls = default.calls.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous(
            "typed-batch-route-decoded",
        ))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(default)
        .route()
        .topic(topic(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Stall)
        .batch_handler(typed)
        .start()
        .await
        .expect("consumer starts");

    publish_order(
        &fixture,
        GOOD_ID,
        json!({ "order_id": "o-2", "quantity": 2 }),
    )
    .await;
    publish_order(
        &fixture,
        SECOND_GOOD_ID,
        json!({ "order_id": "o-3", "quantity": 3 }),
    )
    .await;

    wait_until(|| typed_calls.lock().unwrap().len() == 2).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *typed_calls.lock().unwrap(),
        vec![
            (GOOD_ID, Some(good_order())),
            (
                SECOND_GOOD_ID,
                Some(OrderPlacedV1 {
                    order_id: "o-3".to_owned(),
                    quantity: 3,
                })
            ),
        ]
    );
    assert_eq!(*default_calls.lock().unwrap(), vec![]);
}

/// `EventSkipped` has one emitter, the dispatcher's report of a delivery's skipped
/// event, sent after that delivery's `HandlerBatchCompleted`.
#[tokio::test]
async fn an_undecodable_event_on_a_typed_batch_skip_route_is_skipped_and_reported() {
    let fixture = typed_fixture().await;
    let typed = TypedBatchRecorder::default();
    let typed_calls = typed.calls.clone();
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("typed-batch-route-skip"))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(RawRecorder::default())
        .route()
        .topic(topic(TOPIC))
        .event::<OrderPlacedV1>(Undecodable::Skip)
        .batch_handler(typed)
        .start()
        .await
        .expect("consumer starts");

    publish_order(&fixture, BAD_ID, json!({ "order_id": "o-1" })).await;
    publish_order(
        &fixture,
        GOOD_ID,
        json!({ "order_id": "o-2", "quantity": 2 }),
    )
    .await;

    // The skip is reported before the good event is read, so once the good event is
    // handled the report is recorded.
    wait_until(|| typed_calls.lock().unwrap().len() == 1).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *typed_calls.lock().unwrap(),
        vec![(GOOD_ID, Some(good_order()))]
    );
    let events = runtime_events.lock().unwrap();
    let skipped: Vec<&ConsumerRuntimeEvent> = events
        .iter()
        .filter(|event| matches!(event, ConsumerRuntimeEvent::EventSkipped { .. }))
        .collect();
    let [
        ConsumerRuntimeEvent::EventSkipped {
            topic: skipped_topic,
            partition,
            sequence,
            type_id,
            category,
        },
    ] = skipped.as_slice()
    else {
        panic!("expected one EventSkipped, got {skipped:?}");
    };
    assert_eq!(*skipped_topic, topic(TOPIC));
    assert_eq!(*partition, 0);
    assert_eq!(*sequence, Sequence::assigned(1));
    assert_eq!(*type_id, GtsTypeId::new(ORDER_PLACED));
    assert_eq!(*category, DecodeCategory::Data);
}
