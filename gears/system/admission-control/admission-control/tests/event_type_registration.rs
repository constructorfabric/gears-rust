//! The refusal event type schema is accepted by the real types registry: it
//! registers beside the platform's inventory and validates when the registry
//! switches to ready.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use admission_control_sdk::gts::refusal_event_type_schema;
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::TypesRegistryService;
use types_registry::infra::InMemoryGtsRepository;
use types_registry_sdk::RegisterResult;

/// Registers the platform inventory plus `event_type` in a fresh registry and
/// switches it to ready, which validates every entity.
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
    assert!(register_and_validate(widened).is_err());
}
