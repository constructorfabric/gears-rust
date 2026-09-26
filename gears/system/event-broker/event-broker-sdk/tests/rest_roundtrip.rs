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
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, Event, EventBrokerApi, Fallback,
    GtsInstanceId, GtsTypeId, HandlerOutcome, InMemoryOffsetManager, RawEvent, SingleEventHandler,
    gts_id,
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
impl SingleEventHandler for RecordingHandler {
    async fn handle(
        &self,
        event: RawEvent,
        _attempts: u16,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.subjects.lock().unwrap().push(event.subject);
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
            &Event {
                id: Uuid::new_v4(),
                type_id: GtsTypeId::new(EVENT_TYPE),
                tenant_id: tenant,
                source: "rest-roundtrip-source".to_owned(),
                subject: "rest-1".to_owned(),
                subject_type: GtsTypeId::new(SUBJECT_TYPE),
                occurred_at: chrono::Utc::now(),
                trace_parent: None,
                data: Some(json!({ "ok": true })),
                partition: None,
                sequence: None,
                sequence_time: None,
                meta: None,
            },
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
