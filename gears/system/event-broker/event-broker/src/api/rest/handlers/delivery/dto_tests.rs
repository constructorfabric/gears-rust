//! What a sequence looks like once it reaches the wire.
//!
//! `Sequence` is a newtype over `i64` and the serde representation is meant to
//! be indistinguishable from the integer it wraps. That claim is only checkable
//! where a sequence actually renders, which is here: the SDK's own `Event`
//! derives nothing but `Debug` and `Clone`, deliberately, because wire
//! (de)serialization is the transport's concern rather than the model's.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

use super::dto::{CreateConsumerGroupRequest, EventPayloadDto, JoinSubscriptionRequest};
use crate::domain::model::{Event, Sequence};

fn occurred_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0)
        .single()
        .expect("a fixed instant")
}

fn event(sequence: Option<Sequence>) -> Event {
    Event {
        id: Uuid::nil(),
        r#type: crate::test_support::event_type_id(
            "gts.cf.core.events.event.v1~x.eb.orders.created.v1~",
        ),
        topic: toolkit_gts::GtsInstanceId::try_new(
            "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1",
        )
        .expect("static gts id is valid"),
        tenant_id: Uuid::nil(),
        source: "wire".to_owned(),
        subject: "order-1".to_owned(),
        subject_type: gts::GtsTypeId::new("gts.x.eb.subject.v1~"),
        occurred_at: occurred_at(),
        trace_parent: None,
        data: json!({ "n": 1 }),
        meta: None,
        partition: Some(0),
        sequence,
        sequence_time: None,
    }
}

/// A stamped sequence renders as a bare JSON number, not as an object or a
/// string - the whole body is asserted so a wrapper appearing anywhere in it
/// would fail here.
#[test]
fn a_stamped_sequence_renders_as_a_bare_number() {
    let dto = EventPayloadDto::from(event(Some(Sequence::assigned(42))));

    assert_eq!(
        serde_json::to_value(&dto).expect("the DTO must serialise"),
        json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "type": "gts.cf.core.events.event.v1~x.eb.orders.created.v1~",
            "topic": "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1",
            "tenant_id": "00000000-0000-0000-0000-000000000000",
            "source": "wire",
            "subject": "order-1",
            "subject_type": "gts.x.eb.subject.v1~",
            "occurred_at": "2026-09-07T12:00:00Z",
            "trace_parent": null,
            "data": { "n": 1 },
            "partition": 0,
            "sequence": 42,
            "sequence_time": null,
        })
    );
}

/// An event the broker has not yet stamped renders `null`, which is what makes
/// "no sequence" distinguishable from `Sequence::NONE` on the wire.
#[test]
fn an_unstamped_sequence_renders_null() {
    let dto = EventPayloadDto::from(event(None));

    assert_eq!(
        serde_json::to_value(&dto).expect("the DTO must serialise")["sequence"],
        json!(null)
    );
}

/// The type is 64-bit and stays 64-bit across the crossing: a value past
/// `i32::MAX` survives rather than truncating or being emitted as a string.
#[test]
fn a_sequence_beyond_32_bits_survives_the_crossing() {
    let beyond = i64::from(i32::MAX) + 1;
    let dto = EventPayloadDto::from(event(Some(Sequence::assigned(beyond))));

    assert_eq!(
        serde_json::to_value(&dto).expect("the DTO must serialise")["sequence"],
        json!(2_147_483_648_i64)
    );
}

// -- the request types' declared bounds agree with the validators -----------
//
// `event_broker_sdk::validate` enforces a field's bounds; the request type
// declares the same bounds in the `OpenAPI` it publishes. Two statements of one
// contract, so each test reads the bound from the type's generated schema and
// probes the validator at it: a bound edited on one side fails here.

use event_broker_sdk::validate::{Rule, TextFieldError, client_agent, description};
use utoipa::PartialSchema;

/// The generated schema of one request field.
fn declared_field<T: PartialSchema>(field: &str) -> serde_json::Value {
    let schema = serde_json::to_value(T::schema()).expect("the schema must serialise");
    schema["properties"][field].clone()
}

fn rule_of(result: Result<(), TextFieldError>) -> Rule {
    result.expect_err("the value must be rejected").rule()
}

/// Probes `validate` at the bounds `node` declares: the maximum is accepted, one
/// byte past it is a `Length` rejection, and the minimum decides whether empty
/// is accepted.
fn agrees_with(node: &serde_json::Value, validate: fn(&str) -> Result<(), TextFieldError>) {
    let bound = |key: &str| node.get(key).and_then(serde_json::Value::as_u64);
    let max = usize::try_from(bound("maxLength").expect("the schema declares maxLength"))
        .expect("fits usize");
    let min = bound("minLength").unwrap_or(0);
    assert_eq!(validate(&"x".repeat(max)), Ok(()));
    assert_eq!(rule_of(validate(&"x".repeat(max + 1))), Rule::Length);
    if min == 0 {
        assert_eq!(validate(""), Ok(()), "schema allows empty");
    } else {
        assert_eq!(min, 1);
        assert_eq!(rule_of(validate("")), Rule::Length, "schema forbids empty");
    }
    // The pattern's quantifier says the same thing about empty as `minLength`.
    let pattern = node["pattern"]
        .as_str()
        .expect("the schema declares pattern");
    assert_eq!(
        pattern,
        if min == 0 {
            r"^[\x20-\x7E]*$"
        } else {
            r"^[\x20-\x7E]+$"
        }
    );
}

#[test]
fn consumer_group_client_agent_bounds_agree_with_the_validator() {
    agrees_with(
        &declared_field::<CreateConsumerGroupRequest>("client_agent"),
        client_agent,
    );
}

#[test]
fn subscription_client_agent_bounds_agree_with_the_validator() {
    agrees_with(
        &declared_field::<JoinSubscriptionRequest>("client_agent"),
        client_agent,
    );
}

#[test]
fn consumer_group_description_bounds_agree_with_the_validator() {
    agrees_with(
        &declared_field::<CreateConsumerGroupRequest>("description"),
        description,
    );
}
