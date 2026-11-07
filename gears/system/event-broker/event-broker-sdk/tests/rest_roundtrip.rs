//! End-to-end round-trip of the REST transport against a real in-process broker.
//!
//! The SDK's `RestBroker` (an `EventBrokerApi` over HTTP) is pointed at the
//! `event-broker` gear's own axum router, served on an ephemeral loopback port.
//! Driving publish -> join -> stream through it exercises the whole wire mapping -
//! typed GTS ids to JSON and back, the stream framing, the seek/join protocol -
//! against the same gear a production deployment runs, not a mock. Asserted once
//! per `StreamTransport` framing, since the two share every path but the decoder.

#![cfg(feature = "rest-client")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker::test_support::{EventBrokerHarness, StaticTypesRegistry};
use event_broker_sdk::rest::{RestBroker, StreamTransport};
use event_broker_sdk::{
    Ack, ConsumerBuilder, ConsumerError, ConsumerGroupRef, Delivery, EventBrokerApi,
    EventBrokerError, EventHandler, EventV1, Fallback, GtsInstanceId, GtsTypeId, HandlerOutcome,
    InMemoryOffsetManager, gts_id,
};
use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.rest.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.rest.v1~");
const SUBJECT_TYPE: &str = "gts.x.eb.showcase.subject.v1~";

struct RecordingHandler {
    subjects: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl EventHandler<serde_json::Value> for RecordingHandler {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.subjects
            .lock()
            .unwrap()
            .push(event.subject().to_owned());
        Ok(HandlerOutcome::Success)
    }
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..200 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition was not observed before timeout");
}

/// A publishable event of the test type for `tenant`, about `subject`.
fn event(tenant: Uuid, subject: &str) -> EventV1 {
    EventV1::builder_for(GtsTypeId::new(EVENT_TYPE), json!({ "ok": true }))
        .tenant_id(tenant)
        .source("rest-roundtrip-source")
        .subject(subject)
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .expect("a valid event")
}

/// One publish -> consume round-trip driven entirely through the REST transport
/// with the given stream framing.
async fn rest_round_trip(framing: StreamTransport) {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            {
                "id": EVENT_TYPE,
                "topic": TOPIC,
                "data_schema": { "type": "object" },
                "allowed_subject_types": [SUBJECT_TYPE],
                "partition_key": "/source",
            },
        ])))
        .build()
        .await;

    // The gear's router injects a fixed `SecurityContext` (there is no auth layer
    // in the test router), so every REST request runs as this tenant. The
    // published event and the consumer's interest must share it for delivery to
    // match.
    let tenant = harness.security_context().subject_tenant_id();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("resolve bound addr");
    let router = harness.router().clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve router");
    });

    let broker: Arc<dyn EventBrokerApi> = Arc::new(
        RestBroker::new(format!("http://{addr}"))
            .expect("build rest client")
            .with_stream_transport(framing),
    );

    let subjects = Arc::new(Mutex::new(Vec::new()));
    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("rest-roundtrip"))
        .tenant_id(tenant)
        .topics([GtsInstanceId::try_new(TOPIC).unwrap()])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(RecordingHandler {
            subjects: subjects.clone(),
        })
        .start()
        .await
        .expect("rest consumer starts");

    broker
        .publish(
            &SecurityContext::anonymous(),
            &event(tenant, "rest-1"),
            Ack::Accepted,
        )
        .await
        .expect("event published over REST");

    wait_until(|| subjects.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");
    server.abort();

    assert_eq!(subjects.lock().unwrap().as_slice(), ["rest-1"]);
}

#[tokio::test]
async fn rest_client_round_trips_with_multipart_framing() {
    rest_round_trip(StreamTransport::Multipart).await;
}

#[tokio::test]
async fn rest_client_round_trips_with_sse_framing() {
    rest_round_trip(StreamTransport::Sse).await;
}

/// A persisted publish over REST, single or batch, is answered `Unimplemented` and
/// admits nothing: the consumer then receives only the sentinel published after it.
#[tokio::test]
async fn a_persisted_publish_is_unimplemented_until_the_broker_serves_it() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            {
                "id": EVENT_TYPE,
                "topic": TOPIC,
                "data_schema": { "type": "object" },
                "allowed_subject_types": [SUBJECT_TYPE],
                "partition_key": "/source",
            },
        ])))
        .build()
        .await;
    let tenant = harness.security_context().subject_tenant_id();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("resolve bound addr");
    let router = harness.router().clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve router");
    });
    let broker: Arc<dyn EventBrokerApi> =
        Arc::new(RestBroker::new(format!("http://{addr}")).expect("build rest client"));
    let subjects = Arc::new(Mutex::new(Vec::new()));
    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("rest-roundtrip-persisted"))
        .tenant_id(tenant)
        .topics([GtsInstanceId::try_new(TOPIC).unwrap()])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(RecordingHandler {
            subjects: subjects.clone(),
        })
        .start()
        .await
        .expect("rest consumer starts");
    let ctx = SecurityContext::anonymous();
    let persisted = Ack::Persisted {
        within: Duration::from_secs(5),
    };

    // #5655: the broker serves no persisted publish until synchronous publish lands.
    let single = broker
        .publish(&ctx, &event(tenant, "persisted-single"), persisted)
        .await;
    let Err(EventBrokerError::Unimplemented(detail)) = single else {
        panic!("expected `Unimplemented`, got {single:?}");
    };
    assert_eq!(
        detail,
        "synchronous publish (Prefer: wait) is not implemented yet"
    );
    let batch = broker
        .publish_batch(&ctx, &[event(tenant, "persisted-batch")], persisted)
        .await;
    let Err(EventBrokerError::Unimplemented(detail)) = batch else {
        panic!("expected `Unimplemented`, got {batch:?}");
    };
    assert_eq!(
        detail,
        "synchronous publish (Prefer: wait) is not implemented yet"
    );

    broker
        .publish(&ctx, &event(tenant, "sentinel"), Ack::Accepted)
        .await
        .expect("the sentinel publishes");
    wait_until(|| subjects.lock().unwrap().len() == 1).await;
    handle.stop().await.expect("consumer stops");
    server.abort();

    assert_eq!(subjects.lock().unwrap().as_slice(), ["sentinel"]);
}
