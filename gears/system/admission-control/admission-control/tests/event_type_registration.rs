//! The refusal event type is an inventory entry: the real types registry
//! admits and validates it beside the platform's inventory, and the platform
//! API lists it with the traits the broker reads.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use admission_control_sdk::gts::{
    ADMISSION_CONTROL_RESOURCE, AUDIT_TOPIC_ID, REFUSAL_EVENT_TYPE, refusal_event_type_schema,
};
use serde_json::json;
use toolkit_security::PlatformSecurityContext;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::TypesRegistryService;
use types_registry::infra::InMemoryGtsRepository;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use types_registry_sdk::{
    GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApiExt, RegisterResult,
};

/// Registers the platform inventory, with `event_type` in place of its refusal
/// entry, in a fresh registry and switches it to ready, which validates every
/// entity.
fn register_and_validate(event_type: serde_json::Value) -> Result<(), String> {
    // Linked so their inventory entries (the event and topic bases) exist.
    use event_broker_sdk as _;

    let config = TypesRegistryConfig::default();
    let service = TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    );
    let mut entities = toolkit_gts::all_inventory_type_schemas().unwrap();
    entities.extend(toolkit_gts::all_inventory_instances().unwrap());
    entities.retain(|e| e["$id"] != format!("gts://{REFUSAL_EVENT_TYPE}"));
    entities.push(event_type);
    for result in service.register(entities) {
        if let RegisterResult::Err { gts_id, error } = result {
            return Err(format!("{gts_id:?} rejected: {error:?}"));
        }
    }
    service
        .switch_to_ready()
        .map_err(|err| format!("validation failed: {err:?}"))
}

#[test]
fn the_refusal_event_type_validates_in_the_real_registry() {
    register_and_validate(refusal_event_type_schema()).unwrap();
}

#[test]
fn a_data_schema_that_widens_the_base_is_refused() {
    // Control: the registry does check `data` against the event base, so the
    // test above would catch an incompatible generated schema.
    let mut widened = refusal_event_type_schema();
    widened["allOf"][1]["properties"]["data"] = serde_json::json!({ "type": "string" });
    let error = register_and_validate(widened).unwrap_err();
    assert!(error.contains("changes type incompatibly"), "{error}");
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
