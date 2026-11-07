//! Serde, accessor and builder behaviour of [`crate::gts::EventV1`]: the
//! server-stamped members are absent on a published event and present on a stored
//! one, [`crate::gts::EventV1Builder`] checks the envelope fields in `build()`, and
//! neither `Debug` prints the payload.

use chrono::{DateTime, TimeZone, Utc};
use gts::{GtsSchema, GtsTypeId};
use serde_json::json;
use uuid::Uuid;

use toolkit_canonical_errors::CanonicalError;

use crate::error::{DecodeCategory, EventBrokerError};
use crate::gts::{EventV1, EventV1Builder, StoredPosition};
use crate::gts_tests::{OrderPlacedV1, OrderSubjectV1};
use crate::models;
use crate::sequence::Sequence;

const EVENT_ID: Uuid = Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0001);
const TENANT_ID: Uuid = Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0002);
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~";
const SUBJECT_TYPE: &str = "gts.fabrikam.shop.orders.order.v1~";

const ORDER_ID: Uuid = Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0003);
const SKU_ID: Uuid = Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0004);
const TRACE: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
const ORDER_SUBJECT: &str = "gts.cf.core.events.subject.v1~fabrikam.shop.orders.order.v1~";
const PRODUCER_ID: Uuid = Uuid::from_u128(0x0193_0000_0000_7000_8000_0000_0000_0005);
const PLANTED: &str = "PLANTED-VALUE-7f3a";
const READ_ONLY_DETAIL: &str = "broker-stamped and read-only on publish";
const TRACE_PARENT_DETAIL: &str = "must be a W3C traceparent: four lowercase hex groups of 2, 32, \
                                   16 and 2 digits joined by '-'";

fn occurred_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 6, 10, 0, 0).unwrap()
}

fn stored_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 6, 10, 0, 1).unwrap()
}

type OrderPlaced = EventV1<OrderPlacedV1>;

/// A payload declared by hand: a serde type with its own field naming.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkuPublishedPayload {
    sku_id: Uuid,
    version: i64,
}

impl GtsSchema for SkuPublishedPayload {
    const TYPE_ID: &'static str =
        "gts.cf.core.events.event.v1~fabrikam.shop.catalog.sku_published.v1~";

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

fn order_placed() -> OrderPlacedV1 {
    OrderPlacedV1 {
        order_id: ORDER_ID,
        total_cents: 4299,
        currency: "EUR".to_owned(),
        coupon_code: None,
    }
}

fn published_body() -> serde_json::Value {
    json!({
        "id": EVENT_ID,
        "type": EVENT_TYPE,
        "tenant_id": TENANT_ID,
        "source": "orders-service",
        "subject": "order-1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-11-06T10:00:00Z",
        "data": { "order_id": "o-1" },
    })
}

#[test]
fn a_published_event_deserializes_without_its_server_stamped_members() {
    let event: EventV1 = serde_json::from_value(published_body()).unwrap();

    assert_eq!(event.id(), EVENT_ID);
    assert_eq!(event.event_type(), &GtsTypeId::new(EVENT_TYPE));
    assert_eq!(event.tenant_id(), TENANT_ID);
    assert_eq!(event.source(), Some("orders-service"));
    assert_eq!(event.subject(), "order-1");
    assert_eq!(event.subject_type(), &GtsTypeId::new(SUBJECT_TYPE));
    assert_eq!(event.occurred_at(), occurred_at());
    assert_eq!(event.trace_parent(), None);
    assert_eq!(event.data(), Some(&json!({ "order_id": "o-1" })));
    assert_eq!(event.partition(), None);
    assert_eq!(event.sequence(), None);
    assert_eq!(event.sequence_time(), None);
    assert!(event.meta().is_none());
}

#[test]
fn a_stored_event_round_trips_with_its_stamped_members() {
    let mut body = published_body();
    body["partition"] = json!(3);
    body["sequence"] = json!(42);
    body["sequence_time"] = json!("2026-11-06T10:00:01Z");

    let event: EventV1 = serde_json::from_value(body).unwrap();

    assert_eq!(event.partition(), Some(3));
    assert_eq!(event.sequence(), Some(Sequence::assigned(42)));
    assert_eq!(
        event.sequence_time(),
        Some(Utc.with_ymd_and_hms(2026, 11, 6, 10, 0, 1).unwrap())
    );
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "data": { "order_id": "o-1" },
            "partition": 3,
            "sequence": 42,
            "sequence_time": "2026-11-06T10:00:01Z",
            "meta": null,
        })
    );
}

#[test]
fn an_unstamped_event_serializes_without_its_server_stamped_members() {
    let event: EventV1 = serde_json::from_value(published_body()).unwrap();

    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "data": { "order_id": "o-1" },
            "meta": null,
        })
    );
}

#[test]
fn a_body_less_event_has_no_data() {
    let mut absent = published_body();
    absent.as_object_mut().unwrap().remove("data");
    let absent: EventV1 = serde_json::from_value(absent).unwrap();
    assert_eq!(absent.data(), None);

    let mut null = published_body();
    null["data"] = serde_json::Value::Null;
    let null: EventV1 = serde_json::from_value(null).unwrap();
    assert_eq!(null.data(), None);
}

#[test]
fn debug_names_the_payload_type_but_never_prints_it() {
    let mut body = published_body();
    body["data"] = json!({ "secret": "PLANTED-VALUE-7f3a" });
    let event: EventV1 = serde_json::from_value(body).unwrap();

    let printed = format!("{event:?}");

    assert!(
        printed.contains(std::any::type_name::<serde_json::Value>()),
        "names the payload type: {printed}"
    );
    assert!(
        printed.contains("order-1"),
        "prints the envelope: {printed}"
    );
    assert!(
        !printed.contains("PLANTED-VALUE-7f3a"),
        "never prints the payload: {printed}"
    );
}

#[test]
fn a_typed_event_takes_its_type_from_the_payload() {
    let event = OrderPlaced::builder(order_placed())
        .id(EVENT_ID)
        .tenant_id(TENANT_ID)
        .source("orders-service")
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .occurred_at(occurred_at())
        .trace_parent(TRACE)
        .build()
        .unwrap();

    assert_eq!(event.event_type(), &GtsTypeId::new(EVENT_TYPE));
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": ORDER_SUBJECT,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": TRACE,
            "data": { "order_id": ORDER_ID, "total_cents": 4299, "currency": "EUR", "coupon_code": null },
            "meta": null,
        })
    );
}

#[test]
fn an_id_and_occurred_at_default_once_at_build() {
    let before = Utc::now();
    let event = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .build()
        .unwrap();
    let after = Utc::now();

    assert_eq!(event.id().get_version(), Some(uuid::Version::SortRand));
    assert!(
        before <= event.occurred_at() && event.occurred_at() <= after,
        "occurred_at {} is the build time, between {before} and {after}",
        event.occurred_at()
    );
    let first = serde_json::to_value(&event).unwrap();
    let second = serde_json::to_value(&event).unwrap();
    assert_eq!(
        (&first["id"], &first["occurred_at"]),
        (&second["id"], &second["occurred_at"])
    );
}

#[test]
fn building_without_a_required_field_names_it() {
    let Err(EventBrokerError::InvalidEventField { field, detail }) =
        OrderPlaced::builder(order_placed())
            .id(EVENT_ID)
            .source("orders-service")
            .subject("order-1")
            .subject_type::<OrderSubjectV1>()
            .occurred_at(occurred_at())
            .build()
    else {
        panic!("an event without a tenant must be refused as an invalid field");
    };
    assert_eq!((field, detail.as_str()), ("tenant_id", "required"));

    let Err(EventBrokerError::InvalidEventField { field, detail }) =
        OrderPlaced::builder(order_placed())
            .id(EVENT_ID)
            .tenant_id(TENANT_ID)
            .source("orders-service")
            .subject_type::<OrderSubjectV1>()
            .occurred_at(occurred_at())
            .build()
    else {
        panic!("an event without a subject must be refused as an invalid field");
    };
    assert_eq!((field, detail.as_str()), ("subject", "required"));

    let Err(EventBrokerError::InvalidEventField { field, detail }) =
        OrderPlaced::builder(order_placed())
            .id(EVENT_ID)
            .tenant_id(TENANT_ID)
            .source("orders-service")
            .subject("order-1")
            .occurred_at(occurred_at())
            .build()
    else {
        panic!("an event without a subject type must be refused as an invalid field");
    };
    assert_eq!((field, detail.as_str()), ("subject_type", "required"));
}

#[test]
fn an_untyped_payload_needs_its_type_named() {
    let Err(EventBrokerError::InvalidEventField { field, detail }) =
        EventV1::builder(json!({ "k": 1 }))
            .id(EVENT_ID)
            .tenant_id(TENANT_ID)
            .source("orders-service")
            .subject("order-1")
            .subject_type::<OrderSubjectV1>()
            .occurred_at(occurred_at())
            .build()
    else {
        panic!("an untyped payload carries no type, so the event must be refused");
    };
    assert_eq!((field, detail.as_str()), ("type", "required"));

    let named = EventV1::builder_for(GtsTypeId::new(EVENT_TYPE), json!({ "order_id": "o-1" }))
        .id(EVENT_ID)
        .tenant_id(TENANT_ID)
        .source("orders-service")
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .occurred_at(occurred_at())
        .build()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&named).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": ORDER_SUBJECT,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "data": { "order_id": "o-1" },
            "meta": null,
        })
    );

    let body_less = EventV1::builder_without_data(GtsTypeId::new(EVENT_TYPE))
        .id(EVENT_ID)
        .tenant_id(TENANT_ID)
        .source("orders-service")
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .occurred_at(occurred_at())
        .build()
        .unwrap();
    assert_eq!(body_less.data(), None);
    assert_eq!(
        serde_json::to_value(&body_less).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": ORDER_SUBJECT,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "meta": null,
        })
    );
}

#[test]
fn a_subject_type_comes_from_a_declared_kind_or_an_id() {
    let declared = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .build()
        .unwrap();
    assert_eq!(declared.subject_type(), &GtsTypeId::new(ORDER_SUBJECT));

    let named = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .subject("order-1")
        .subject_type_id(GtsTypeId::new("gts.acme.av.vendor.v1~"))
        .build()
        .unwrap();
    assert_eq!(
        named.subject_type(),
        &GtsTypeId::new("gts.acme.av.vendor.v1~")
    );

    let last_wins = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .subject_type_id(GtsTypeId::new("gts.acme.av.vendor.v1~"))
        .build()
        .unwrap();
    assert_eq!(
        last_wins.subject_type(),
        &GtsTypeId::new("gts.acme.av.vendor.v1~")
    );

    let Err(EventBrokerError::InvalidEventField { field, detail }) =
        OrderPlaced::builder(order_placed())
            .tenant_id(TENANT_ID)
            .subject("order-1")
            .subject_type::<serde_json::Value>()
            .build()
    else {
        panic!("a subject kind with no type id must be refused as an invalid field");
    };
    assert_eq!((field, detail.as_str()), ("subject_type", "required"));
}

#[test]
fn source_is_optional_and_checked_when_set() {
    let unsourced = OrderPlaced::builder(order_placed())
        .id(EVENT_ID)
        .tenant_id(TENANT_ID)
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .occurred_at(occurred_at())
        .build()
        .unwrap();
    assert_eq!(unsourced.source(), None);
    assert_eq!(
        serde_json::to_value(&unsourced).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "subject": "order-1",
            "subject_type": ORDER_SUBJECT,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "data": { "order_id": ORDER_ID, "total_cents": 4299, "currency": "EUR", "coupon_code": null },
            "meta": null,
        })
    );

    let sourced = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .source("orders-service")
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .build()
        .unwrap();
    assert_eq!(sourced.source(), Some("orders-service"));

    let Err(err) = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .source("x".repeat(257))
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .build()
    else {
        panic!("a 257-byte source must be refused");
    };
    let EventBrokerError::InvalidTextField {
        field,
        detail,
        reason,
    } = err
    else {
        panic!("a source over its length bound is a text-field error: {err:?}");
    };
    assert_eq!(
        (field, detail.as_str(), reason),
        ("source", "must be 1-256 bytes, got 257", "field_too_long")
    );
}

#[test]
fn subject_text_rules_apply_at_build() {
    let Err(err) = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .subject("")
        .subject_type::<OrderSubjectV1>()
        .build()
    else {
        panic!("an empty subject must be refused");
    };
    let EventBrokerError::InvalidTextField {
        field,
        detail,
        reason,
    } = err
    else {
        panic!("an empty subject is a text-field error: {err:?}");
    };
    assert_eq!(
        (field, detail.as_str(), reason),
        ("subject", "must be 1-1024 bytes, got 0", "field_too_long")
    );

    let Err(err) = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .subject("PLANTED-\u{e9}")
        .subject_type::<OrderSubjectV1>()
        .build()
    else {
        panic!("a non-ASCII subject must be refused");
    };
    let displayed = format!("{err}");
    let debugged = format!("{err:?}");
    let EventBrokerError::InvalidTextField {
        field,
        detail,
        reason,
    } = err
    else {
        panic!("a non-ASCII subject is a text-field error: {debugged}");
    };
    assert_eq!(
        (field, detail.as_str(), reason),
        (
            "subject",
            "must contain only printable ASCII (0x20-0x7E)",
            "ascii_only"
        )
    );
    assert!(
        !displayed.contains("PLANTED") && !debugged.contains("PLANTED"),
        "the error never carries the submitted value: {displayed} / {debugged}"
    );
}

#[test]
fn a_trace_parent_must_be_a_w3c_traceparent() {
    let traced = OrderPlaced::builder(order_placed())
        .tenant_id(TENANT_ID)
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .trace_parent(TRACE)
        .build()
        .unwrap();
    assert_eq!(traced.trace_parent(), Some(TRACE));

    // Each value breaks exactly one rule: uppercase hex, a short group, a fifth group.
    for invalid in [
        "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01",
        "00-0af7-b7ad6b7169203331-01",
        "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-ff",
    ] {
        let Err(EventBrokerError::InvalidEventField { field, detail }) =
            OrderPlaced::builder(order_placed())
                .tenant_id(TENANT_ID)
                .subject("order-1")
                .subject_type::<OrderSubjectV1>()
                .trace_parent(invalid)
                .build()
        else {
            panic!("trace parent {invalid} must be refused as an invalid field");
        };
        assert_eq!(
            (field, detail.as_str()),
            ("trace_parent", TRACE_PARENT_DETAIL),
            "{invalid}"
        );
    }
}

#[test]
fn event_type_id_is_the_payloads_and_type_id_is_the_bases() {
    assert_eq!(OrderPlaced::EVENT_TYPE_ID, EVENT_TYPE);
    assert_eq!(
        <OrderPlaced as GtsSchema>::TYPE_ID,
        "gts.cf.core.events.event.v1~"
    );
    assert_eq!(<EventV1>::EVENT_TYPE_ID, "");
    assert_eq!(
        EventV1::<SkuPublishedPayload>::EVENT_TYPE_ID,
        SkuPublishedPayload::TYPE_ID
    );
}

#[test]
fn a_hand_declared_payload_keeps_its_own_field_names() {
    let event = EventV1::builder(SkuPublishedPayload {
        sku_id: SKU_ID,
        version: 3,
    })
    .id(EVENT_ID)
    .tenant_id(TENANT_ID)
    .source("catalog-service")
    .subject("sku-1")
    .subject_type_id(GtsTypeId::new("gts.fabrikam.shop.catalog.sku.v1~"))
    .occurred_at(occurred_at())
    .build()
    .unwrap();

    assert_eq!(
        event.event_type(),
        &GtsTypeId::new(SkuPublishedPayload::TYPE_ID)
    );
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": "gts.cf.core.events.event.v1~fabrikam.shop.catalog.sku_published.v1~",
            "tenant_id": TENANT_ID,
            "source": "catalog-service",
            "subject": "sku-1",
            "subject_type": "gts.fabrikam.shop.catalog.sku.v1~",
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "data": { "skuId": SKU_ID, "version": 3 },
            "meta": null,
        })
    );
}

#[test]
fn builder_debug_names_the_payload_type_but_never_prints_it() {
    let builder = EventV1::builder_for(
        GtsTypeId::new(EVENT_TYPE),
        json!({ "secret": "PLANTED-VALUE-7f3a" }),
    )
    .subject("order-1");

    let printed = format!("{builder:?}");

    assert!(
        printed.contains(std::any::type_name::<serde_json::Value>()),
        "names the payload type: {printed}"
    );
    assert!(
        printed.contains("order-1"),
        "prints the envelope: {printed}"
    );
    assert!(
        !printed.contains("PLANTED-VALUE-7f3a"),
        "never prints the payload: {printed}"
    );
}

/// `published_body()` carrying `data` instead of its own.
fn published_with_data(data: serde_json::Value) -> EventV1 {
    let mut body = published_body();
    body["data"] = data;
    serde_json::from_value(body).unwrap()
}

/// `published_body()` with `member` set to `value`.
fn published_with(member: &str, value: serde_json::Value) -> EventV1 {
    let mut body = published_body();
    body[member] = value;
    serde_json::from_value(body).unwrap()
}

/// `published_body()` without its `source`.
fn published_without_source() -> serde_json::Value {
    let mut body = published_body();
    body.as_object_mut().unwrap().remove("source");
    body
}

#[test]
fn decode_gives_a_typed_view_and_leaves_the_raw_event() {
    let raw = published_with_data(json!({
        "order_id": ORDER_ID,
        "total_cents": 4299,
        "currency": "EUR",
        "coupon_code": null,
    }));

    let typed: OrderPlaced = raw.decode::<OrderPlacedV1>().unwrap();

    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "data": { "order_id": ORDER_ID, "total_cents": 4299, "currency": "EUR", "coupon_code": null },
            "meta": null,
        })
    );
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        serde_json::to_value(&raw).unwrap()
    );
    assert_eq!(
        raw.data(),
        Some(&json!({
            "order_id": ORDER_ID,
            "total_cents": 4299,
            "currency": "EUR",
            "coupon_code": null,
        }))
    );
}

/// A `#[gts_type_schema]` payload's generated deserializer requires every non-generic
/// field and ignores `#[serde(default)]`: `null` reads as `None`, an absent field fails.
/// The worked example's schema requires `coupon_code`, so the broker refuses such an
/// event before a consumer reads it.
#[test]
fn a_macro_payload_reads_every_field_as_present() {
    let typed = published_with_data(json!({
        "order_id": ORDER_ID,
        "total_cents": 4299,
        "currency": "EUR",
        "coupon_code": null,
    }))
    .decode::<OrderPlacedV1>()
    .unwrap();
    let Some(payload) = typed.data() else {
        panic!("a null optional field decodes with its data");
    };
    assert_eq!(
        (
            payload.order_id,
            payload.total_cents,
            payload.currency.as_str(),
            payload.coupon_code.as_deref()
        ),
        (ORDER_ID, 4299, "EUR", None)
    );

    let absent = published_with_data(
        json!({ "order_id": ORDER_ID, "total_cents": 4299, "currency": "EUR" }),
    )
    .decode::<OrderPlacedV1>();

    let Err(err) = absent else {
        panic!("an absent optional field must not decode");
    };
    let EventBrokerError::Decode { type_id, category } = &err else {
        panic!("expected a decode error, got {err:?}");
    };
    assert_eq!(
        (type_id.as_str(), *category),
        (EVENT_TYPE, DecodeCategory::Data)
    );
}

/// The macro omits an absent generic slot, so the typed body has no `data` key.
#[test]
fn a_body_less_event_decodes_without_data() {
    let mut body = published_body();
    body.as_object_mut().unwrap().remove("data");
    let raw: EventV1 = serde_json::from_value(body).unwrap();

    let typed = raw.decode::<OrderPlacedV1>().unwrap();

    assert!(typed.data().is_none());
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "meta": null,
        })
    );
}

#[test]
fn data_that_does_not_fit_is_a_value_free_decode_error() {
    let raw = published_with_data(json!({
        "order_id": ORDER_ID,
        "total_cents": PLANTED,
        "currency": "EUR",
        "coupon_code": null,
    }));
    let raw_error = <OrderPlacedV1 as ::gts::GtsDeserialize>::gts_deserialize(
        raw.data().expect("the planted body has data"),
    )
    .map(|_| ())
    .expect_err("the planted body must not deserialize");
    assert!(
        raw_error.to_string().contains(PLANTED),
        "precondition: serde quotes the plant: {raw_error}"
    );

    let Err(err) = raw.decode::<OrderPlacedV1>() else {
        panic!("a total_cents that is not an integer must not decode");
    };

    let EventBrokerError::Decode { type_id, category } = &err else {
        panic!("expected a decode error, got {err:?}");
    };
    assert_eq!(
        (type_id.as_str(), *category),
        (EVENT_TYPE, DecodeCategory::Data)
    );
    let display = format!("{err}");
    assert_eq!(
        display,
        "event data of gts.cf.core.events.event.v1~fabrikam.shop.orders.order_placed.v1~ \
         does not decode as the payload type (data)"
    );
    let debug = format!("{err:?}");
    assert!(!debug.contains("PLANTED"), "{debug}");
    let canonical = format!("{:?}", CanonicalError::from(err));
    assert!(!canonical.contains("PLANTED"), "{canonical}");
}

#[test]
fn a_hand_declared_payload_decodes_with_its_own_field_names() {
    let raw = published_with_data(json!({ "skuId": SKU_ID, "version": 3 }));

    let typed = raw.decode::<SkuPublishedPayload>().unwrap();

    let Some(payload) = typed.data() else {
        panic!("the payload decodes with its data");
    };
    assert_eq!((payload.sku_id, payload.version), (SKU_ID, 3));
}

#[test]
fn a_typed_event_converts_to_its_raw_form() {
    let typed = OrderPlaced::builder(order_placed())
        .id(EVENT_ID)
        .tenant_id(TENANT_ID)
        .source("orders-service")
        .subject("order-1")
        .subject_type::<OrderSubjectV1>()
        .occurred_at(occurred_at())
        .trace_parent(TRACE)
        .build()
        .unwrap();

    let raw: EventV1 = EventV1::try_from(&typed).unwrap();

    assert_eq!(
        serde_json::to_value(&raw).unwrap(),
        serde_json::to_value(&typed).unwrap()
    );
    assert_eq!(raw.event_type(), typed.event_type());
    assert_eq!(
        serde_json::to_value(raw.decode::<OrderPlacedV1>().unwrap()).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": ORDER_SUBJECT,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": TRACE,
            "data": { "order_id": ORDER_ID, "total_cents": 4299, "currency": "EUR", "coupon_code": null },
            "meta": null,
        })
    );
}

/// `Clone` copies every member, the stamped ones and `meta` included: a transport
/// borrows an event, and the batch path and the test doubles copy it.
#[test]
fn a_cloned_event_keeps_every_member() {
    let event: EventV1 = serde_json::from_value(json!({
        "id": EVENT_ID,
        "type": EVENT_TYPE,
        "tenant_id": TENANT_ID,
        "source": "orders-service",
        "subject": "order-1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-11-06T10:00:00Z",
        "trace_parent": TRACE,
        "data": { "order_id": "o-1" },
        "partition": 3,
        "sequence": 42,
        "sequence_time": "2026-11-06T10:00:01Z",
        "meta": { "version": 1, "producer_id": PRODUCER_ID, "previous": 4, "sequence": 5 },
    }))
    .unwrap();

    assert_eq!(
        serde_json::to_value(event.clone()).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": TRACE,
            "data": { "order_id": "o-1" },
            "partition": 3,
            "sequence": 42,
            "sequence_time": "2026-11-06T10:00:01Z",
            "meta": { "version": 1, "producer_id": PRODUCER_ID, "previous": 4, "sequence": 5 },
        })
    );
}

/// A producer resolves the partition key of an `EventV1` exactly as the backend record
/// [`models::Event`] does for the same members: the same string, or the same error.
#[test]
fn the_raw_event_resolves_a_partition_key_as_the_backend_record_does() {
    let data = json!({
        "order_id": "o-1",
        "count": 7,
        "flag": true,
        "none": null,
        "nested": { "line": 1 },
    });
    let event = published_with_data(data.clone());
    let record = models::Event {
        id: EVENT_ID,
        type_id: GtsTypeId::new(EVENT_TYPE),
        tenant_id: TENANT_ID,
        source: "orders-service".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: GtsTypeId::new(SUBJECT_TYPE),
        occurred_at: occurred_at(),
        trace_parent: None,
        data: Some(data),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    };
    let internal = |result: Result<String, EventBrokerError>| match result {
        Ok(key) => Ok(key),
        Err(EventBrokerError::Internal(detail)) => Err(detail),
        Err(other) => panic!("expected an `Internal` error, got {other:?}"),
    };
    let pointers = [
        "/tenant_id",
        "/subject",
        "/type",
        "/data/order_id",
        "/data/count",
        "/data/flag",
        "/data/none",
        "/data/nested",
        "/data/missing",
    ];

    let from_event: Vec<_> = pointers
        .iter()
        .map(|pointer| internal(event.partition_input(pointer)))
        .collect();
    let from_record: Vec<_> = pointers
        .iter()
        .map(|pointer| internal(record.partition_input(pointer)))
        .collect();

    let expected: Vec<Result<String, String>> = vec![
        Ok("01930000-0000-7000-8000-000000000002".to_owned()),
        Ok("order-1".to_owned()),
        Ok(EVENT_TYPE.to_owned()),
        Ok("o-1".to_owned()),
        Ok("7".to_owned()),
        Ok("true".to_owned()),
        Err("partition-key pointer `/data/none` resolves to null".to_owned()),
        Err(
            "partition-key pointer `/data/nested` resolves to an object or array, which has \
             no stable hash input"
                .to_owned(),
        ),
        Err("partition-key pointer `/data/missing` resolves to no member of the event".to_owned()),
    ];
    assert_eq!(from_event, expected);
    assert_eq!(from_record, expected);
}

#[test]
fn stamping_a_position_sets_the_three_server_members() {
    let event: EventV1 = serde_json::from_value(published_body()).unwrap();

    let stored = event.stamp_position(StoredPosition {
        partition: 3,
        sequence: Sequence::assigned(42),
        sequence_time: stored_at(),
    });

    assert_eq!(
        (
            stored.partition(),
            stored.sequence(),
            stored.sequence_time()
        ),
        (Some(3), Some(Sequence::assigned(42)), Some(stored_at()))
    );
    assert_eq!(
        serde_json::to_value(&stored).unwrap(),
        json!({
            "id": EVENT_ID,
            "type": EVENT_TYPE,
            "tenant_id": TENANT_ID,
            "source": "orders-service",
            "subject": "order-1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-11-06T10:00:00Z",
            "trace_parent": null,
            "data": { "order_id": "o-1" },
            "partition": 3,
            "sequence": 42,
            "sequence_time": "2026-11-06T10:00:01Z",
            "meta": null,
        })
    );
}

#[test]
fn check_publishable_refuses_stamped_members_and_a_missing_source() {
    let built = EventV1::builder_for(GtsTypeId::new(EVENT_TYPE), json!({ "order_id": "o-1" }))
        .tenant_id(TENANT_ID)
        .source("orders-service")
        .subject("order-1")
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .unwrap();
    built.check_publishable().unwrap();

    for (member, value) in [
        ("partition", json!(3)),
        ("sequence", json!(42)),
        ("sequence_time", json!("2026-11-06T10:00:01Z")),
    ] {
        let Err(EventBrokerError::InvalidEventField { field, detail }) =
            published_with(member, value).check_publishable()
        else {
            panic!("an event carrying {member} must not be publishable");
        };
        assert_eq!((field, detail.as_str()), (member, READ_ONLY_DETAIL));
    }

    let sourceless: EventV1 = serde_json::from_value(published_without_source()).unwrap();
    let Err(EventBrokerError::InvalidEventField { field, detail }) = sourceless.check_publishable()
    else {
        panic!("an event without a source must not be publishable");
    };
    assert_eq!((field, detail.as_str()), ("source", "required"));
}

#[test]
fn check_producer_input_refuses_stamped_members_meta_and_a_source() {
    let built = EventV1::builder_for(GtsTypeId::new(EVENT_TYPE), json!({ "order_id": "o-1" }))
        .tenant_id(TENANT_ID)
        .subject("order-1")
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .unwrap();
    built.check_producer_input().unwrap();

    let mut stamped = published_without_source();
    stamped["partition"] = json!(3);
    let stamped: EventV1 = serde_json::from_value(stamped).unwrap();
    let Err(EventBrokerError::InvalidEventField { field, detail }) = stamped.check_producer_input()
    else {
        panic!("a stamped event is not producer input");
    };
    assert_eq!((field, detail.as_str()), ("partition", READ_ONLY_DETAIL));

    let mut with_meta = published_without_source();
    with_meta["meta"] =
        json!({ "version": 1, "producer_id": null, "previous": null, "sequence": null });
    let with_meta: EventV1 = serde_json::from_value(with_meta).unwrap();
    let Err(EventBrokerError::InvalidEventField { field, detail }) =
        with_meta.check_producer_input()
    else {
        panic!("an event carrying meta is not producer input");
    };
    assert_eq!((field, detail.as_str()), ("meta", "the producer stamps it"));

    let with_source: EventV1 = serde_json::from_value(published_body()).unwrap();
    let Err(EventBrokerError::InvalidEventField { field, detail }) =
        with_source.check_producer_input()
    else {
        panic!("an event carrying a source is not producer input");
    };
    assert_eq!(
        (field, detail.as_str()),
        ("source", "the producer stamps it")
    );
}

/// The builder that makes `published_body()`.
fn published_builder() -> EventV1Builder<serde_json::Value> {
    EventV1::builder_for(GtsTypeId::new(EVENT_TYPE), json!({ "order_id": "o-1" }))
        .id(EVENT_ID)
        .tenant_id(TENANT_ID)
        .source("orders-service")
        .subject("order-1")
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .occurred_at(occurred_at())
}

/// A decoded event skips the builder, so `check_envelope` applies the builder's value
/// rules a decoded event can break, with the builder's errors. It does not re-check
/// `type` or `subject_type`: an empty one never decodes, because `GtsTypeId` refuses
/// an empty id.
#[test]
fn a_decoded_event_is_checked_against_the_envelope_rules() {
    serde_json::from_value::<EventV1>(published_body())
        .unwrap()
        .check_envelope()
        .unwrap();
    for member in ["type", "subject_type"] {
        let mut body = published_body();
        body[member] = json!("");
        let refused = serde_json::from_value::<EventV1>(body).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "Invalid GTS identifier: : must start with 'gts.'",
            "{member}"
        );
    }

    let decoded = published_with("subject", json!("PLANTED-\u{e9}")).check_envelope();
    let built = published_builder().subject("PLANTED-\u{e9}").build();
    for (path, outcome) in [("decoded", decoded), ("built", built.map(|_| ()))] {
        let Err(EventBrokerError::InvalidTextField {
            field,
            detail,
            reason,
        }) = outcome
        else {
            panic!("{path}: a non-ASCII subject must be refused, got {outcome:?}");
        };
        assert_eq!(
            (field, detail.as_str(), reason),
            (
                "subject",
                "must contain only printable ASCII (0x20-0x7E)",
                "ascii_only"
            ),
            "{path}"
        );
    }

    let decoded = published_with("trace_parent", json!("00-abc-def-01")).check_envelope();
    let built = published_builder().trace_parent("00-abc-def-01").build();
    for (path, outcome) in [("decoded", decoded), ("built", built.map(|_| ()))] {
        let Err(EventBrokerError::InvalidEventField { field, detail }) = outcome else {
            panic!("{path}: a malformed trace parent must be refused, got {outcome:?}");
        };
        assert_eq!(
            (field, detail.as_str()),
            ("trace_parent", TRACE_PARENT_DETAIL),
            "{path}"
        );
    }

    let decoded = published_with("source", json!("PLANTED-\u{e9}")).check_envelope();
    let built = published_builder().source("PLANTED-\u{e9}").build();
    for (path, outcome) in [("decoded", decoded), ("built", built.map(|_| ()))] {
        let Err(EventBrokerError::InvalidTextField {
            field,
            detail,
            reason,
        }) = outcome
        else {
            panic!("{path}: a non-ASCII source must be refused, got {outcome:?}");
        };
        assert_eq!(
            (field, detail.as_str(), reason),
            (
                "source",
                "must contain only printable ASCII (0x20-0x7E)",
                "ascii_only"
            ),
            "{path}"
        );
    }
}

#[test]
fn a_body_without_source_deserializes_with_none() {
    let event: EventV1 = serde_json::from_value(published_without_source()).unwrap();

    assert_eq!(event.source(), None);
}
