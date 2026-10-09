//! Contract macro IR assertions for both planes.

use toolkit_contract::descriptor::ContractKind;
use toolkit_contract::ir::contract::{FieldRole, Idempotency};

use super::{
    PLATFORM_TYPES_REGISTRY_API_DESCRIPTOR, TYPES_REGISTRY_API_DESCRIPTOR,
    platform_types_registry_api_ir, types_registry_api_ir,
};

#[test]
fn the_contract_descriptor_names_the_types_registry_v1_api() {
    let descriptor = &PLATFORM_TYPES_REGISTRY_API_DESCRIPTOR;
    assert_eq!(descriptor.kind, ContractKind::Api);
    assert_eq!(descriptor.gear, "types-registry");
    assert_eq!(descriptor.version, "v1");
}

#[test]
fn the_contract_has_exactly_the_five_spec_methods_with_their_idempotency() {
    let ir = platform_types_registry_api_ir();
    let actual: Vec<(&str, Idempotency)> = ir
        .methods
        .iter()
        .map(|m| (m.name.as_str(), m.idempotency))
        .collect();
    assert_eq!(
        actual,
        [
            ("batch_get_entities", Idempotency::SafeRead),
            ("list_entities", Idempotency::SafeRead),
            ("register_entities", Idempotency::IdempotentWrite),
            ("delete_entities", Idempotency::IdempotentWrite),
            ("get_operation", Idempotency::SafeRead),
        ]
    );
}

#[test]
fn every_method_takes_exactly_one_security_context_first() {
    for method in &platform_types_registry_api_ir().methods {
        let roles: Vec<FieldRole> = method.input.fields.iter().map(|f| f.role).collect();
        assert_eq!(
            roles.first(),
            Some(&FieldRole::SecurityContext),
            "{}: the first parameter must be the security context",
            method.name
        );
        assert_eq!(
            roles
                .iter()
                .filter(|r| **r == FieldRole::SecurityContext)
                .count(),
            1,
            "{}: exactly one security context",
            method.name
        );
    }
}

#[tokio::test]
async fn an_implementation_registered_in_the_client_hub_answers_through_the_trait_object() {
    use std::sync::Arc;

    use crate::models::{BatchGetEntitiesRequest, BatchGetItem, EntityKey, EntityLookup};

    const TYPE: &str = "gts.cf.test.pkg.thing.v1~";
    let fake = crate::testing_platform::FakePlatformRegistry::new();
    fake.seed(TYPE, serde_json::json!({}));
    let hub = toolkit::ClientHub::new();
    hub.register::<dyn super::PlatformTypesRegistryApi>(
        Arc::new(fake) as Arc<dyn super::PlatformTypesRegistryApi>
    );

    let api = hub
        .get::<dyn super::PlatformTypesRegistryApi>()
        .expect("the registered client resolves");
    let key = EntityKey::GtsId(gts::GtsId::try_new(TYPE).expect("valid"));
    let lookups = api
        .batch_get_entities(
            &toolkit_security::PlatformSecurityContext::outbound_marker(),
            BatchGetEntitiesRequest {
                items: vec![BatchGetItem::from(key.clone())],
                projection: crate::models::Projection::Default,
                fresh: false,
            },
        )
        .await
        .expect("answers");

    assert!(matches!(
        lookups.0.get(&key),
        Some(EntityLookup::Found { .. })
    ));
}

// ---- the tenant contract ------------------------------------------------------

#[test]
fn the_tenant_contract_descriptor_names_the_types_registry_v1_api() {
    let descriptor = &TYPES_REGISTRY_API_DESCRIPTOR;
    assert_eq!(descriptor.kind, ContractKind::Api);
    assert_eq!(descriptor.gear, "types-registry");
    assert_eq!(descriptor.version, "v1");
}

#[test]
fn the_tenant_contract_has_only_the_two_safe_reads() {
    let ir = types_registry_api_ir();
    let actual: Vec<(&str, Idempotency)> = ir
        .methods
        .iter()
        .map(|m| (m.name.as_str(), m.idempotency))
        .collect();
    assert_eq!(
        actual,
        [
            ("batch_get_entities", Idempotency::SafeRead),
            ("list_entities", Idempotency::SafeRead),
        ]
    );
}

#[test]
fn every_tenant_method_takes_exactly_one_security_context_first() {
    for method in &types_registry_api_ir().methods {
        let roles: Vec<FieldRole> = method.input.fields.iter().map(|f| f.role).collect();
        assert_eq!(
            roles.first(),
            Some(&FieldRole::SecurityContext),
            "{}: the first parameter must be the security context",
            method.name
        );
        assert_eq!(
            roles
                .iter()
                .filter(|r| **r == FieldRole::SecurityContext)
                .count(),
            1,
            "{}: exactly one security context",
            method.name
        );
    }
}

#[tokio::test]
async fn the_client_hub_resolves_the_tenant_trait_object_and_its_helpers() {
    use std::sync::Arc;

    use toolkit_security::SecurityContext;

    use crate::TypesRegistryApiExt;
    use crate::models::Projection;

    const TYPE: &str = "gts.cf.test.pkg.thing.v1~";
    let fake = crate::testing_platform::FakePlatformRegistry::new();
    fake.seed(TYPE, serde_json::json!({}));
    let hub = toolkit::ClientHub::new();
    hub.register::<dyn super::TypesRegistryApi>(Arc::new(fake) as Arc<dyn super::TypesRegistryApi>);

    let api = hub
        .get::<dyn super::TypesRegistryApi>()
        .expect("the registered client resolves");
    let snapshot = api
        .get_type_schema(
            &SecurityContext::anonymous(),
            &gts::GtsTypeId::try_new(TYPE).expect("valid"),
            Projection::Default,
        )
        .await
        .expect("an extension helper answers through the trait object");

    assert_eq!(
        snapshot.type_id,
        gts::GtsTypeId::try_new(TYPE).expect("valid")
    );
}
