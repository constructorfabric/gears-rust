//! The generated refusal event schema obeys the broker's base contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use admission_control_sdk::REFUSAL_EVENT_TYPE;
use admission_control_sdk::gts::refusal_event_type_schema;
use event_broker_sdk::gts::EventV1;
use gts::GtsSchema;

fn validate(event_type: &serde_json::Value) -> Result<gts::ResolvedType, gts::StoreError> {
    let mut store = gts::GtsStore::new();
    let base: serde_json::Value =
        serde_json::from_str(&EventV1::gts_schema_with_refs_as_string()).unwrap();
    store.register_schema(EventV1::TYPE_ID, &base).unwrap();
    store
        .register_schema(REFUSAL_EVENT_TYPE, event_type)
        .unwrap();
    store.validate_schema_with(REFUSAL_EVENT_TYPE, gts::GtsRefValidation::None)
}

#[test]
fn the_refusal_event_type_validates_against_the_real_gts_base() {
    validate(&refusal_event_type_schema()).unwrap();
}

#[test]
fn a_data_schema_that_widens_the_base_is_refused() {
    let mut widened = refusal_event_type_schema();
    widened["allOf"][1]["properties"]["data"] = serde_json::json!({ "type": "string" });
    assert!(validate(&widened).is_err());
}
