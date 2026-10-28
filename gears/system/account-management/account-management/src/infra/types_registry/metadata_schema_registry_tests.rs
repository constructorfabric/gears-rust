//! Tests for [`super::GtsMetadataSchemaRegistry`] over real metadata schema
//! documents: the AM envelope is the linked inventory declaration and
//! `MockTypesRegistry` materializes `effective_traits` and `resolved_schema`
//! with `gts-rust` as admission does.

#![allow(clippy::expect_used, reason = "test helpers")]

use serde_json::json;
use toolkit_gts::gts_uri;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

use super::*;
use crate::infra::types_registry::test_helpers::{
    CallerSpy, Tampered, assert_only_caller, at_the_boundary, caller, refusals,
};

const INHERITING: &str = "gts.cf.core.am.tenant_metadata.v1~acme.test.am.tier.v1~";
const PLAIN: &str = "gts.cf.core.am.tenant_metadata.v1~acme.test.am.plain.v1~";

fn uuid(type_id: &str) -> Uuid {
    GtsId::try_new(type_id).expect("valid id").to_uuid()
}

/// The inventory envelope, an inheriting schema whose payload requires a
/// string `tier`, and a schema declaring no traits.
fn registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed_inventory();
    registry.seed(
        INHERITING,
        json!({
            "$id": gts_uri!(INHERITING),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": gts_uri!("cf.core.am.tenant_metadata.v1~") }],
            "properties": {
                "payload": {
                    "type": "object",
                    "required": ["tier"],
                    "properties": { "tier": { "type": "string" } },
                },
            },
            "x-gts-traits": { "inheritance_policy": "inherit" },
        }),
    );
    registry.seed(
        PLAIN,
        json!({
            "$id": gts_uri!(PLAIN),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": gts_uri!("cf.core.am.tenant_metadata.v1~") }],
        }),
    );
    registry
}

fn delete(registry: &MockTypesRegistry, type_id: &str) {
    let content = registry.content(type_id).expect("seeded");
    registry.seed_entity(Seed::new(type_id, content).deleted());
}

fn failing_reads(registry: &MockTypesRegistry) {
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
}

// -------- inheritance policy --------

#[tokio::test]
async fn reads_an_explicit_inherit_policy() {
    let adapter = GtsMetadataSchemaRegistry::new(registry());

    let policy = adapter
        .resolve_inheritance_policy(&caller(1), &GtsTypeId::new(INHERITING))
        .await
        .expect("policy");

    assert_eq!(policy, InheritancePolicy::Inherit);
}

#[tokio::test]
async fn a_schema_without_traits_gets_the_envelope_default() {
    let adapter = GtsMetadataSchemaRegistry::new(registry());

    let policy = adapter
        .resolve_inheritance_policy(&caller(1), &GtsTypeId::new(PLAIN))
        .await
        .expect("policy");

    assert_eq!(policy, InheritancePolicy::OverrideOnly);
}

#[tokio::test]
async fn an_unknown_or_deleted_schema_is_a_metadata_not_found() {
    let registry = registry();
    delete(&registry, INHERITING);
    let adapter = GtsMetadataSchemaRegistry::new(registry);

    for type_id in [
        INHERITING,
        "gts.cf.core.am.tenant_metadata.v1~acme.test.am.absent.v1~",
    ] {
        let err = adapter
            .resolve_inheritance_policy(&caller(1), &GtsTypeId::new(type_id))
            .await
            .expect_err("not registered");
        assert!(
            matches!(err, DomainError::MetadataEntryNotFound { .. }),
            "{type_id}: {err:?}"
        );
    }
}

#[tokio::test]
async fn a_failed_read_is_service_unavailable() {
    let registry = registry();
    failing_reads(&registry);
    let adapter = GtsMetadataSchemaRegistry::new(registry);

    let err = adapter
        .resolve_inheritance_policy(&caller(1), &GtsTypeId::new(INHERITING))
        .await
        .expect_err("read failed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_snapshot_without_effective_traits_is_not_the_default_policy() {
    let adapter = GtsMetadataSchemaRegistry::new(Arc::new(Tampered::without_documents(registry())));

    let err = adapter
        .resolve_inheritance_policy(&caller(1), &GtsTypeId::new(INHERITING))
        .await
        .expect_err("an incomplete projection is a contract break");
    assert!(matches!(err, DomainError::Internal { .. }), "{err:?}");
}

// -------- value validation --------

#[tokio::test]
async fn validates_the_payload_against_the_resolved_schema() {
    let adapter = GtsMetadataSchemaRegistry::new(registry());

    adapter
        .validate_value(
            &caller(1),
            &GtsTypeId::new(INHERITING),
            &json!({ "tier": "gold" }),
        )
        .await
        .expect("a conforming value");
    let err = adapter
        .validate_value(
            &caller(1),
            &GtsTypeId::new(INHERITING),
            &json!({ "tier": 3 }),
        )
        .await
        .expect_err("a malformed value");
    assert!(
        matches!(err, DomainError::MetadataValidation { .. }),
        "{err:?}"
    );
    let err = adapter
        .validate_value(&caller(1), &GtsTypeId::new(INHERITING), &json!({}))
        .await
        .expect_err("a missing required property");
    assert!(
        matches!(err, DomainError::MetadataValidation { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_envelope_constraints_apply_to_a_derived_schema() {
    // The envelope requires an object payload; the derived schema inherits it
    // through the reference inlined into `resolved_schema`.
    let adapter = GtsMetadataSchemaRegistry::new(registry());

    let err = adapter
        .validate_value(&caller(1), &GtsTypeId::new(PLAIN), &json!("not an object"))
        .await
        .expect_err("the envelope's payload type applies");
    assert!(
        matches!(err, DomainError::MetadataValidation { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_deleted_schema_accepts_no_new_value() {
    let registry = registry();
    delete(&registry, INHERITING);
    let adapter = GtsMetadataSchemaRegistry::new(registry);

    let err = adapter
        .validate_value(
            &caller(1),
            &GtsTypeId::new(INHERITING),
            &json!({ "tier": "gold" }),
        )
        .await
        .expect_err("a tombstone is not registered");
    assert!(
        matches!(err, DomainError::MetadataEntryNotFound { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_snapshot_without_resolved_schema_validates_nothing() {
    let adapter = GtsMetadataSchemaRegistry::new(Arc::new(Tampered::without_documents(registry())));

    let err = adapter
        .validate_value(
            &caller(1),
            &GtsTypeId::new(INHERITING),
            &json!({ "tier": 3 }),
        )
        .await
        .expect_err("an incomplete projection is a contract break");
    assert!(matches!(err, DomainError::Internal { .. }), "{err:?}");
}

// -------- reverse hydration --------

#[tokio::test]
async fn hydrates_a_page_in_one_read_omitting_unknown_and_deleted_schemas() {
    let registry = registry();
    delete(&registry, PLAIN);
    let unknown = Uuid::from_u128(0xF00D);
    let adapter = GtsMetadataSchemaRegistry::new(registry.clone());

    let ids = adapter
        .resolve_ids_by_uuid(&caller(1), &[uuid(INHERITING), uuid(PLAIN), unknown])
        .await
        .expect("hydration");

    assert_eq!(ids.len(), 1);
    assert_eq!(ids[&uuid(INHERITING)].as_ref(), INHERITING);
    assert_eq!(registry.calls(Call::BatchGet), 1);
}

#[tokio::test]
async fn a_failed_page_read_fails_hydration() {
    let registry = registry();
    failing_reads(&registry);
    let adapter = GtsMetadataSchemaRegistry::new(registry);

    let err = adapter
        .resolve_ids_by_uuid(&caller(1), &[uuid(INHERITING)])
        .await
        .expect_err("read failed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "{err:?}"
    );
}

/// A refusal of the caller's credential stays that caller's 401 / 403, and
/// any other failure that is not an outage is internal, on every read —
/// never a retryable 503.
#[tokio::test]
async fn a_failed_read_that_is_not_an_outage_is_not_service_unavailable() {
    let expected = refusals()
        .into_iter()
        .chain([(CanonicalError::internal("registry exploded").create(), 500)]);
    for (error, status) in expected {
        let registry = registry();
        registry.inject(Fault::on(Call::BatchGet).fail(error.clone()));
        let adapter = GtsMetadataSchemaRegistry::new(registry);

        let single = adapter
            .resolve_inheritance_policy(&caller(1), &GtsTypeId::new(INHERITING))
            .await
            .expect_err("read failed");
        let page = adapter
            .resolve_ids_by_uuid(&caller(1), &[uuid(INHERITING)])
            .await
            .expect_err("page read failed");
        for err in [single, page] {
            let (category, public_status) = at_the_boundary(err);
            assert_eq!(public_status, Some(status), "{error:?} -> {category}");
            if status != 500 {
                assert_eq!(category, error.gts_type(), "{error:?}");
            }
        }
    }
}

#[tokio::test]
async fn a_schema_that_does_not_round_trip_its_uuid_is_internal() {
    let adapter = GtsMetadataSchemaRegistry::new(Arc::new(Tampered::new(registry(), |entity| {
        entity.gts_id = GtsId::try_new(PLAIN).expect("valid id");
    })));

    let err = adapter
        .resolve_id_by_uuid(&caller(1), uuid(INHERITING))
        .await
        .expect_err("the answered id does not hash to the asked uuid");
    assert!(matches!(err, DomainError::Internal { .. }), "{err:?}");
    let err = adapter
        .resolve_ids_by_uuid(&caller(1), &[uuid(INHERITING)])
        .await
        .expect_err("the same guard on the page");
    assert!(matches!(err, DomainError::Internal { .. }), "{err:?}");
}

#[tokio::test]
async fn a_deleted_schema_uuid_is_an_orphan() {
    let registry = registry();
    delete(&registry, INHERITING);
    let adapter = GtsMetadataSchemaRegistry::new(registry);

    let err = adapter
        .resolve_id_by_uuid(&caller(1), uuid(INHERITING))
        .await
        .expect_err("a tombstone hydrates nothing");
    assert!(
        matches!(err, DomainError::MetadataEntryNotFound { .. }),
        "{err:?}"
    );
}

// -------- who the schemas are read for (P33) --------

/// One adapter serves every metadata request: each of its reads is made
/// for the caller passed with it, and none reaches the platform API.
#[tokio::test]
async fn every_read_is_made_for_its_own_caller() {
    let spy = CallerSpy::for_requests(registry());
    let adapter = GtsMetadataSchemaRegistry::new(spy.clone());

    for n in [1, 2] {
        let ctx = caller(n);
        adapter
            .resolve_inheritance_policy(&ctx, &GtsTypeId::new(INHERITING))
            .await
            .expect("policy");
        adapter
            .validate_value(
                &ctx,
                &GtsTypeId::new(INHERITING),
                &json!({ "tier": "gold" }),
            )
            .await
            .expect("valid value");
        adapter
            .resolve_id_by_uuid(&ctx, uuid(INHERITING))
            .await
            .expect("known uuid");
        adapter
            .resolve_ids_by_uuid(&ctx, &[uuid(INHERITING), uuid(PLAIN)])
            .await
            .expect("known uuids");
        assert_eq!(spy.callers().len(), 4, "one read per method");
        assert_only_caller(&spy.callers(), &ctx);
        spy.clear();
    }
}

/// A port implementation that relies on the default batch reverse lookup.
struct SingleLookups(GtsMetadataSchemaRegistry);

#[async_trait]
impl MetadataSchemaRegistry for SingleLookups {
    async fn resolve_inheritance_policy(
        &self,
        ctx: &SecurityContext,
        type_id: &GtsTypeId,
    ) -> Result<InheritancePolicy, DomainError> {
        self.0.resolve_inheritance_policy(ctx, type_id).await
    }

    async fn resolve_id_by_uuid(
        &self,
        ctx: &SecurityContext,
        schema_uuid: Uuid,
    ) -> Result<GtsTypeId, DomainError> {
        self.0.resolve_id_by_uuid(ctx, schema_uuid).await
    }

    async fn validate_value(
        &self,
        ctx: &SecurityContext,
        type_id: &GtsTypeId,
        value: &Value,
    ) -> Result<(), DomainError> {
        self.0.validate_value(ctx, type_id, value).await
    }
}

/// The default `resolve_ids_by_uuid` forwards its caller to every single
/// lookup it makes, and still omits unknown UUIDs.
#[tokio::test]
async fn the_default_batch_lookup_forwards_the_caller_to_each_lookup() {
    let spy = CallerSpy::for_requests(registry());
    let port = SingleLookups(GtsMetadataSchemaRegistry::new(spy.clone()));
    let unknown = Uuid::from_u128(0xDEAD);

    for n in [1, 2] {
        let ctx = caller(n);
        let found = port
            .resolve_ids_by_uuid(&ctx, &[uuid(INHERITING), uuid(PLAIN), unknown])
            .await
            .expect("lookups");
        assert_eq!(found.len(), 2, "the unknown uuid is omitted");
        assert_eq!(spy.callers().len(), 3, "one lookup per uuid");
        assert_only_caller(&spy.callers(), &ctx);
        spy.clear();
    }
}
