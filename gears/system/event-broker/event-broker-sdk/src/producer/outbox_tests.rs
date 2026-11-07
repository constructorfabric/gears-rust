use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use toolkit_db::outbox::MessageResult;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::{Ack, IngestOutcome, ProducerMode};
use crate::broker_stub::BrokerStub;
use crate::error::EventBrokerError;
use crate::gts::EventV1;
use crate::ids::ProducerId;
use crate::producer::{OutboxRegistration, OutboxSetup};
use crate::warn_capture::WarnCapture;

use super::outbox::{
    PRODUCER_OUTBOX_ENVELOPE_VERSION, PRODUCER_OUTBOX_PAYLOAD_TYPE, ProducerOutboxEnvelope,
    TEST_QUEUE, retry_or_reject,
};

/// An outbox row as the producer writes it, with `producer_mode` in its wire form.
fn row(producer_mode: &str) -> Value {
    json!({
        "version": 1,
        "event_id": "0b3f4c9e-2a41-4f5e-9d3c-6f1f2a7b8c90",
        "type": "gts.cf.core.events.event.v1~acme.orders.order.created.v1~",
        "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
        "tenant_id": "5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f",
        "source": "orders",
        "subject": "order-1",
        "subject_type": "gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~",
        "occurred_at": "2026-09-30T12:00:00Z",
        "producer_mode": producer_mode,
        "diagnostic_metadata": { "sdk_client_agent": "test" }
    })
}

#[test]
fn an_outbox_row_keeps_the_producer_mode_wire_form() {
    for mode in ["stateless", "monotonic", "chained"] {
        let stored = row(mode);
        let envelope: ProducerOutboxEnvelope =
            serde_json::from_value(stored.clone()).expect("a stored row reads");
        assert_eq!(
            serde_json::to_value(&envelope).expect("the envelope writes"),
            stored,
            "the row round-trips with producer_mode {mode:?}"
        );
    }
}

#[test]
fn an_outbox_row_with_an_unknown_producer_mode_is_refused() {
    let err = serde_json::from_value::<ProducerOutboxEnvelope>(row("burst"))
        .expect_err("an unknown mode does not read");
    assert!(
        err.to_string().contains("unknown variant `burst`"),
        "refused for its mode, not for another field: {err}"
    );
}

#[test]
fn a_rejected_message_stores_the_error_kind_not_its_text() {
    let result = retry_or_reject(&EventBrokerError::EventDataInvalid {
        type_id: "gts.cf.core.events.event.v1~acme.orders.order.created.v1~".to_owned(),
        errors: vec!["\"PLANTED-VALUE-7f3a\" is not of type \"integer\"".to_owned()],
        detail: "event payload failed schema validation".to_owned(),
    });
    let MessageResult::Reject(reason) = result else {
        panic!("expected a reject, got {result:?}");
    };
    assert_eq!(reason, "producer outbox message rejected: EventDataInvalid");

    let result = retry_or_reject(&EventBrokerError::Transport(
        "PLANTED-VALUE-7f3a connection reset".to_owned(),
    ));
    assert!(
        matches!(result, MessageResult::Retry),
        "a transport failure retries, got {result:?}"
    );
}

/// Pins the v1 producer-outbox envelope on the wire (design 9.3). bss pricing and bss products
/// hand-write it on their queues before a broker is bound, the SDK's processor drains those rows,
/// and their tests read it only as JSON (pricing `events_tests.rs`, `pricing_events.rs` and
/// `price_cancel_end_doors.rs`, products `broker_tests.rs`). The first body is pricing's interim
/// envelope as `the_interim_envelope_is_the_v1_stateless_envelope` asserts it, with a fixed
/// `event_id`; it reads as `ProducerOutboxEnvelope` and writes back unchanged, except its
/// `"trace_parent": null`, which the SDK leaves out. The second is a managed chained producer's
/// envelope, with every optional field set.
#[test]
fn the_v1_envelope_wire_shape_is_pinned() {
    assert_eq!(PRODUCER_OUTBOX_ENVELOPE_VERSION, 1);
    assert_eq!(
        PRODUCER_OUTBOX_PAYLOAD_TYPE,
        "application/vnd.constructorfabric.event-broker.producer-outbox+json;version=1"
    );

    let interim = json!({
        "version": 1,
        "event_id": "0190a5b2-7c3d-7e4f-8a1b-2c3d4e5f6a7b",
        "type": "gts.cf.core.events.event.v1~cf.bss.pricing.approval_unit_decided.v1~",
        "topic": "gts.cf.core.events.topic.v1~cf.bss.pricing.catalog.v1",
        "tenant_id": "5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f",
        "source": "bss-pricing",
        "subject": "6b1f9e2d-8c3a-4d7b-9f4e-0a5c2d3e4f5a",
        "subject_type": "gts.cf.core.events.subject.v1~cf.bss.pricing.approval_unit.v1~",
        "occurred_at": "1970-01-01T00:00:00Z",
        "trace_parent": null,
        "data": {
            "tenantId": "5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f",
            "unitId": "6b1f9e2d-8c3a-4d7b-9f4e-0a5c2d3e4f5a",
            "kind": "prices",
            "state": "approved",
            "generation": 1,
            "actors": ["7c2a0f3e-9d4b-4e8c-8a5f-1b6d3e4f5a6b"]
        },
        "producer_mode": "stateless",
        "diagnostic_metadata": { "sdk_client_agent": "bss-pricing" }
    });
    let envelope: ProducerOutboxEnvelope =
        serde_json::from_value(interim).expect("the interim envelope reads");
    assert_eq!(
        serde_json::to_value(&envelope).expect("the envelope writes"),
        json!({
            "version": 1,
            "event_id": "0190a5b2-7c3d-7e4f-8a1b-2c3d4e5f6a7b",
            "type": "gts.cf.core.events.event.v1~cf.bss.pricing.approval_unit_decided.v1~",
            "topic": "gts.cf.core.events.topic.v1~cf.bss.pricing.catalog.v1",
            "tenant_id": "5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f",
            "source": "bss-pricing",
            "subject": "6b1f9e2d-8c3a-4d7b-9f4e-0a5c2d3e4f5a",
            "subject_type": "gts.cf.core.events.subject.v1~cf.bss.pricing.approval_unit.v1~",
            "occurred_at": "1970-01-01T00:00:00Z",
            "data": {
                "tenantId": "5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f",
                "unitId": "6b1f9e2d-8c3a-4d7b-9f4e-0a5c2d3e4f5a",
                "kind": "prices",
                "state": "approved",
                "generation": 1,
                "actors": ["7c2a0f3e-9d4b-4e8c-8a5f-1b6d3e4f5a6b"]
            },
            "producer_mode": "stateless",
            "diagnostic_metadata": { "sdk_client_agent": "bss-pricing" }
        })
    );

    let managed = json!({
        "version": 1,
        "event_id": "0190a5b2-7c3d-7e4f-8a1b-000000000042",
        "type": "gts.cf.core.events.event.v1~acme.orders.order.created.v1~",
        "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
        "tenant_id": "5a0e8d1c-7b2f-4c6a-8e3d-9f4b1c2d3e4f",
        "source": "orders",
        "subject": "order-1",
        "subject_type": "gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~",
        "occurred_at": "2026-11-03T10:15:30.123456789Z",
        "trace_parent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "data": { "order_id": "order-1", "total_cents": 1250 },
        "producer_mode": "chained",
        "diagnostic_metadata": { "sdk_client_agent": "order-service/1.0" }
    });
    let envelope: ProducerOutboxEnvelope =
        serde_json::from_value(managed.clone()).expect("the managed envelope reads");
    assert_eq!(
        serde_json::to_value(&envelope).expect("the envelope writes"),
        managed
    );
}

/// The id `CountingBroker` registers every partition under.
const REGISTERED: ProducerId = ProducerId(Uuid::from_u128(1));

/// Counts the broker calls a drain can make: `register_producer` (a managed partition's first
/// drain) and `publish`. Every other call panics through `BrokerStub`, so a drain that reaches
/// any other broker operation fails the test too.
#[derive(Default)]
struct CountingBroker {
    calls: AtomicUsize,
}

impl CountingBroker {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BrokerStub for CountingBroker {
    async fn register_producer(
        &self,
        _ctx: &SecurityContext,
        _mode: ProducerMode,
        _client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(REGISTERED)
    }

    async fn publish(
        &self,
        _ctx: &SecurityContext,
        _event: &EventV1,
        _ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(IngestOutcome::Accepted)
    }
}

/// A managed monotonic setup over `broker` and a migrated in-memory SQLite database, built
/// without a broker call. Monotonic, so a drain that gets past the checks registers its
/// partition and publishes, and reads no chain cursor.
async fn managed_setup(broker: Arc<CountingBroker>) -> OutboxSetup {
    let db = toolkit_db::connect_db(
        "sqlite::memory:",
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("sqlite connects");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::producer_registration_migrations(),
    )
    .await
    .expect("registration migrations run");
    OutboxSetup::builder()
        .broker(broker)
        .db(db)
        .security_context(SecurityContext::anonymous())
        .source("orders")
        .registration(OutboxRegistration::managed(
            ProducerMode::Monotonic,
            "orders",
        ))
        .topics(["gts.cf.core.events.topic.v1~acme.orders.v1"])
        .event_type_patterns(["gts.cf.core.events.event.v1~acme.orders.*"])
        .queue(TEST_QUEUE, toolkit_db::outbox::Partitions::of(1))
        .client_agent("test")
        .lazy_schema_loading()
        .build()
        .await
        .expect("the setup builds")
}

/// An envelope of a version this SDK does not write is refused right after it decodes, with its
/// own reject text: before the partition's producer id is resolved, so the broker is never
/// called and no registration is stored.
#[tokio::test]
async fn an_envelope_of_another_version_is_rejected_before_any_broker_call() {
    let broker = Arc::new(CountingBroker::default());
    let setup = managed_setup(Arc::clone(&broker)).await;
    let mut payload = row("monotonic");
    payload["version"] = json!(2);

    let result = setup
        .process_outbox_payload_for_test(serde_json::to_vec(&payload).expect("the row writes"), 7)
        .await;

    let MessageResult::Reject(reason) = result else {
        panic!("expected a reject, got {result:?}");
    };
    assert_eq!(reason, "unsupported producer envelope version 2");
    assert_eq!(broker.calls(), 0, "no broker call");
    assert_eq!(
        setup
            .registrations_for_test()
            .await
            .expect("the registrations read"),
        vec![]
    );
}

/// An undecodable envelope is logged at `ERROR` before it is rejected, with the message's
/// position, its payload type and serde's error category only. Serde's message quotes the
/// planted field value; neither the log line nor the reject carries it.
#[tokio::test]
async fn an_undecodable_envelope_is_logged_without_its_field_values() {
    let broker = Arc::new(CountingBroker::default());
    let setup = managed_setup(Arc::clone(&broker)).await;
    let mut payload = row("monotonic");
    payload["version"] = json!("PLANTED-v");
    let payload = serde_json::to_vec(&payload).expect("the row writes");
    let serde_message = serde_json::from_slice::<ProducerOutboxEnvelope>(&payload)
        .expect_err("a string version does not read")
        .to_string();
    assert!(
        serde_message.contains("PLANTED-v"),
        "serde's own message quotes the value: {serde_message}"
    );

    let capture = WarnCapture::install();
    let result = setup.process_outbox_payload_for_test(payload, 7).await;
    let lines = capture.lines();

    let MessageResult::Reject(reason) = result else {
        panic!("expected a reject, got {result:?}");
    };
    assert_eq!(reason, "undecodable producer envelope (data)");
    let [line] = lines.as_slice() else {
        panic!("expected one logged line, got {lines:?}");
    };
    assert!(!line.contains("PLANTED-v"), "{line}");
    assert_eq!(
        line,
        "ERROR producer outbox message rejected queue=\"event-broker-producer-test\" \
         partition_id=0 seq=7 \
         payload_type=\"application/vnd.constructorfabric.event-broker.\
         producer-outbox+json;version=1\" \
         reason=\"undecodable\" category=\"data\""
    );
    assert_eq!(broker.calls(), 0, "no broker call");
}

/// An envelope of another version is logged at `ERROR` before it is rejected, with the version
/// it carries.
#[tokio::test]
async fn an_envelope_of_another_version_is_logged_with_its_version() {
    let broker = Arc::new(CountingBroker::default());
    let setup = managed_setup(Arc::clone(&broker)).await;
    let mut payload = row("monotonic");
    payload["version"] = json!(2);

    let capture = WarnCapture::install();
    let result = setup
        .process_outbox_payload_for_test(serde_json::to_vec(&payload).expect("the row writes"), 7)
        .await;
    let lines = capture.lines();

    assert!(
        matches!(result, MessageResult::Reject(_)),
        "expected a reject, got {result:?}"
    );
    assert_eq!(
        lines,
        vec![
            "ERROR producer outbox message rejected queue=\"event-broker-producer-test\" \
             partition_id=0 seq=7 \
             payload_type=\"application/vnd.constructorfabric.event-broker.\
             producer-outbox+json;version=1\" \
             reason=\"version\" version=2"
                .to_owned()
        ]
    );
    assert_eq!(broker.calls(), 0, "no broker call");
}
