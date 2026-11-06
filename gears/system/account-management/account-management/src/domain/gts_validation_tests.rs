//! Payload and tenant-name validation tests; see `gts_validation.rs` for the
//! failure contract. Covers missing schemas, registry failures, property
//! constraints and caller propagation.
//!
//! `MockTypesRegistry` materializes `resolved_schema` from schema documents
//! with `gts-rust`, as admission does.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::sync::Arc;

use account_management_sdk::IdpNewUser;
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::gts_uri;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

use crate::domain::error::DomainError;
use crate::domain::types_registry_reader::TypesRegistryReader;
use crate::infra::types_registry::test_helpers::{CallerSpy, Tampered, assert_only_caller, caller};

// ---- helpers -------------------------------------------------------

fn user_payload(username: &str) -> IdpNewUser {
    IdpNewUser::new(username.to_owned())
}

fn schema_doc(type_id: &str, body: &Value) -> Value {
    let mut doc = json!({
        "$id": gts_uri!(type_id),
        "$schema": "http://json-schema.org/draft-07/schema#",
    });
    doc.as_object_mut()
        .expect("object")
        .extend(body.as_object().expect("object").clone());
    doc
}

fn registry_with_user_schema(max_chars: usize) -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.seed(
        super::USER_TYPE_ID,
        schema_doc(
            super::USER_TYPE_ID,
            &json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "username"],
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "username": { "type": "string", "minLength": 1, "maxLength": max_chars },
                    "email": { "type": "string", "format": "email" },
                    "display_name": { "type": "string", "minLength": 1, "maxLength": 255 },
                },
            }),
        ),
    );
    registry
}

fn registry_with_tenant_schema(max_chars: usize) -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.seed(
        super::TENANT_TYPE_ID,
        schema_doc(
            super::TENANT_TYPE_ID,
            &json!({
                "type": "object",
                "additionalProperties": true,
                "required": ["id", "name"],
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "name": { "type": "string", "minLength": 1, "maxLength": max_chars },
                    "parent_id": { "type": ["string", "null"], "format": "uuid" },
                },
            }),
        ),
    );
    registry
}

/// A registry whose every call fails as temporarily unavailable. The
/// helpers short-circuit only `NotFound`; every other category routes
/// to their `ServiceUnavailable` arm.
fn unavailable_registry() -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.inject(
        Fault::on(Call::BatchGet).fail(
            CanonicalError::service_unavailable()
                .with_detail("registry transport down (test stub)")
                .create(),
        ),
    );
    registry
}

// ---- validate_new_user_payload_via_gts ----------------------------

#[tokio::test]
async fn user_payload_schema_not_registered_surfaces_service_unavailable() {
    let registry = MockTypesRegistry::new();
    let payload = user_payload("alice");
    let err = super::validate_new_user_payload_via_gts(&payload, &registry, &caller(1))
        .await
        .expect_err(
            "schema-not-found MUST fail closed: provision_user has no AM-side \
             fallback gate and degrading to length-only checks would silently \
             disable format/pattern rules until the catalog is seeded",
        );
    match err {
        DomainError::ServiceUnavailable { detail, .. } => {
            assert!(
                detail.contains(super::USER_TYPE_ID) && detail.contains("catalog"),
                "ServiceUnavailable.detail must name the missing schema and the \
                 catalog-seed remediation; got: {detail}"
            );
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn user_payload_valid_username_passes_registered_schema() {
    let registry = registry_with_user_schema(255);
    let payload = user_payload("alice");
    super::validate_new_user_payload_via_gts(&payload, &registry, &caller(1))
        .await
        .expect("valid username passes the registered schema");
}

#[tokio::test]
async fn user_payload_oversized_username_rejects_with_validation() {
    let registry = registry_with_user_schema(255);
    let payload = user_payload(&"x".repeat(256));
    let err = super::validate_new_user_payload_via_gts(&payload, &registry, &caller(1))
        .await
        .expect_err("oversize username must be rejected by the registered schema");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("username"),
                "diagnostic must name the violating field; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn user_payload_registry_transport_error_surfaces_as_service_unavailable() {
    let registry = unavailable_registry();
    let payload = user_payload("alice");
    let err = super::validate_new_user_payload_via_gts(&payload, &registry, &caller(1))
        .await
        .expect_err("registry transport error must surface");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got {err:?}"
    );
}

// ---- validate_tenant_name_via_gts ---------------------------------

#[tokio::test]
async fn tenant_name_schema_not_registered_short_circuits_to_ok() {
    let registry = MockTypesRegistry::new();
    super::validate_tenant_name_via_gts("acme", TypesRegistryReader::caller(&registry, &caller(1)))
        .await
        .expect("schema-not-found short-circuits to Ok");
}

#[tokio::test]
async fn tenant_name_valid_passes_registered_schema() {
    let registry = registry_with_tenant_schema(255);
    super::validate_tenant_name_via_gts("acme", TypesRegistryReader::caller(&registry, &caller(1)))
        .await
        .expect("valid name passes the registered schema");
}

#[tokio::test]
async fn tenant_name_oversized_rejects_with_validation() {
    let registry = registry_with_tenant_schema(255);
    let oversized = "x".repeat(256);
    let err = super::validate_tenant_name_via_gts(
        &oversized,
        TypesRegistryReader::caller(&registry, &caller(1)),
    )
    .await
    .expect_err("oversize name must be rejected");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("name"),
                "diagnostic must name the violating field; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn tenant_name_empty_rejects_with_validation() {
    let registry = registry_with_tenant_schema(255);
    let err =
        super::validate_tenant_name_via_gts("", TypesRegistryReader::caller(&registry, &caller(1)))
            .await
            .expect_err("empty name must be rejected");
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn tenant_name_registry_transport_error_surfaces_as_service_unavailable() {
    let registry = unavailable_registry();
    let err = super::validate_tenant_name_via_gts(
        "acme",
        TypesRegistryReader::caller(&registry, &caller(1)),
    )
    .await
    .expect_err("registry transport error must surface");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got {err:?}"
    );
}

/// Every schema read of a request is classified alike: an outage is a
/// retryable 503, the registry's refusal of the caller stays its 401 / 403,
/// anything else is internal. AM's own platform read during bootstrap holds
/// no caller credential to refuse, so a refusal there is internal.
#[tokio::test]
async fn a_failed_schema_read_is_classified_at_the_boundary() {
    use crate::infra::types_registry::test_helpers::{at_the_boundary, outages, refusals};

    let expected = outages()
        .into_iter()
        .map(|error| (error, 503, 503))
        .chain(
            refusals()
                .into_iter()
                .map(|(error, status)| (error, status, 500)),
        )
        .chain([(
            CanonicalError::internal("registry exploded").create(),
            500,
            500,
        )]);
    for (error, for_caller, for_platform) in expected {
        let registry = MockTypesRegistry::new();
        registry.inject(Fault::on(Call::BatchGet).fail(error.clone()));
        let ctx = caller(1);

        let user =
            super::validate_new_user_payload_via_gts(&user_payload("alice"), &registry, &ctx)
                .await
                .expect_err("user payload read failed");
        let name = super::validate_tenant_name_via_gts(
            "acme",
            TypesRegistryReader::caller(&registry, &ctx),
        )
        .await
        .expect_err("tenant name read failed");
        for err in [user, name] {
            let (category, status) = at_the_boundary(err);
            assert_eq!(status, Some(for_caller), "{error:?} -> {category}");
            if for_caller != 503 && for_caller != 500 {
                assert_eq!(category, error.gts_type(), "{error:?}");
            }
        }

        let platform =
            super::validate_tenant_name_via_gts("acme", TypesRegistryReader::Platform(&registry))
                .await
                .expect_err("platform read failed");
        assert_eq!(at_the_boundary(platform).1, Some(for_platform), "{error:?}");
    }
}

// ---- materialized schema ------------------------------------------

#[tokio::test]
async fn user_payload_honours_properties_inherited_through_a_reference() {
    // The user schema takes `username` from a referenced schema; the
    // registry inlines the reference into `resolved_schema`.
    let fields = "gts.cf.test.am.user_fields.v1~";
    let registry = MockTypesRegistry::new();
    registry.seed(
        fields,
        schema_doc(
            fields,
            &json!({
                "type": "object",
                "properties": {
                    "username": { "type": "string", "minLength": 1, "maxLength": 8 },
                    "email": { "type": "string", "format": "email" },
                    "display_name": { "type": "string" },
                },
            }),
        ),
    );
    registry.seed(
        super::USER_TYPE_ID,
        schema_doc(
            super::USER_TYPE_ID,
            &json!({
                "type": "object",
                "allOf": [{ "$ref": gts_uri!(fields) }],
                "properties": { "id": { "type": "string", "format": "uuid" } },
            }),
        ),
    );

    super::validate_new_user_payload_via_gts(&user_payload("alice"), &registry, &caller(1))
        .await
        .expect("an inherited bound admits a short username");
    let err = super::validate_new_user_payload_via_gts(
        &user_payload("alexandria"),
        &registry,
        &caller(1),
    )
    .await
    .expect_err("the inherited maxLength applies");
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

#[tokio::test]
async fn user_payload_validates_against_the_published_user_schema() {
    let registry = MockTypesRegistry::new();
    registry.seed_inventory();
    let payload = user_payload(&"x".repeat(256));

    let err = super::validate_new_user_payload_via_gts(&payload, &registry, &caller(1))
        .await
        .expect_err("the published schema's username bound applies");
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

#[tokio::test]
async fn a_snapshot_without_the_selected_resolved_schema_is_internal() {
    let registry = Arc::new(registry_with_user_schema(255));
    let tampered = Tampered::without_documents(registry);

    let err =
        super::validate_new_user_payload_via_gts(&user_payload("alice"), &tampered, &caller(1))
            .await
            .expect_err("an incomplete projection is a contract break");
    assert!(matches!(err, DomainError::Internal { .. }), "got {err:?}");
}

#[tokio::test]
async fn a_deleted_user_schema_fails_closed_as_unregistered() {
    let registry = registry_with_user_schema(255);
    let content = registry.content(super::USER_TYPE_ID).expect("seeded");
    registry.seed_entity(Seed::new(super::USER_TYPE_ID, content).deleted());

    let err =
        super::validate_new_user_payload_via_gts(&user_payload("alice"), &registry, &caller(1))
            .await
            .expect_err("a tombstone is not a registered schema");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got {err:?}"
    );
}

// ---- conjunctive property constraints ------------------------------

fn seed_user_with(registry: &MockTypesRegistry, body: &Value) {
    registry.seed(super::USER_TYPE_ID, schema_doc(super::USER_TYPE_ID, body));
}

#[tokio::test]
async fn an_inline_all_of_bound_applies_beside_the_own_property() {
    let registry = MockTypesRegistry::new();
    seed_user_with(
        &registry,
        &json!({
            "type": "object",
            "properties": { "username": { "type": "string", "maxLength": 255 } },
            "allOf": [{ "properties": { "username": { "type": "string", "maxLength": 8 } } }],
        }),
    );

    let err = super::validate_new_user_payload_via_gts(
        &user_payload("alexandria"),
        &registry,
        &caller(1),
    )
    .await
    .expect_err("the tighter allOf bound applies");
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
    super::validate_new_user_payload_via_gts(&user_payload("alice"), &registry, &caller(1))
        .await
        .expect("both bounds admit a short name");
}

#[tokio::test]
async fn bounds_from_two_all_of_members_both_apply() {
    let registry = MockTypesRegistry::new();
    seed_user_with(
        &registry,
        &json!({
            "type": "object",
            "allOf": [
                { "properties": { "username": { "type": "string", "minLength": 3 } } },
                { "properties": { "username": { "type": "string", "maxLength": 8 } } },
            ],
        }),
    );

    for (name, ok) in [("al", false), ("alice", true), ("alexandria", false)] {
        let result =
            super::validate_new_user_payload_via_gts(&user_payload(name), &registry, &caller(1))
                .await;
        assert_eq!(result.is_ok(), ok, "{name}: {result:?}");
    }
}

#[tokio::test]
async fn a_referenced_bound_applies_beside_the_own_property_on_patch() {
    let fields = "gts.cf.test.am.user_bounds.v1~";
    let registry = MockTypesRegistry::new();
    registry.seed(
        fields,
        schema_doc(
            fields,
            &json!({
                "type": "object",
                "properties": { "username": { "type": "string", "maxLength": 8 } },
            }),
        ),
    );
    seed_user_with(
        &registry,
        &json!({
            "type": "object",
            "allOf": [{ "$ref": gts_uri!(fields) }],
            "properties": { "username": { "type": "string", "maxLength": 255 } },
        }),
    );
    let patch = account_management_sdk::IdpUserPatch::new().with_username("alexandria");

    let err = super::validate_user_patch_via_gts(&patch, &registry, &caller(1))
        .await
        .expect_err("the referenced bound applies");
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

#[tokio::test]
async fn an_inline_bound_applies_to_the_tenant_name() {
    let registry = MockTypesRegistry::new();
    registry.seed(
        super::TENANT_TYPE_ID,
        schema_doc(
            super::TENANT_TYPE_ID,
            &json!({
                "type": "object",
                "properties": { "name": { "type": "string", "maxLength": 255 } },
                "allOf": [{ "properties": { "name": { "type": "string", "maxLength": 4 } } }],
            }),
        ),
    );

    let err = super::validate_tenant_name_via_gts(
        "acme-corp",
        TypesRegistryReader::caller(&registry, &caller(1)),
    )
    .await
    .expect_err("the inline bound applies");
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

// ---- who the schema is read for (P33) -----------------------------

/// Two requests through one shared registry: each read names its own
/// caller, and neither reaches the platform API.
#[tokio::test]
async fn user_payload_and_patch_read_for_each_request_caller() {
    let spy = CallerSpy::for_requests(Arc::new(registry_with_user_schema(255)));
    let patch = account_management_sdk::IdpUserPatch::new().with_username("bob");

    for n in [1, 2] {
        let ctx = caller(n);
        super::validate_new_user_payload_via_gts(&user_payload("alice"), spy.as_ref(), &ctx)
            .await
            .expect("valid payload");
        super::validate_user_patch_via_gts(&patch, spy.as_ref(), &ctx)
            .await
            .expect("valid patch");
        assert_only_caller(&spy.callers(), &ctx);
        assert_eq!(spy.callers().len(), 2, "one schema read per helper");
        spy.clear();
    }
}

#[tokio::test]
async fn tenant_name_reads_for_each_request_caller() {
    let spy = CallerSpy::for_requests(Arc::new(registry_with_tenant_schema(255)));
    for n in [1, 2] {
        let ctx = caller(n);
        super::validate_tenant_name_via_gts(
            "acme",
            TypesRegistryReader::caller(spy.as_ref(), &ctx),
        )
        .await
        .expect("valid name");
        assert_only_caller(&spy.callers(), &ctx);
        spy.clear();
    }
}

/// A tenant-API failure is the request's answer: it is not retried on
/// the platform API, which the spy would refuse.
#[tokio::test]
async fn a_failed_request_read_does_not_fall_back_to_the_platform_api() {
    let spy = CallerSpy::for_requests(Arc::new(unavailable_registry()));
    let ctx = caller(1);
    let err = super::validate_tenant_name_via_gts(
        "acme",
        TypesRegistryReader::caller(spy.as_ref(), &ctx),
    )
    .await
    .expect_err("registry down");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got {err:?}"
    );
    assert_only_caller(&spy.callers(), &ctx);
    assert_eq!(spy.platform_reads(), 0);
}

/// The bootstrap saga has no request: its root-name check reads through
/// the platform API and never the tenant API.
#[tokio::test]
async fn the_platform_tenant_name_check_reads_only_the_platform_api() {
    let spy = CallerSpy::for_platform(Arc::new(registry_with_tenant_schema(4)));
    let err = super::validate_tenant_name_via_gts(
        "acme-corp",
        TypesRegistryReader::Platform(spy.as_ref()),
    )
    .await
    .expect_err("the platform read still applies the bound");
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
    assert_eq!(spy.platform_reads(), 1);
    assert!(spy.callers().is_empty());
}
