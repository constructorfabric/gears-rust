use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_canonical_errors::context::InvalidArgumentV1;
use toolkit_gts::gts_id;

use ::gts::{GtsInstanceId, GtsTypeId};

use crate::error::{EventBrokerError, reasons};
use crate::models::EventType;

use super::PreparedEventType;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.sdk.producer.schema_orders.v1");
const EVENT_TYPE: &str =
    gts_id!("cf.core.events.event.v1~example.sdk.producer.schema_order_created.v1~");
const PLANTED_VALUE: &str = "PLANTED-VALUE-7f3a";
const PLANTED_KEY: &str = "PLANTED-KEY-9c1e";

#[test]
fn a_schema_violation_names_the_keyword_and_schema_location_but_no_value() {
    let prepared = PreparedEventType::new(&EventType {
        id: GtsTypeId::try_new(EVENT_TYPE).expect("a valid event type id"),
        topic: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
        description: None,
        allowed_subject_types: Vec::new(),
        partition_key: "/tenant_id".to_owned(),
        data_schema: json!({
            "type": "object",
            "properties": { "total_cents": { "type": "integer" } },
            "additionalProperties": false,
            "propertyNames": { "pattern": "^[a-z_]+$" }
        }),
    })
    .expect("the schema compiles");

    let err = prepared
        .validate(&json!({ "total_cents": PLANTED_VALUE, PLANTED_KEY: 1 }))
        .expect_err("the payload breaks three keywords");

    let EventBrokerError::EventDataInvalid {
        type_id,
        errors,
        detail,
    } = err.clone()
    else {
        panic!("expected EventDataInvalid, got {err:?}");
    };
    assert_eq!(type_id, EVENT_TYPE);
    assert_eq!(detail, "event payload failed schema validation");
    assert_eq!(
        errors,
        [
            "type at /properties/total_cents/type",
            "additionalProperties at /additionalProperties",
            "propertyNames at /propertyNames/pattern",
        ]
    );

    let display = err.to_string();
    assert_eq!(
        display,
        format!(
            "event data invalid for {EVENT_TYPE}: [\"type at /properties/total_cents/type\", \
             \"additionalProperties at /additionalProperties\", \
             \"propertyNames at /propertyNames/pattern\"]"
        )
    );
    assert!(!display.contains(PLANTED_VALUE), "{display}");
    assert!(!display.contains(PLANTED_KEY), "{display}");

    let canonical = CanonicalError::from(err);
    let CanonicalError::InvalidArgument {
        ctx: InvalidArgumentV1::FieldViolations { field_violations },
        ..
    } = &canonical
    else {
        panic!("expected InvalidArgument field violations, got {canonical:?}");
    };
    let [violation] = field_violations.as_slice() else {
        panic!("expected one field violation, got {field_violations:?}");
    };
    assert_eq!(violation.field, "data");
    assert_eq!(
        violation.description,
        "event payload failed schema validation: type at /properties/total_cents/type; \
         additionalProperties at /additionalProperties; propertyNames at /propertyNames/pattern"
    );
    assert_eq!(violation.reason, reasons::EVENT_DATA_INVALID);
    let rendered = format!("{canonical:?}");
    assert!(!rendered.contains(PLANTED_VALUE), "{rendered}");
    assert!(!rendered.contains(PLANTED_KEY), "{rendered}");
}
