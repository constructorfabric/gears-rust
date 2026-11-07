//! JSON round-trip tests pinning the wire DTO shapes against the server's
//! `#[toolkit_macros::api_dto]` bodies. Each asserts the entire JSON inline.

use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::warn_capture::WarnCapture;

fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

#[test]
fn publish_event_serialises_with_type_rename_and_meta() {
    let wire = PublishEventWire {
        id: uuid(1),
        type_id: gts::GtsTypeId::new("gts.cf.core.events.event.v1~acme.orders.created.v1~"),
        tenant_id: uuid(2),
        source: "orders".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: gts::GtsTypeId::new("gts.cf.core.events.subject.v1~acme.orders.order.v1~"),
        occurred_at: "2026-01-02T03:04:05Z".parse().unwrap(),
        trace_parent: Some("00-abc-def-01".to_owned()),
        data: Some(json!({ "amount": 100 })),
        meta: Some(MetaWire {
            version: 1,
            producer_id: Some(uuid(3)),
            previous: Some(4),
            sequence: Some(5),
        }),
    };
    assert_eq!(
        serde_json::to_value(&wire).unwrap(),
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "type": "gts.cf.core.events.event.v1~acme.orders.created.v1~",
            "tenant_id": "00000000-0000-0000-0000-000000000002",
            "source": "orders",
            "subject": "order-1",
            "subject_type": "gts.cf.core.events.subject.v1~acme.orders.order.v1~",
            "occurred_at": "2026-01-02T03:04:05Z",
            "trace_parent": "00-abc-def-01",
            "data": { "amount": 100 },
            "meta": {
                "version": 1,
                "producer_id": "00000000-0000-0000-0000-000000000003",
                "previous": 4,
                "sequence": 5
            }
        })
    );
}

#[test]
fn publish_event_omits_absent_optionals() {
    let wire = PublishEventWire {
        id: uuid(1),
        type_id: gts::GtsTypeId::new("t"),
        tenant_id: uuid(2),
        source: "s".to_owned(),
        subject: "sub".to_owned(),
        subject_type: gts::GtsTypeId::new("st"),
        occurred_at: "2026-01-02T03:04:05Z".parse().unwrap(),
        trace_parent: None,
        data: None,
        meta: None,
    };
    assert_eq!(
        serde_json::to_value(&wire).unwrap(),
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "type": "t",
            "tenant_id": "00000000-0000-0000-0000-000000000002",
            "source": "s",
            "subject": "sub",
            "subject_type": "st",
            "occurred_at": "2026-01-02T03:04:05Z"
        })
    );
}

#[test]
fn register_producer_uses_snake_case_mode() {
    let wire = RegisterProducerWire {
        mode: ProducerModeWire::Chained,
        client_agent: "svc/1.0".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(&wire).unwrap(),
        json!({ "mode": "chained", "client_agent": "svc/1.0" })
    );
}

#[test]
fn interest_max_depth_is_untagged_int_or_null() {
    let unlimited = InterestWire {
        topic: gts::GtsInstanceId::new("gts.cf.core.events.topic.v1~acme.orders.v1", ""),
        tenant_id: uuid(2),
        max_depth: MaxDepthWire::Unlimited,
        barrier_mode: BarrierModeWire::Ignore,
        types: vec!["gts.cf.core.events.event.v1~acme.orders.*".to_owned()],
        filter: Some(FilterSpecWire {
            engine: gts::GtsInstanceId::new(
                "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
                "",
            ),
            expression: "event.data.amount > 100".to_owned(),
        }),
    };
    assert_eq!(
        serde_json::to_value(&unlimited).unwrap(),
        json!({
            "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
            "tenant_id": "00000000-0000-0000-0000-000000000002",
            "max_depth": null,
            "barrier_mode": "ignore",
            "types": ["gts.cf.core.events.event.v1~acme.orders.*"],
            "filter": {
                "engine": "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
                "expression": "event.data.amount > 100"
            }
        })
    );

    let levels = InterestWire {
        topic: gts::GtsInstanceId::new("t", ""),
        tenant_id: uuid(2),
        max_depth: MaxDepthWire::Levels(2),
        barrier_mode: BarrierModeWire::Respect,
        types: vec!["e".to_owned()],
        filter: None,
    };
    assert_eq!(
        serde_json::to_value(&levels).unwrap(),
        json!({
            "topic": "t",
            "tenant_id": "00000000-0000-0000-0000-000000000002",
            "max_depth": 2,
            "barrier_mode": "respect",
            "types": ["e"]
        })
    );
}

#[test]
fn seek_value_is_untagged_int_or_sentinel() {
    assert_eq!(
        serde_json::to_value(SeekValueWire::Exact(42)).unwrap(),
        json!(42)
    );
    assert_eq!(
        serde_json::to_value(SeekValueWire::Sentinel("earliest".to_owned())).unwrap(),
        json!("earliest")
    );
}

#[test]
fn seek_request_carries_topology_version() {
    let wire = SeekSubscriptionWire::new(
        7,
        &[crate::api::SeekPosition {
            topic: gts::GtsInstanceId::new("gts.cf.core.events.topic.v1~acme.orders.v1", ""),
            partition: 0,
            value: crate::api::Position::Earliest,
        }],
    );
    assert_eq!(
        serde_json::to_value(&wire).unwrap(),
        json!({
            "topology_version": 7,
            "positions": {
                "gts.cf.core.events.topic.v1~acme.orders.v1": [{
                    "partition": 0,
                    "value": "earliest"
                }]
            }
        })
    );
}

#[test]
fn event_type_response_deserialises() {
    let wire: EventTypeWire = serde_json::from_value(json!({
        "id": "gts.cf.core.events.event.v1~acme.orders.order.created.v1~",
        "topic": "gts.cf.core.events.topic.v1~acme.orders.order.stream.v1",
        "description": null,
        "allowed_subject_types": ["gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~"],
        "partition_key": "/tenant_id",
        "data_schema": { "type": "object" }
    }))
    .unwrap();
    assert_eq!(
        wire.id,
        "gts.cf.core.events.event.v1~acme.orders.order.created.v1~"
    );
    assert_eq!(
        wire.topic,
        "gts.cf.core.events.topic.v1~acme.orders.order.stream.v1"
    );
    assert_eq!(wire.description, None);
    assert_eq!(
        wire.allowed_subject_types,
        vec!["gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~".to_owned()]
    );
    assert_eq!(wire.partition_key, "/tenant_id");
    assert_eq!(wire.data_schema, json!({ "type": "object" }));
}

#[test]
fn page_envelope_deserialises() {
    let page: PageWire<TopicWire> = serde_json::from_value(json!({
        "items": [
            { "id": "gts.cf.core.events.topic.v1~acme.orders.order.stream.v1", "description": "Orders", "retention": "P7D" }
        ],
        "page_info": { "next_cursor": "abc", "prev_cursor": null, "limit": 50 }
    }))
    .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].id,
        "gts.cf.core.events.topic.v1~acme.orders.order.stream.v1"
    );
    assert_eq!(page.items[0].description, Some("Orders".to_owned()));
    assert_eq!(page.items[0].retention, Some("P7D".to_owned()));
    assert_eq!(page.page_info.next_cursor, Some("abc".to_owned()));
    assert_eq!(page.page_info.prev_cursor, None);
    assert_eq!(page.page_info.limit, 50);
}

#[test]
fn frame_event_decodes_into_wire_frame() {
    let frame: FrameWire = serde_json::from_value(json!({
        "kind": "event",
        "payload": {
            "id": "00000000-0000-0000-0000-000000000001",
            "type": "gts.cf.core.events.event.v1~acme.orders.order.created.v1~",
            "topic": "gts.cf.core.events.topic.v1~acme.orders.order.stream.v1",
            "tenant_id": "00000000-0000-0000-0000-000000000002",
            "source": "orders",
            "subject": "order-1",
            "subject_type": "gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~",
            "occurred_at": "2026-01-02T03:04:05Z",
            "trace_parent": null,
            "data": { "amount": 100 },
            "partition": 3,
            "sequence": 7,
            "sequence_time": "2026-01-02T03:04:06Z"
        }
    }))
    .unwrap();
    let out = frame.into_frame().unwrap();
    match out {
        crate::api::WireFrame::Event(e) => {
            assert_eq!(e.id(), uuid(1));
            assert_eq!(
                e.event_type(),
                "gts.cf.core.events.event.v1~acme.orders.order.created.v1~"
            );
            assert_eq!(e.partition(), Some(3));
            assert_eq!(e.sequence(), Some(crate::sequence::Sequence::assigned(7)));
            assert_eq!(e.data(), Some(&json!({ "amount": 100 })));
        }
        other => panic!("expected Event frame, got {other:?}"),
    }
}

/// An event frame with every member the server sends.
fn delivered_event_frame(overrides: serde_json::Value) -> FrameWire {
    let mut payload = json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "type": "gts.cf.core.events.event.v1~acme.orders.order.created.v1~",
        "topic": "gts.cf.core.events.topic.v1~acme.orders.order.stream.v1",
        "tenant_id": "00000000-0000-0000-0000-000000000002",
        "source": "order-service",
        "subject": "order-1",
        "subject_type": "gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~",
        "occurred_at": "2026-01-02T03:04:05Z",
        "trace_parent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        "data": { "amount": 100 },
        "partition": 2,
        "sequence": 7,
        "sequence_time": "2026-01-02T03:04:06Z"
    });
    let (Some(payload_members), Some(override_members)) =
        (payload.as_object_mut(), overrides.as_object())
    else {
        panic!("the frame payload and its overrides are JSON objects");
    };
    for (key, value) in override_members {
        payload_members.insert(key.clone(), value.clone());
    }
    serde_json::from_value(json!({ "kind": "event", "payload": payload })).unwrap()
}

#[test]
fn a_delivered_event_frame_keeps_its_source_and_position() {
    let frame = delivered_event_frame(json!({})).into_frame().unwrap();

    let crate::api::WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(event.id(), uuid(1));
    assert_eq!(
        event.event_type(),
        "gts.cf.core.events.event.v1~acme.orders.order.created.v1~"
    );
    assert_eq!(event.tenant_id(), uuid(2));
    assert_eq!(event.source(), Some("order-service"));
    assert_eq!(event.subject(), "order-1");
    assert_eq!(
        event.subject_type(),
        "gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~"
    );
    assert_eq!(
        event.occurred_at(),
        "2026-01-02T03:04:05Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
    );
    assert_eq!(
        event.trace_parent(),
        Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
    );
    assert_eq!(event.data(), Some(&json!({ "amount": 100 })));
    assert_eq!(event.partition(), Some(2));
    assert_eq!(event.sequence(), Some(Sequence::assigned(7)));
    assert_eq!(
        event.sequence_time(),
        Some(
            "2026-01-02T03:04:06Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        )
    );
    assert!(event.meta().is_none());
}

#[test]
fn a_delivered_event_with_null_data_has_no_data() {
    let frame = delivered_event_frame(json!({ "data": null }))
        .into_frame()
        .unwrap();

    let crate::api::WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(event.data(), None);
    assert_eq!(event.source(), Some("order-service"));
    assert_eq!(event.sequence(), Some(Sequence::assigned(7)));
}

#[test]
fn a_delivered_event_without_sequence_time_takes_occurred_at() {
    let frame = delivered_event_frame(json!({ "sequence_time": null }))
        .into_frame()
        .unwrap();

    let crate::api::WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(
        event.sequence_time(),
        Some(
            "2026-01-02T03:04:05Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        )
    );
    assert_eq!(event.partition(), Some(2));
    assert_eq!(event.sequence(), Some(Sequence::assigned(7)));
}

/// A stored event can predate ingest's `trace_parent` check, and the field is
/// optional telemetry: delivery drops a malformed one instead of failing the stream.
#[test]
fn a_delivered_event_with_a_malformed_trace_parent_is_delivered_without_it() {
    let frame = delivered_event_frame(json!({ "trace_parent": "00-abc-def-01" }))
        .into_frame()
        .unwrap();

    let crate::api::WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(event.id(), uuid(1));
    assert_eq!(
        event.event_type(),
        "gts.cf.core.events.event.v1~acme.orders.order.created.v1~"
    );
    assert_eq!(event.tenant_id(), uuid(2));
    assert_eq!(event.source(), Some("order-service"));
    assert_eq!(event.subject(), "order-1");
    assert_eq!(
        event.subject_type(),
        "gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~"
    );
    assert_eq!(
        event.occurred_at(),
        "2026-01-02T03:04:05Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
    );
    assert_eq!(event.trace_parent(), None);
    assert_eq!(event.data(), Some(&json!({ "amount": 100 })));
    assert_eq!(event.partition(), Some(2));
    assert_eq!(event.sequence(), Some(Sequence::assigned(7)));
    assert_eq!(
        event.sequence_time(),
        Some(
            "2026-01-02T03:04:06Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        )
    );
    assert!(event.meta().is_none());
}

#[test]
fn a_delivered_partition_outside_u32_is_a_transport_error() {
    let err = delivered_event_frame(json!({ "partition": -1 }))
        .into_frame()
        .unwrap_err();

    let EventBrokerError::Transport(text) = err else {
        panic!("expected a Transport error, got {err:?}");
    };
    assert_eq!(text, "stream event frame carried a partition outside u32");
}

#[test]
fn a_delivered_event_that_breaks_an_envelope_rule_is_a_transport_error() {
    const PLANTED: &str = "PLANTED-\u{e9}";
    // The builder's own error for this subject quotes no part of it, so wrapping
    // that error cannot leak the value either.
    let rule =
        EventBrokerError::invalid_text_field(&crate::validate::subject(PLANTED).unwrap_err())
            .to_string();
    assert_eq!(
        rule,
        "text field invalid: subject: must contain only printable ASCII (0x20-0x7E)"
    );

    let err = delivered_event_frame(json!({ "subject": PLANTED }))
        .into_frame()
        .unwrap_err();

    let EventBrokerError::Transport(text) = err else {
        panic!("expected a Transport error, got {err:?}");
    };
    // Names the field and holds no part of the planted value.
    assert_eq!(
        text,
        "stream event frame broke an envelope rule: text field invalid: subject: must contain only printable ASCII (0x20-0x7E)"
    );
}

#[test]
fn frame_control_decodes_code_and_reason() {
    let frame: FrameWire = serde_json::from_value(json!({
        "kind": "control",
        "code": "terminal",
        "positions": [
            { "topic": "gts.cf.core.events.topic.v1~acme.billing.orders.stream.v1", "partition": 0, "offset": 9, "last_examined": 9 }
        ],
        "reason": "rebalance"
    }))
    .unwrap();
    match frame.into_frame().unwrap() {
        crate::api::WireFrame::Control {
            code,
            positions,
            reason,
        } => {
            assert_eq!(code, crate::api::ControlCode::Terminal);
            assert_eq!(positions.len(), 1);
            assert_eq!(positions[0].partition, 0);
            assert_eq!(reason, Some("rebalance".to_owned()));
        }
        other => panic!("expected Control frame, got {other:?}"),
    }
}

#[test]
fn frame_event_missing_sequence_is_transport_error() {
    let frame: FrameWire = serde_json::from_value(json!({
        "kind": "event",
        "payload": {
            "id": "00000000-0000-0000-0000-000000000001",
            "type": "gts.cf.core.events.event.v1~acme.orders.order.created.v1~",
            "topic": "gts.cf.core.events.topic.v1~acme.orders.order.stream.v1",
            "tenant_id": "00000000-0000-0000-0000-000000000002",
            "source": "s",
            "subject": "sub",
            "subject_type": "gts.cf.core.events.subject.v1~acme.orders.order.entity.v1~",
            "occurred_at": "2026-01-02T03:04:05Z",
            "trace_parent": null,
            "data": null,
            "partition": 0,
            "sequence": null,
            "sequence_time": null
        }
    }))
    .unwrap();
    assert!(matches!(
        frame.into_frame(),
        Err(EventBrokerError::Transport(_))
    ));
}

/// The warning for a dropped `trace_parent` names the event and its type, never the
/// value: a stored value is producer input.
#[test]
fn a_malformed_trace_parent_warning_names_the_event_not_the_value() {
    let planted = "00-PLANTED-def-01";
    let frame = delivered_event_frame(json!({ "trace_parent": planted }));
    assert!(
        format!("{frame:?}").contains("PLANTED"),
        "the frame carries the planted value"
    );
    let capture = WarnCapture::install();

    let frame = frame.into_frame().unwrap();

    let crate::api::WireFrame::Event(event) = frame else {
        panic!("expected an Event frame, got {frame:?}");
    };
    assert_eq!(event.trace_parent(), None);
    assert_eq!(
        capture.lines(),
        vec![
            "WARN a delivered event carries a malformed trace_parent; delivering it without one \
             event_id=00000000-0000-0000-0000-000000000001 \
             type_id=gts.cf.core.events.event.v1~acme.orders.order.created.v1~"
                .to_owned()
        ]
    );
}
