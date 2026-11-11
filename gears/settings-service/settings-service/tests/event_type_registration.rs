//! The broadcast's registry documents are accepted by the real types registry:
//! the activation subject type seeds from the link-time inventory beside the
//! platform's, the cache topic and the `cache_invalidate` type register on top,
//! and every entity validates when the registry switches to ready.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use cf_gears_settings_service::infra::event_schemas::{
    cache_invalidate_event_type_schema, cache_topic_instance,
};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::TypesRegistryService;
use types_registry::infra::InMemoryGtsRepository;
use types_registry_sdk::RegisterResult;

/// Registers the platform inventory plus `extra` in a fresh registry and
/// switches it to ready, which validates every entity.
fn register_and_validate(extra: Vec<serde_json::Value>) -> Result<(), String> {
    // Linked so their inventory entries (the event and topic bases, the
    // activation subject type) exist.
    use event_broker_sdk as _;

    let config = TypesRegistryConfig::default();
    let service = TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    );
    let mut entities = toolkit_gts::all_inventory_type_schemas().unwrap();
    entities.extend(toolkit_gts::all_inventory_instances().unwrap());
    entities.extend(extra);
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
fn the_cache_topic_and_event_type_validate_in_the_real_registry() {
    register_and_validate(vec![
        cache_topic_instance(),
        cache_invalidate_event_type_schema(),
    ])
    .unwrap();
}

#[test]
fn a_data_schema_that_widens_the_base_is_refused() {
    // Control: the registry does check `data` against the event base, so the
    // test above would catch an incompatible generated schema.
    let mut widened = cache_invalidate_event_type_schema();
    widened["allOf"][1]["properties"]["data"] = serde_json::json!({ "type": "string" });
    assert!(register_and_validate(vec![cache_topic_instance(), widened]).is_err());
}
