use serde_json::{Value, json};

use super::outbox::ProducerOutboxEnvelope;

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
        "broker_partition": 2,
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
