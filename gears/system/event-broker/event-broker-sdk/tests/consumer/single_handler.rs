use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker::test_support::StaticTypesRegistry;
use event_broker_sdk::{
    Ack, ConsumerBuilder, ConsumerError, ConsumerGroupRef, ConsumerRetry, ConsumerRuntimeEvent,
    Delivery, EventHandler, EventV1, Fallback, GtsInstanceId, GtsTypeId, HandlerOutcome,
    InMemoryOffsetManager, gts_id,
};
use serde_json::json;
use uuid::Uuid;

use super::common::{
    PublishJson, SHOWCASE_SUBJECT_TYPE, fixture_from_catalog, publish_json, topic, topic_fixture,
    two_topic_fixture, wait_until,
};
use super::doubles::RecordingRuntimeListener;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.single.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.single.v1~");

/// Bounds each await of a test that waits on delivery, a runtime event or `stop()`.
const BOUND: Duration = Duration::from_secs(5);

struct SingleEventProjector {
    offsets: Arc<Mutex<Vec<i64>>>,
    partitions: Arc<Mutex<Vec<u32>>>,
}

#[async_trait]
impl EventHandler<serde_json::Value> for SingleEventProjector {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.partitions.lock().unwrap().push(delivery.partition());
        self.offsets.lock().unwrap().push(
            event
                .sequence()
                .expect("a delivered event is stamped")
                .as_i64(),
        );
        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn if_i_want_a_simple_single_event_handler() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let offsets = Arc::new(Mutex::new(Vec::new()));
    let partitions = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-single"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(SingleEventProjector {
            offsets: offsets.clone(),
            partitions: partitions.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        EVENT_TYPE,
        "single-1",
        None,
        serde_json::json!({ "kind": "single" }),
    )
    .await;

    wait_until(|| offsets.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");

    assert_eq!(offsets.lock().unwrap().len(), 1);
    assert_eq!(partitions.lock().unwrap()[0], 0);
}

/// The delivered event carries the partition the broker derived, not the value it
/// derived it from - the key is a property of the event type, so a consumer never
/// sees it.
#[tokio::test]
async fn if_i_publish_events_partitioned_by_a_payload_member_each_lands_on_its_partition() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let offsets = Arc::new(Mutex::new(Vec::new()));
    let partitions = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous(
            "showcase-single-partition-key",
        ))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(SingleEventProjector {
            offsets: offsets.clone(),
            partitions: partitions.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    for target in [0, 1] {
        super::common::publish_json_with_partition_key(PublishJson {
            broker: &fixture.broker,
            ctx: &fixture.ctx,
            event_type: EVENT_TYPE,
            subject: "single-1",
            partition_key: None,
            partition: Some(target),
            data: serde_json::json!({ "kind": "single" }),
        })
        .await;
    }

    wait_until(|| offsets.lock().unwrap().len() == 2).await;
    handle.stop().await.expect("consumer stops");

    let mut seen = partitions.lock().unwrap().clone();
    seen.sort_unstable();
    assert_eq!(seen, vec![0, 1]);
}

const ORDERS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.orders.v1");
const ORDERS_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.orders.v1~");
const AUDIT_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.audit.v1");
const AUDIT_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.audit.v1~");

/// The `(topic, partition)` each delivered event was attributed to.
type Attributions = Arc<Mutex<Vec<(String, u32)>>>;

/// Records which `(topic, partition)` each delivered event was attributed to.
struct AttributionProjector {
    seen: Attributions,
}

#[async_trait]
impl EventHandler<serde_json::Value> for AttributionProjector {
    async fn handle(
        &self,
        _event: EventV1,
        delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.seen
            .lock()
            .unwrap()
            .push((delivery.topic().as_ref().to_owned(), delivery.partition()));
        Ok(HandlerOutcome::Success)
    }
}

/// An event carries no topic, so a consumer of two topics attributes each event by
/// resolving its type. Both topics use partition 0, which is exactly the case a
/// `partition`-only frame could not distinguish - transposing the two topics makes
/// this fail.
#[tokio::test]
async fn events_from_two_topics_are_each_attributed_to_their_own_topic() {
    let fixture =
        two_topic_fixture((ORDERS_TOPIC, ORDERS_EVENT), (AUDIT_TOPIC, AUDIT_EVENT), 1).await;
    let seen = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-two-topics"))
        .topics([topic(ORDERS_TOPIC), topic(AUDIT_TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(AttributionProjector { seen: seen.clone() })
        .start()
        .await
        .expect("consumer starts");

    for (event_type, subject) in [(ORDERS_EVENT, "order-1"), (AUDIT_EVENT, "audit-1")] {
        publish_json(
            &fixture.broker,
            &fixture.ctx,
            event_type,
            subject,
            None,
            serde_json::json!({ "kind": "two-topic" }),
        )
        .await;
    }

    wait_until(|| seen.lock().unwrap().len() == 2).await;
    handle.stop().await.expect("consumer stops");

    let mut attributed = seen.lock().unwrap().clone();
    attributed.sort();
    assert_eq!(
        attributed,
        vec![(AUDIT_TOPIC.to_owned(), 0), (ORDERS_TOPIC.to_owned(), 0)]
    );
}

const SEEN_ID: Uuid = Uuid::from_u128(0x7160_0001);

/// What a handler saw of one delivered event: `(id, source, data, topic, partition,
/// attempts)`.
type Seen = (
    Uuid,
    Option<String>,
    Option<serde_json::Value>,
    GtsInstanceId,
    u32,
    u16,
);

/// Records what it is handed of every event and its delivery.
#[derive(Clone, Default)]
struct SeeingHandler {
    seen: Arc<Mutex<Vec<Seen>>>,
}

#[async_trait]
impl EventHandler<serde_json::Value> for SeeingHandler {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.seen.lock().unwrap().push((
            event.id(),
            event.source().map(str::to_owned),
            event.data().cloned(),
            delivery.topic().clone(),
            delivery.partition(),
            delivery.attempts(),
        ));
        Ok(HandlerOutcome::Success)
    }
}

/// An event of `event_type` with a fixed id, published by `order-service`. The
/// fixture's types are partitioned by `/source`, so with one partition it lands on 0.
fn order_service_event(tenant: Uuid, event_type: &str, data: Option<serde_json::Value>) -> EventV1 {
    let builder = match data {
        Some(data) => EventV1::builder_for(GtsTypeId::new(event_type), data),
        None => EventV1::builder_without_data(GtsTypeId::new(event_type)),
    };
    builder
        .id(SEEN_ID)
        .tenant_id(tenant)
        .source("order-service")
        .subject("order-1")
        .subject_type_id(GtsTypeId::new(SHOWCASE_SUBJECT_TYPE))
        .build()
        .expect("a valid event")
}

/// The handler gets the event the producer built, its `source` included, and the
/// topic, partition and attempt it was delivered on.
#[tokio::test]
async fn a_handler_sees_the_published_events_source_and_delivery() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let handler = SeeingHandler::default();
    let seen = handler.seen.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-single-seen"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(handler)
        .start()
        .await
        .expect("consumer starts");

    let event = order_service_event(
        fixture.ctx.subject_tenant_id(),
        EVENT_TYPE,
        Some(json!({ "kind": "seen" })),
    );
    fixture
        .broker
        .publish(&fixture.ctx, &event, Ack::Accepted)
        .await
        .expect("event published");

    wait_until(|| seen.lock().unwrap().len() == 1).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            SEEN_ID,
            Some("order-service".to_owned()),
            Some(json!({ "kind": "seen" })),
            topic(TOPIC),
            0,
            1,
        )]
    );
}

/// Answers `Retry` on its first call and `Success` after, recording the attempt of
/// every call.
#[derive(Clone, Default)]
struct RetryOnceHandler {
    attempts: Arc<Mutex<Vec<u16>>>,
}

#[async_trait]
impl EventHandler<serde_json::Value> for RetryOnceHandler {
    async fn handle(
        &self,
        _event: EventV1,
        delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let mut attempts = self.attempts.lock().unwrap();
        attempts.push(delivery.attempts());
        if attempts.len() == 1 {
            return Ok(HandlerOutcome::Retry {
                reason: "not yet".to_owned(),
            });
        }
        Ok(HandlerOutcome::Success)
    }
}

/// The runtime counts attempts from 1 and redelivers a retried event as the next one.
#[tokio::test]
async fn a_retried_event_is_redelivered_with_the_next_attempt() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let handler = RetryOnceHandler::default();
    let attempts = handler.attempts.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("showcase-single-retry"))
        .topics([topic(TOPIC)])
        .retry(ConsumerRetry {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(handler)
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        EVENT_TYPE,
        "retry-1",
        None,
        json!({ "kind": "retry" }),
    )
    .await;

    wait_until(|| attempts.lock().unwrap().len() == 2).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(*attempts.lock().unwrap(), vec![1, 2]);
}

/// `.retry(ConsumerRetry)` sets the backoff the runtime waits before redelivering a
/// retried event: the scheduled retry carries the 60 s delay, and `stop()` does not
/// wait out the pending backoff.
#[tokio::test]
async fn a_retry_override_sets_the_handler_backoff() {
    let fixture = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let handler = RetryOnceHandler::default();
    let attempts = handler.attempts.clone();
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous(
            "showcase-single-retry-override",
        ))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .retry(ConsumerRetry {
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(60),
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(handler)
        .start()
        .await
        .expect("consumer starts");

    publish_json(
        &fixture.broker,
        &fixture.ctx,
        EVENT_TYPE,
        "retry-override-1",
        None,
        json!({ "kind": "retry" }),
    )
    .await;

    // `RetryScheduled` is emitted after the handler's `Retry`, and no other handler
    // answer or error schedules a retry in this test.
    let retries = || -> Vec<ConsumerRuntimeEvent> {
        runtime_events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, ConsumerRuntimeEvent::RetryScheduled { .. }))
            .cloned()
            .collect()
    };
    tokio::time::timeout(BOUND, async {
        while retries().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the retry is scheduled within the bound");
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound while a backoff is pending")
        .expect("consumer stops");

    let retries = retries();
    let [ConsumerRuntimeEvent::RetryScheduled { attempt, delay, .. }] = retries.as_slice() else {
        panic!("expected exactly one RetryScheduled, got {retries:?}");
    };
    assert_eq!((*attempt, *delay), (1, Duration::from_secs(60)));
    assert_eq!(*attempts.lock().unwrap(), vec![1]);
}

const BODY_LESS_EVENT: &str =
    gts_id!("cf.core.events.event.v1~example.mock.showcase.body_less.v1~");

/// A body-less event reaches the handler without data, not with a JSON `null`. Its
/// type's data schema admits the `null` the broker validates a body-less event as.
#[tokio::test]
async fn a_body_less_event_reaches_the_handler_without_data() {
    let fixture = fixture_from_catalog(StaticTypesRegistry::of(json!([
        { "id": TOPIC, "partitions": 1 },
        {
            "id": BODY_LESS_EVENT,
            "topic": TOPIC,
            "data_schema": {},
            "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE],
            "partition_key": "/source",
        },
    ])))
    .await;
    let handler = SeeingHandler::default();
    let seen = handler.seen.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous(
            "showcase-single-body-less",
        ))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(handler)
        .start()
        .await
        .expect("consumer starts");

    let event = order_service_event(fixture.ctx.subject_tenant_id(), BODY_LESS_EVENT, None);
    fixture
        .broker
        .publish(&fixture.ctx, &event, Ack::Accepted)
        .await
        .expect("a body-less event is published");

    wait_until(|| seen.lock().unwrap().len() == 1).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *seen.lock().unwrap(),
        vec![(
            SEEN_ID,
            Some("order-service".to_owned()),
            None,
            topic(TOPIC),
            0,
            1,
        )]
    );
}
