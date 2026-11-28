//! The types-registry adapter of the quota intake (P-D-268) over the platform's in-memory
//! registry, seeded as a test adapter registers its instances: reads answer the type and the
//! resource version, an unknown id is `None`, an outage or a missing registry is unavailable, and
//! validation runs the registry's own compiled schema.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::quota_test_support::{
    DERIVED_CAPABILITY, VCPU_METRIC, WELL_KNOWN, seed_mock, vcpu_record,
};
use crate::test_support::authed_ctx;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};
use uuid::Uuid;

fn adapter(mock: &Arc<MockTypesRegistry>) -> TypesRegistryQuotaRegistry {
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn types_registry_sdk::TypesRegistryApi>(mock.clone());
    TypesRegistryQuotaRegistry::new(hub)
}

#[tokio::test]
async fn an_instance_reads_with_its_type_and_resource_version() {
    let mock = Arc::new(MockTypesRegistry::new());
    seed_mock(&mock);
    let got = adapter(&mock)
        .instance(&authed_ctx(Uuid::new_v4()), VCPU_METRIC)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.type_id, bss_products_sdk::quota::gts::METRIC_TYPE);
    assert!(got.resource_version >= 1);
    assert_eq!(got.content["quota_kind"], "allocation");
    assert_eq!(
        got.uuid,
        gts::GtsId::try_new(VCPU_METRIC).unwrap().to_uuid()
    );
    mock.seed_entity(Seed::new(VCPU_METRIC, got.content.clone()).version(4));
    let bumped = adapter(&mock)
        .instance(&authed_ctx(Uuid::new_v4()), VCPU_METRIC)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bumped.resource_version, 4);
}

#[tokio::test]
async fn an_unknown_instance_is_none_and_an_outage_is_unavailable() {
    let mock = Arc::new(MockTypesRegistry::new());
    seed_mock(&mock);
    let registry = adapter(&mock);
    let ctx = authed_ctx(Uuid::new_v4());
    let unknown = "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.nothing.v1";
    assert!(registry.instance(&ctx, unknown).await.unwrap().is_none());
    let deleted = "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.gone.v1";
    mock.seed_entity(Seed::new(deleted, serde_json::json!({"id": deleted})).deleted());
    assert!(registry.instance(&ctx, deleted).await.unwrap().is_none());
    assert!(
        registry
            .type_schema(&ctx, "gts.cf.compute.quota_adapter.nothing.v1~")
            .await
            .unwrap()
            .is_none()
    );
    mock.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    assert!(registry.instance(&ctx, VCPU_METRIC).await.is_err());
}

#[tokio::test]
async fn without_a_registry_in_the_hub_every_read_is_unavailable() {
    let registry = TypesRegistryQuotaRegistry::new(Arc::new(toolkit::ClientHub::new()));
    let ctx = authed_ctx(Uuid::new_v4());
    assert!(registry.instance(&ctx, VCPU_METRIC).await.is_err());
    assert!(
        registry
            .type_schema(&ctx, bss_products_sdk::quota::gts::UNIT_TYPE)
            .await
            .is_err()
    );
    assert!(
        registry
            .validate(
                &ctx,
                bss_products_sdk::quota::gts::CAPABILITY_TYPE,
                &vcpu_record()
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_well_known_instances_validate_against_their_schemas() {
    let mock = Arc::new(MockTypesRegistry::new());
    seed_mock(&mock);
    let registry = adapter(&mock);
    let ctx = authed_ctx(Uuid::new_v4());
    for id in WELL_KNOWN {
        let instance = registry.instance(&ctx, id).await.unwrap().expect(id);
        let verdict = registry
            .validate(&ctx, &instance.type_id, &instance.content)
            .await
            .unwrap();
        assert_eq!(verdict, RegistryVerdict::Valid, "{id}");
    }
    let verdict = registry
        .validate(
            &ctx,
            bss_products_sdk::quota::gts::CAPABILITY_TYPE,
            &vcpu_record(),
        )
        .await
        .unwrap();
    assert_eq!(verdict, RegistryVerdict::Valid);
}

#[tokio::test]
async fn a_record_the_capability_schema_refuses_is_invalid() {
    let mock = Arc::new(MockTypesRegistry::new());
    seed_mock(&mock);
    let mut record = vcpu_record();
    record["limits"]["step"] = serde_json::json!(1); // a number, not a decimal string
    let verdict = adapter(&mock)
        .validate(
            &authed_ctx(Uuid::new_v4()),
            bss_products_sdk::quota::gts::CAPABILITY_TYPE,
            &record,
        )
        .await
        .unwrap();
    assert!(
        matches!(verdict, RegistryVerdict::Invalid(_)),
        "{verdict:?}"
    );
}

/// A schema an adapter derives from the capability schema validates its own `properties` through
/// the registry's materialized schema, and an unknown schema is no verdict at all.
#[tokio::test]
async fn a_derived_capability_schema_validates_through_its_ancestors() {
    let mock = Arc::new(MockTypesRegistry::new());
    seed_mock(&mock);
    let registry = adapter(&mock);
    let ctx = authed_ctx(Uuid::new_v4());
    let derived = registry
        .type_schema(&ctx, DERIVED_CAPABILITY)
        .await
        .unwrap()
        .unwrap();
    assert!(!derived.is_abstract);
    let mut record = vcpu_record();
    record["type_id"] = serde_json::json!(DERIVED_CAPABILITY);
    record["properties"] = serde_json::json!({"reviewed_by": "adapter-owner"});
    assert_eq!(
        registry
            .validate(&ctx, DERIVED_CAPABILITY, &record)
            .await
            .unwrap(),
        RegistryVerdict::Valid
    );
    record["properties"] = serde_json::json!({"reviewed_by": 1});
    assert!(matches!(
        registry
            .validate(&ctx, DERIVED_CAPABILITY, &record)
            .await
            .unwrap(),
        RegistryVerdict::Invalid(_)
    ));
    assert_eq!(
        registry
            .validate(&ctx, "gts.cf.compute.quota_adapter.nothing.v1~", &record)
            .await
            .unwrap(),
        RegistryVerdict::UnknownSchema
    );
}

/// A live schema that resolves but compiles into no validator answers a verdict on the record,
/// naming the schema, never an outage: it blocks only the records that name it. An abstract leaf is
/// such a schema: the registry holds and resolves it, and the compiler refuses it.
#[tokio::test]
async fn a_live_schema_that_does_not_compile_is_a_verdict_not_an_outage() {
    let mock = Arc::new(MockTypesRegistry::new());
    seed_mock(&mock);
    let broken =
        "gts.cf.bss.products.quota_capability.v1~cf.compute.quota_adapter.broken_capability.v1~";
    mock.seed(
        broken,
        serde_json::json!({
            "$id": format!("gts://{broken}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-abstract": true,
            "allOf": [{"$ref": format!("gts://{}", bss_products_sdk::quota::gts::CAPABILITY_TYPE)}]
        }),
    );
    let mut record = vcpu_record();
    record["type_id"] = serde_json::json!(broken);
    let verdict = adapter(&mock)
        .validate(&authed_ctx(Uuid::new_v4()), broken, &record)
        .await
        .unwrap();
    match verdict {
        RegistryVerdict::Uncompilable(why) => assert!(why.contains(broken), "{why}"),
        other => panic!("expected an uncompilable schema, got {other:?}"),
    }
}
