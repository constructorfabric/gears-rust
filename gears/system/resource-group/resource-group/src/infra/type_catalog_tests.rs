// Created: 2026-10-12 by Constructor Tech
use super::*;

use resource_group_sdk::TENANT_RG_TYPE_PATH;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

fn catalog_with_inventory() -> (Arc<MockTypesRegistry>, TrTypeCatalog) {
    let mock = Arc::new(MockTypesRegistry::new());
    mock.seed_inventory();
    let api: Arc<dyn PlatformTypesRegistryApi> = mock.clone();
    (mock, TrTypeCatalog::new(api))
}

#[tokio::test]
async fn resolves_tenant_type_by_code_with_topology_and_schema() {
    let (_mock, catalog) = catalog_with_inventory();
    let code = TENANT_RG_TYPE_PATH.to_owned();

    let resolved = catalog
        .resolve(std::slice::from_ref(&code), &[])
        .await
        .unwrap();

    let def = resolved
        .by_code
        .get(&code)
        .expect("tenant type is registered");
    assert_eq!(def.code, TENANT_RG_TYPE_PATH);
    assert!(def.can_be_root);
    assert_eq!(
        def.allowed_parent_types,
        vec![TENANT_RG_TYPE_PATH.to_owned()]
    );
    assert!(def.allowed_membership_types.is_empty());
    assert!(def.schema.is_some(), "resolved schema is projected");
    assert!(
        resolved.by_uuid.contains_key(&def.uuid),
        "a type resolved by code is also reachable by its uuid"
    );
}

#[tokio::test]
async fn resolves_by_uuid_round_trip() {
    let (_mock, catalog) = catalog_with_inventory();
    let code = TENANT_RG_TYPE_PATH.to_owned();
    let by_code = catalog
        .resolve(std::slice::from_ref(&code), &[])
        .await
        .unwrap()
        .by_code;
    let uuid = by_code[&code].uuid;

    let resolved = catalog.resolve(&[], &[uuid, uuid]).await.unwrap();

    assert_eq!(resolved.by_uuid.len(), 1);
    assert_eq!(resolved.by_uuid[&uuid].code, TENANT_RG_TYPE_PATH);
    assert!(resolved.by_code.is_empty());
}

#[tokio::test]
async fn unknown_and_unparsable_codes_are_absent_not_errors() {
    let (_mock, catalog) = catalog_with_inventory();
    let unknown = "gts.cf.core.rg.type.v1~x.test.nothing.v1~".to_owned();
    let unparsable = "not a gts id".to_owned();

    let resolved = catalog
        .resolve(&[unknown.clone(), unparsable.clone()], &[Uuid::now_v7()])
        .await
        .unwrap();

    assert!(!resolved.by_code.contains_key(&unknown));
    assert!(!resolved.by_code.contains_key(&unparsable));
    assert!(resolved.by_uuid.is_empty());
}

#[tokio::test]
async fn empty_input_makes_no_registry_call() {
    let (mock, catalog) = catalog_with_inventory();

    let empty = catalog.resolve(&[], &[]).await.unwrap();
    assert!(empty.by_code.is_empty() && empty.by_uuid.is_empty());
    assert!(
        catalog
            .resolve(&["not a gts id".to_owned()], &[])
            .await
            .unwrap()
            .by_code
            .is_empty()
    );

    assert_eq!(mock.calls(Call::BatchGet), 0);
}

#[tokio::test]
async fn mixed_codes_and_uuids_make_exactly_one_registry_request() {
    let (mock, catalog) = catalog_with_inventory();
    let code = TENANT_RG_TYPE_PATH.to_owned();
    let uuid = catalog
        .resolve(std::slice::from_ref(&code), &[])
        .await
        .unwrap()
        .by_code[&code]
        .uuid;
    let base = mock.calls(Call::BatchGet);

    let resolved = catalog
        .resolve(std::slice::from_ref(&code), &[uuid])
        .await
        .unwrap();

    assert_eq!(mock.calls(Call::BatchGet) - base, 1);
    assert_eq!(resolved.by_code[&code].uuid, uuid);
    assert_eq!(resolved.by_uuid[&uuid].code, TENANT_RG_TYPE_PATH);
}

#[tokio::test]
async fn registry_failure_maps_to_types_registry_unavailable() {
    let (mock, catalog) = catalog_with_inventory();
    mock.inject(Fault::any().fail(CanonicalError::internal("boom").create()));

    let result = catalog
        .resolve(&[TENANT_RG_TYPE_PATH.to_owned()], &[Uuid::now_v7()])
        .await;

    match result {
        Err(DomainError::TypesRegistryUnavailable { message }) => {
            assert!(!message.contains("boom"), "raw registry text must not leak");
        }
        other => panic!("expected TypesRegistryUnavailable, got {other:?}"),
    }
}

#[test]
fn missing_or_malformed_traits_read_as_defaults() {
    let traits: RgTraits = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(!traits.can_be_root);
    assert!(traits.allowed_parent_types.is_empty());
    assert!(traits.allowed_membership_types.is_empty());

    let malformed = serde_json::from_value::<RgTraits>(serde_json::json!({"can_be_root": "yes"}));
    assert!(
        malformed.is_err(),
        "mismatching traits fall back to defaults in `to_def`"
    );
}
