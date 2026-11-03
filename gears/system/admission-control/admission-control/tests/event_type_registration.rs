//! The generated refusal event schema obeys the broker's base contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use admission_control_sdk::gts::{
    ADMISSION_CONTROL_RESOURCE, AUDIT_TOPIC_ID, REFUSAL_EVENT_TYPE, refusal_event_type_schema,
};
use event_broker_sdk::gts::EventV1;
use gts::GtsSchema;
use serde_json::json;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use types_registry_sdk::{GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApiExt};

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

#[tokio::test]
async fn the_platform_registry_lists_the_refusal_event_type_from_the_inventory() {
    // Linked so their inventory entries (the event and topic bases) exist.
    use event_broker_sdk as _;

    let mock = MockTypesRegistry::new();
    mock.seed_inventory();
    let ctx = PlatformSecurityContext::outbound_marker();

    let event_types = mock
        .list_type_schemas(
            &ctx,
            ListEntitiesRequest::matching(
                GtsIdPattern::try_new("gts.cf.core.events.event.v1~*").unwrap(),
            ),
        )
        .await
        .unwrap();
    let found: Vec<_> = event_types
        .iter()
        .filter(|schema| schema.type_id.as_ref() == REFUSAL_EVENT_TYPE)
        .collect();
    let [refusal] = found.as_slice() else {
        panic!("expected exactly one {REFUSAL_EVENT_TYPE}, found {found:?}");
    };
    assert_eq!(
        refusal.effective_traits,
        Some(json!({
            "topic": AUDIT_TOPIC_ID,
            "allowed_subject_types": [ADMISSION_CONTROL_RESOURCE],
            "partition_key": "/tenant_id",
        }))
    );
    assert_eq!(refusal.content, Some(refusal_event_type_schema()));
}
