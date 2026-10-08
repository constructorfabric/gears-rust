//! Contract macro IR assertions for the tenant plane.

use toolkit_contract::descriptor::ContractKind;
use toolkit_contract::ir::contract::{FieldRole, Idempotency};

use super::{TYPES_REGISTRY_API_DESCRIPTOR, types_registry_api_ir};

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
        .get_type_schema(&SecurityContext::anonymous(), TYPE, Projection::Default)
        .await
        .expect("an extension helper answers through the trait object");

    assert_eq!(snapshot.gts_id, gts::GtsId::try_new(TYPE).expect("valid"));
}
