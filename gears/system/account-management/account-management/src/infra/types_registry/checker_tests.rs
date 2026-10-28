//! Tests for [`super::GtsTenantTypeChecker`].
//!
//! Extracted into a companion file per `cargo gears lint` rule `DE1101` (inline test
//! blocks > 100 lines must move out of the production source file).
//! Schemas are real documents in `MockTypesRegistry`, which materializes
//! `effective_traits` with `gts-rust` as admission does; the AM envelope is the
//! linked inventory declaration.

use super::*;
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::{GTS_ID_PREFIX, gts_id, gts_uri};
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

use crate::infra::types_registry::test_helpers::{
    CallerSpy, Tampered, assert_only_caller, at_the_boundary, caller, outages, refusals,
};

const PLATFORM: &str = gts_id!("cf.core.am.tenant_type.v1~acme.am.tenant_type.platform.v1~");
const CUSTOMER: &str = gts_id!("cf.core.am.tenant_type.v1~acme.am.tenant_type.customer.v1~");
const ALIEN_ROOT: &str = gts_id!("acme.core.events.type.v1~");
const ALIEN_LEAF: &str = gts_id!("acme.core.events.type.v1~acme.commerce.orders.order.v1~");

fn uuid(type_id: &str) -> Uuid {
    GtsId::try_new(type_id).expect("valid id").to_uuid()
}

/// A registry holding the linked inventory, the AM envelope among it.
fn registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed_inventory();
    registry
}

/// A tenant type derived from the envelope, declaring `allowed_parent_types`
/// when given.
fn seed_tenant_type(registry: &MockTypesRegistry, type_id: &str, allowed: Option<Value>) {
    let mut doc = json!({
        "$id": gts_uri!(type_id),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [{ "$ref": gts_uri!(TENANT_TYPE_BASE_GTS_ID) }],
    });
    if let Some(allowed) = allowed {
        doc["x-gts-traits"] = json!({ ALLOWED_PARENT_TYPES_TRAIT: allowed });
    }
    registry.seed(type_id, doc);
}

fn seed_alien(registry: &MockTypesRegistry) {
    registry.seed(
        ALIEN_ROOT,
        json!({
            "$id": gts_uri!(ALIEN_ROOT),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
        }),
    );
    registry.seed(
        ALIEN_LEAF,
        json!({
            "$id": gts_uri!(ALIEN_LEAF),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": gts_uri!(ALIEN_ROOT) }],
        }),
    );
}

#[tokio::test]
async fn admits_when_parent_in_child_allowed_parent_types() {
    let registry = registry();
    seed_tenant_type(&registry, PLATFORM, None);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(registry.clone());

    checker
        .check_parent_child(&caller(1), uuid(PLATFORM), uuid(CUSTOMER))
        .await
        .expect("admit when parent listed");
    // Single batch call — checker MUST NOT issue per-uuid round-trips.
    assert_eq!(registry.calls(Call::BatchGet), 1);
}

// -------- same-type nesting --------

#[tokio::test]
async fn admits_same_type_nesting_when_self_listed() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    let checker = GtsTenantTypeChecker::new(registry);

    checker
        .check_parent_child(&caller(1), uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect("admit when self-nesting allowed");
}

#[tokio::test]
async fn rejects_same_type_nesting_when_self_not_listed() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(&caller(1), uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("self-nesting must reject");
    assert!(matches!(err, DomainError::TypeNotAllowed { .. }));
    assert!(err.to_string().contains("same-type nesting"), "got: {err}");
}

// -------- rejection paths --------

#[tokio::test]
async fn rejects_when_parent_not_in_allowed_list() {
    let stranger = gts_id!("cf.core.am.tenant_type.v1~acme.am.tenant_type.stranger.v1~");
    let registry = registry();
    seed_tenant_type(&registry, stranger, None);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(&caller(1), uuid(stranger), uuid(CUSTOMER))
        .await
        .expect_err("parent not listed must reject");
    assert!(matches!(err, DomainError::TypeNotAllowed { .. }));
    assert_eq!(err.code(), "type_not_allowed");
}

#[tokio::test]
async fn a_parent_without_its_own_traits_gets_the_envelope_default() {
    // The envelope defaults `allowed_parent_types` to `[]`: a type that
    // declares nothing admits no parent, itself included.
    let registry = registry();
    seed_tenant_type(&registry, PLATFORM, None);
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(&caller(1), uuid(PLATFORM), uuid(PLATFORM))
        .await
        .expect_err("the default list is empty");
    assert!(
        matches!(err, DomainError::TypeNotAllowed { .. }),
        "got: {err}"
    );
}

#[tokio::test]
async fn rejects_child_not_under_envelope_as_invalid_tenant_type() {
    // Registered, but its chain root is not the AM tenant_type envelope.
    let registry = registry();
    seed_alien(&registry);
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(&caller(1), uuid(ALIEN_ROOT), uuid(ALIEN_LEAF))
        .await
        .expect_err("alien child must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("does not descend"), "got: {err}");
}

#[tokio::test]
async fn rejects_parent_not_under_envelope_as_invalid_tenant_type() {
    // The child lists the alien parent literally — a value its traits
    // schema (`x-gts-ref` to the envelope) keeps admission from storing —
    // and only the envelope re-check on the parent refuses it.
    let registry = registry();
    seed_alien(&registry);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(Arc::new(Tampered::new(registry, |entity| {
        if entity.gts_id.as_ref() == CUSTOMER {
            entity.effective_traits = Some(json!({ ALLOWED_PARENT_TYPES_TRAIT: [ALIEN_LEAF] }));
        }
    })));

    let err = checker
        .check_parent_child(&caller(1), uuid(ALIEN_LEAF), uuid(CUSTOMER))
        .await
        .expect_err("alien parent must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(
        err.to_string().contains("parent tenant type")
            && err.to_string().contains("does not descend"),
        "got: {err}"
    );
}

#[tokio::test]
async fn rejects_child_not_registered_as_invalid_tenant_type() {
    let checker = GtsTenantTypeChecker::new(registry());

    let err = checker
        .check_parent_child(&caller(1), Uuid::from_u128(0x1), Uuid::from_u128(0x2))
        .await
        .expect_err("unregistered child must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("child tenant type"), "got: {err}");
}

#[tokio::test]
async fn rejects_parent_not_registered_as_invalid_tenant_type() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(&caller(1), Uuid::from_u128(0xCAFE), uuid(CUSTOMER))
        .await
        .expect_err("missing parent must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("parent tenant type"), "got: {err}");
}

fn delete(registry: &MockTypesRegistry, type_id: &str) {
    let content = registry.content(type_id).expect("seeded");
    registry.seed_entity(Seed::new(type_id, content).deleted());
}

#[tokio::test]
async fn a_deleted_child_type_admits_no_same_type_nesting() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    delete(&registry, CUSTOMER);
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(&caller(1), uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("a tombstone is not a tenant type");
    assert!(
        matches!(err, DomainError::InvalidTenantType { .. }),
        "got: {err}"
    );
    assert!(err.to_string().contains("deleted"), "got: {err}");
}

#[tokio::test]
async fn a_deleted_child_type_admits_no_listed_parent() {
    let registry = registry();
    seed_tenant_type(&registry, PLATFORM, None);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    delete(&registry, CUSTOMER);
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(&caller(1), uuid(PLATFORM), uuid(CUSTOMER))
        .await
        .expect_err("a tombstone is not a tenant type");
    assert!(
        matches!(err, DomainError::InvalidTenantType { .. }),
        "got: {err}"
    );
}

#[tokio::test]
async fn a_deleted_parent_type_is_refused_even_when_listed() {
    let registry = registry();
    seed_tenant_type(&registry, PLATFORM, None);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    delete(&registry, PLATFORM);
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(&caller(1), uuid(PLATFORM), uuid(CUSTOMER))
        .await
        .expect_err("a deleted parent type is refused");
    assert!(
        matches!(err, DomainError::InvalidTenantType { .. }),
        "got: {err}"
    );
    assert!(err.to_string().contains("parent tenant type"), "got: {err}");
}

// -------- malformed trait values --------

#[tokio::test]
async fn rejects_when_allowed_parent_types_contains_an_instance_identifier() {
    // One malformed entry collapses the whole list, so a sibling
    // well-formed entry cannot admit through it.
    // Admission refuses such a document by the envelope's `x-gts-ref`.
    let instance =
        format!("{GTS_ID_PREFIX}cf.core.am.tenant_type.v1~acme.am.tenant.customer.v1~prod_acct_42");
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    let checker = GtsTenantTypeChecker::new(Arc::new(Tampered::new(registry, move |entity| {
        entity.effective_traits = Some(json!({ ALLOWED_PARENT_TYPES_TRAIT: [CUSTOMER, instance] }));
    })));

    let err = checker
        .check_parent_child(&caller(1), uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("malformed entry must collapse the list to InvalidTenantType");
    assert!(
        matches!(err, DomainError::InvalidTenantType { .. }),
        "got: {err}"
    );
}

#[tokio::test]
async fn rejects_when_allowed_parent_types_is_not_an_array() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    let checker = GtsTenantTypeChecker::new(Arc::new(Tampered::new(registry, |entity| {
        entity.effective_traits = Some(json!({ ALLOWED_PARENT_TYPES_TRAIT: "not an array" }));
    })));

    let err = checker
        .check_parent_child(&caller(1), uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("malformed trait must reject");
    assert!(
        matches!(err, DomainError::InvalidTenantType { .. }),
        "got: {err}"
    );
}

// -------- registry faults and contract breaks --------

/// A failed read for the request: an outage is a retryable 503, the
/// registry's refusal of the caller stays that caller's 401 / 403, and
/// anything else is internal.
#[tokio::test]
async fn a_failed_batch_read_is_classified_for_the_caller() {
    let expected = outages()
        .into_iter()
        .map(|error| (error, Some(503)))
        .chain(
            refusals()
                .into_iter()
                .map(|(error, status)| (error, Some(status))),
        )
        .chain([(
            CanonicalError::internal("registry exploded").create(),
            Some(500),
        )]);
    for (error, status) in expected {
        let registry = registry();
        seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
        registry.inject(Fault::on(Call::BatchGet).fail(error.clone()));
        let checker = GtsTenantTypeChecker::new(registry);

        let err = checker
            .check_parent_child(&caller(1), uuid(CUSTOMER), uuid(CUSTOMER))
            .await
            .expect_err("registry error must propagate");
        let (category, public_status) = at_the_boundary(err);
        assert_eq!(public_status, status, "{error:?} -> {category}");
        if status != Some(503) && status != Some(500) {
            assert_eq!(category, error.gts_type(), "{error:?}");
        }
    }
}

#[tokio::test]
async fn a_snapshot_without_the_selected_effective_traits_is_not_an_empty_list() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    let checker = GtsTenantTypeChecker::new(Arc::new(Tampered::without_documents(registry)));

    let err = checker
        .check_parent_child(&caller(1), uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("an incomplete projection is a contract break");
    assert!(matches!(err, DomainError::Internal { .. }), "got: {err}");
}

#[tokio::test(start_paused = true)]
async fn rejects_when_registry_probe_times_out() {
    let registry = registry();
    registry.inject(Fault::on(Call::BatchGet).delay(Duration::from_millis(50)));
    let checker = GtsTenantTypeChecker::with_timeout(registry, 10);

    let err = checker
        .check_parent_child(&caller(1), Uuid::from_u128(0x1), Uuid::from_u128(0x1))
        .await
        .expect_err("slow registry must time out");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert!(
        err.to_string().contains("types-registry: timeout exceeded"),
        "got: {err}"
    );
}

// -------- who the schemas are read for (P33) --------

/// One checker serves every request: each check reads for the caller it
/// was given, and none reaches the platform API.
#[tokio::test]
async fn each_check_reads_for_its_own_caller() {
    let registry = registry();
    seed_tenant_type(&registry, PLATFORM, None);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let spy = CallerSpy::for_requests(registry);
    let checker = GtsTenantTypeChecker::new(spy.clone());

    for n in [1, 2] {
        let ctx = caller(n);
        checker
            .check_parent_child(&ctx, uuid(PLATFORM), uuid(CUSTOMER))
            .await
            .expect("admit when parent listed");
        assert_only_caller(&spy.callers(), &ctx);
        spy.clear();
    }
}

/// A failed tenant read is the check's answer; it is not retried on the
/// platform API, which the spy refuses.
#[tokio::test]
async fn a_failed_read_does_not_fall_back_to_the_platform_api() {
    let registry = registry();
    registry.inject(Fault::on(Call::BatchGet).fail(CanonicalError::service_unavailable().create()));
    let spy = CallerSpy::for_requests(registry);
    let checker = GtsTenantTypeChecker::new(spy.clone());
    let ctx = caller(1);

    let err = checker
        .check_parent_child(&ctx, uuid(PLATFORM), uuid(CUSTOMER))
        .await
        .expect_err("registry down");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got {err:?}"
    );
    assert_only_caller(&spy.callers(), &ctx);
    assert_eq!(spy.platform_reads(), 0);
}
