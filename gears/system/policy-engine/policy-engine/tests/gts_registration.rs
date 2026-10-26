//! Every GTS entity the gear owns is accepted by the real types registry:
//! the SDK's inventory (resource types, permission catalog) and the engine
//! plugin instance the gear registers at init, beside the platform's
//! inventory, validated when the registry switches to ready.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use policy_engine::domain::engine_plugin;
use policy_engine_sdk::gts::permissions::Capability;
use policy_engine_sdk::{ASSIGNMENT_RESOURCE, BUNDLE_RESOURCE, BUNDLE_VERSION_RESOURCE};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::TypesRegistryService;
use types_registry::infra::InMemoryGtsRepository;
use types_registry_sdk::RegisterResult;

/// A ready registry holding the platform inventory and the engine plugin
/// instance, as the gear registers it.
fn ready_registry() -> TypesRegistryService {
    // Linked so their inventory entries (the plugin spec the instance
    // derives from, the event and topic bases) exist.
    use admission_control_sdk as _;
    use event_broker_sdk as _;

    let config = TypesRegistryConfig::default();
    let service = TypesRegistryService::new(
        Arc::new(InMemoryGtsRepository::new(config.to_gts_config())),
        config,
    );
    let mut entities = toolkit_gts::all_inventory_type_schemas().unwrap();
    entities.extend(toolkit_gts::all_inventory_instances().unwrap());
    let (_, plugin) = engine_plugin::registration("constructorfabric", 100).unwrap();
    entities.push(plugin);
    for result in service.register(entities) {
        if let RegisterResult::Err { gts_id, error } = result {
            panic!("{gts_id:?} rejected: {error:?}");
        }
    }
    service.switch_to_ready().expect("every entity validates");
    service
}

#[test]
fn the_resource_types_are_registered() {
    let registry = ready_registry();
    for type_id in [
        BUNDLE_RESOURCE,
        BUNDLE_VERSION_RESOURCE,
        ASSIGNMENT_RESOURCE,
    ] {
        let entity = registry
            .get(type_id)
            .unwrap_or_else(|e| panic!("{type_id}: {e:?}"));
        assert!(entity.is_type_schema, "{type_id} is a type schema");
    }
}

#[test]
fn the_permission_catalog_is_registered() {
    let registry = ready_registry();
    for capability in Capability::ALL {
        let id = capability.permission_id();
        let entity = registry.get(id).unwrap_or_else(|e| panic!("{id}: {e:?}"));
        assert!(!entity.is_type_schema, "{id} is an instance");
        assert_eq!(entity.content["resource_type"], BUNDLE_RESOURCE);
    }
}

#[test]
fn the_engine_plugin_instance_is_registered() {
    let registry = ready_registry();
    let (id, _) = engine_plugin::registration("constructorfabric", 100).unwrap();
    let entity = registry
        .get(id.as_ref())
        .unwrap_or_else(|e| panic!("{id}: {e:?}"));
    assert!(!entity.is_type_schema);
    assert_eq!(entity.content["vendor"], "constructorfabric");
    assert_eq!(entity.content["priority"], 100);
}
