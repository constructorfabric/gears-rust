//! Unit tests for [`UserService`].
//!
//! Every test wires the service against [`FakeTenantRepo`] +
//! [`FakeIdpUserProvisioner`]. Pins:
//!
//! * Guard ordering: tenant existence + Active precondition runs
//!   BEFORE any `IdP` call, so a non-existent / non-Active tenant
//!   surfaces as `NotFound` / `Validation` and `idp_call_count == 0`.
//! * Error mapping: `Unavailable` -> [`DomainError::IdpUnavailable`],
//!   `UnsupportedOperation` -> [`DomainError::UnsupportedOperation`],
//!   `Rejected` -> [`DomainError::Validation`] -- per the
//!   `feature-errors-observability` envelope mapping.
//! * Idempotency: every `Ok(())` from the plugin surfaces as
//!   `Ok(())` from the service (plugins map vendor "user does not
//!   exist" responses to `Ok(())` themselves); `Unavailable` and
//!   `UnsupportedOperation` pass through unchanged.
//! * `list_users` filter: `?user_id=<id>` returns 0 or 1 results
//!   matching the authoritative existence signal contract.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::sync::Arc;

use account_management_sdk::{IdpNewUser, IdpUser, IdpUserPagination, ListUsersQuery};
use modkit_security::SecurityContext;
use serde_json::json;
use time::OffsetDateTime;
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{GtsTypeId, GtsTypeSchema};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::tenant::model::{TenantModel, TenantStatus};
use crate::domain::tenant::test_support::{FakeTenantRepo, mock_enforcer};
use crate::domain::user::service::UserService;
use crate::domain::user::test_support::{FakeIdpUserProvisioner, FakeUserOutcome};

/// Canonical chained `tenant_type` every seeded tenant carries. The
/// matching `GtsTypeSchema` is pre-registered in [`make_service`]'s
/// `MockTypesRegistryClient` so `UserService::resolve_active_tenant`
/// can resolve the typed `GtsSchemaId` via
/// `get_type_schema_by_uuid` (mandatory after the IdP-metadata
/// isolation refactor — a missing schema now surfaces as
/// `ServiceUnavailable` instead of `Option::None`). Tests that need
/// a specific failure shape override the registry stub.
const TEST_TENANT_TYPE_ID: &str = "gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~";

fn test_tenant_type_uuid() -> Uuid {
    gts::GtsID::new(TEST_TENANT_TYPE_ID)
        .expect("hardcoded chain is valid")
        .to_uuid()
}

/// Minimal `GtsTypeSchema` matching [`TEST_TENANT_TYPE_ID`]. The body
/// is an empty object — `resolve_active_tenant` only reads `type_id`
/// off the result, so the schema content does not affect any tests.
/// Derived chains require a parent reference; we build one
/// recursively up to the base.
fn test_tenant_type_schema() -> GtsTypeSchema {
    fn build(type_id: &str) -> GtsTypeSchema {
        let parent = GtsTypeSchema::derive_parent_type_id(type_id)
            .map(|p| std::sync::Arc::new(build(p.as_ref())));
        GtsTypeSchema::try_new(GtsTypeId::new(type_id), json!({}), None, parent)
            .expect("synthetic tenant_type schema is valid")
    }
    build(TEST_TENANT_TYPE_ID)
}

// ---- helpers -------------------------------------------------------

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch")
}

/// Subject tenant id used by every service-test `ctx()`. The
/// `mock_enforcer` returns an `InTenantSubtree` predicate rooted at
/// this id, and `seed_tenant` materialises closure rows
/// `(subject_root, tenant, barrier = 0)` so the PEP-derived scope
/// resolves to a non-empty visible set on the `FakeTenantRepo`.
const fn ctx_subject_root() -> Uuid {
    Uuid::from_u128(0xCAFE_BABE)
}

fn ctx() -> SecurityContext {
    // The seeded `subject_tenant_id` matches `ctx_subject_root()` so
    // closure-row seeding in `seed_tenant` keeps every test tenant
    // visible under the compiled PEP scope. Tests asserting on the
    // PEP-deny path build a different ctx via `ctx_for(...)`.
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xCAFE))
        .subject_tenant_id(ctx_subject_root())
        .build()
        .expect("ctx")
}

fn ctx_for(root: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xCAFE))
        .subject_tenant_id(root)
        .build()
        .expect("ctx")
}

fn user_schema() -> GtsTypeSchema {
    let body = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "username"],
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "username": { "type": "string", "minLength": 1, "maxLength": 255 },
            "email": { "type": "string", "format": "email" },
            "display_name": { "type": "string", "minLength": 1, "maxLength": 255 },
        },
    });
    GtsTypeSchema::try_new(GtsTypeId::new("gts.cf.core.am.user.v1~"), body, None, None)
        .expect("synthetic user schema is valid")
}

fn make_service(tenants: Arc<FakeTenantRepo>, idp: Arc<FakeIdpUserProvisioner>) -> UserService {
    // Registry pre-loaded with [`TEST_TENANT_TYPE_ID`] so
    // `resolve_active_tenant` (which now treats `tenant_type` as
    // mandatory) can resolve the seeded tenants' chained type, AND
    // with the `gts.cf.core.am.user.v1~` user-projection schema so
    // `validate_new_user_payload_via_gts` reaches the registered-
    // schema path. `create_user` is fail-closed on a missing user
    // schema (see `validate_new_user_payload_via_gts` doc), so any
    // happy-path test would otherwise short-circuit to
    // `ServiceUnavailable` before reaching the IdP. A dedicated test
    // (`create_user_without_registered_user_schema_returns_service_unavailable`)
    // pins the fail-closed behaviour with a registry that omits the
    // user schema.
    let types_registry = Arc::new(
        MockTypesRegistryClient::new()
            .with_type_schemas([test_tenant_type_schema(), user_schema()]),
    );
    UserService::new(tenants, idp, types_registry, mock_enforcer())
}

fn make_service_without_user_schema(
    tenants: Arc<FakeTenantRepo>,
    idp: Arc<FakeIdpUserProvisioner>,
) -> UserService {
    // Mirrors `make_service` but omits the `gts.cf.core.am.user.v1~`
    // schema so `validate_new_user_payload_via_gts` exercises its
    // fail-closed arm. Drives the
    // `create_user_without_registered_user_schema_returns_service_unavailable`
    // service-layer contract test.
    let types_registry =
        Arc::new(MockTypesRegistryClient::new().with_type_schemas([test_tenant_type_schema()]));
    UserService::new(tenants, idp, types_registry, mock_enforcer())
}

fn seed_tenant(
    fake: &FakeTenantRepo,
    id: Uuid,
    parent_id: Option<Uuid>,
    status: TenantStatus,
    name: &str,
) {
    let now = fixed_now();
    let depth = u32::from(parent_id.is_some());
    fake.insert_tenant_raw(TenantModel {
        id,
        parent_id,
        name: name.to_owned(),
        status,
        self_managed: false,
        tenant_type_uuid: test_tenant_type_uuid(),
        depth,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });
    // Mirror metadata service tests: closure rows the
    // `MockAuthZResolver`-derived `InTenantSubtree` predicate
    // consults via `FakeTenantRepo::visible_ids_for`. Without
    // these every PEP-derived scope would resolve to an empty
    // visible set and every tenant lookup would surface as
    // `NotFound`.
    let subject_root = ctx_subject_root();
    fake.seed_closure(subject_root, id, 0, status);
    if subject_root != id {
        fake.seed_closure(id, id, 0, status);
    }
}

fn payload(username: &str) -> IdpNewUser {
    IdpNewUser::new(username.to_owned())
        .with_email(format!("{username}@example.com"))
        .with_display_name(username.to_owned())
}

fn pagination() -> IdpUserPagination {
    IdpUserPagination::new(50, None).expect("top=50 is valid")
}

/// Pagination shape required by `list_users` when `user_id_filter`
/// is set: AM-side validation pins `top=1, cursor=None` so the
/// filtered lookup matches the authoritative-existence-check
/// semantics documented on the SDK.
fn filter_pagination() -> IdpUserPagination {
    IdpUserPagination::new(1, None).expect("top=1 is valid")
}

// ---- create_user -----------------------------------------------

#[tokio::test]
async fn create_user_happy_path_returns_projection() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");

    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let pinned_id = Uuid::from_u128(0xABCD);
    idp.set_create_projection(
        IdpUser::new(pinned_id, "alice")
            .with_email("alice@example.com")
            .with_display_name("Alice"),
    );
    let svc = make_service(tenants, idp.clone());

    let projection = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect("happy path provision");

    assert_eq!(
        projection.id, pinned_id,
        "service forwards the provider-assigned IdP id verbatim"
    );
    assert_eq!(idp.create_call_count(), 1);
    let calls = idp.create_calls_snapshot();
    assert_eq!(calls[0].0, tenant_id, "tenant scope is forwarded");
    assert_eq!(calls[0].1, "alice");
}

/// Mirror of [`create_user_happy_path_returns_projection`] with
/// the `gts.cf.core.am.user.v1~` user-projection schema actually
/// registered in the types registry. Pins that the service-layer
/// happy path traverses the registered-schema validator without
/// short-circuiting to the unknown-schema arm. Closes deep-review
/// #20.
#[tokio::test]
async fn create_user_happy_path_with_registered_user_schema_returns_projection() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");

    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let pinned_id = Uuid::from_u128(0xABCE);
    idp.set_create_projection(
        IdpUser::new(pinned_id, "alice")
            .with_email("alice@example.com")
            .with_display_name("Alice"),
    );
    let svc = make_service(tenants, idp.clone());

    let projection = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect("happy path provision with registered user schema");

    assert_eq!(
        projection.id, pinned_id,
        "service forwards the provider-assigned IdP id verbatim"
    );
    assert_eq!(
        idp.create_call_count(),
        1,
        "IdP must be called once when the registered schema admits the payload"
    );
}

/// Service-layer contract test for the fail-closed
/// `validate_new_user_payload_via_gts` arm: when the registry has no
/// `gts.cf.core.am.user.v1~` entry, `create_user` MUST surface
/// `ServiceUnavailable` rather than degrading to length-only checks.
/// Pins that the deployment cliff (`format: email` and future
/// `pattern` rules silently disabled before catalog seed) cannot
/// recur.
#[tokio::test]
async fn create_user_without_registered_user_schema_returns_service_unavailable() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");

    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service_without_user_schema(tenants, idp.clone());

    let err = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect_err(
            "user schema unregistered MUST fail closed -- no AM-side fallback gate \
             exists for users, so degrading to length-only would silently disable \
             format/pattern rules until the catalog is seeded",
        );

    match err {
        DomainError::ServiceUnavailable { detail, .. } => {
            assert!(
                detail.contains("gts.cf.core.am.user.v1~") && detail.contains("catalog"),
                "ServiceUnavailable.detail must name the missing schema and the \
                 catalog-seed remediation; got: {detail}"
            );
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
    assert_eq!(
        idp.create_call_count(),
        0,
        "fail-closed validation MUST run before any IdP call"
    );
}

#[tokio::test]
async fn create_user_rejects_unknown_tenant_with_not_found_no_idp_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let unknown = Uuid::from_u128(0x2);
    let err = svc
        .create_user(&ctx(), unknown, payload("alice"))
        .await
        .expect_err("unknown tenant must reject");

    match err {
        DomainError::NotFound { resource, .. } => {
            assert_eq!(resource, unknown.to_string());
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert_eq!(
        idp.create_call_count(),
        0,
        "tenant guard runs before any IdP call"
    );
}

/// Cross-tenant denial coverage on the user-side public seam. The
/// tenant exists but the caller's `AccessScope` is narrowed to a
/// foreign tenant — the `resolve_active_tenant` lookup forwards
/// the caller scope and surfaces the seeded tenant as `NotFound`.
/// An internal actor that holds a `create_user` capability
/// cannot probe tenant existence by submitting requests against
/// tenants outside its scope. Closes deep-review #8 user-side
/// coverage.
#[tokio::test]
async fn create_user_under_restricted_scope_collapses_to_not_found_no_idp_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x1);
    let foreign = Uuid::from_u128(0x99);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");

    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let restricted = ctx_for(foreign);
    let err = svc
        .create_user(&restricted, tenant_id, payload("alice"))
        .await
        .expect_err("out-of-scope caller must not see the tenant");
    match err {
        DomainError::NotFound { resource, .. } => {
            assert_eq!(resource, tenant_id.to_string());
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert_eq!(
        idp.create_call_count(),
        0,
        "scope guard runs before any IdP call"
    );
}

#[tokio::test]
async fn create_user_rejects_suspended_tenant_with_validation_no_idp_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x10);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Suspended, "frozen");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let err = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect_err("suspended tenant must reject");

    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("suspended"),
                "validation must surface the rejected status; got {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
    assert_eq!(idp.create_call_count(), 0);
}

#[tokio::test]
async fn create_user_rejects_provisioning_tenant_with_validation_no_idp_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x11);
    seed_tenant(
        &tenants,
        tenant_id,
        None,
        TenantStatus::Provisioning,
        "mid-saga",
    );
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let err = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect_err("provisioning tenant must reject");

    assert!(matches!(err, DomainError::Validation { .. }));
    assert_eq!(idp.create_call_count(), 0);
}

#[tokio::test]
async fn create_user_idp_unavailable_maps_to_idp_unavailable() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x20);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    idp.set_create_outcome(FakeUserOutcome::Unavailable);
    let svc = make_service(tenants, idp);

    let err = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect_err("unavailable must err");

    assert!(matches!(err, DomainError::IdpUnavailable { .. }));
}

#[tokio::test]
async fn create_user_idp_unsupported_maps_to_unsupported_operation() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x21);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    idp.set_create_outcome(FakeUserOutcome::Unsupported);
    let svc = make_service(tenants, idp);

    let err = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect_err("unsupported must err");

    assert!(matches!(err, DomainError::UnsupportedOperation { .. }));
}

#[tokio::test]
async fn create_user_idp_rejects_payload_maps_to_validation() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x22);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    idp.set_create_outcome(FakeUserOutcome::RejectPayload);
    let svc = make_service(tenants, idp);

    let err = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect_err("rejected payload must err");

    assert!(matches!(err, DomainError::Validation { .. }));
}

// ---- delete_user ---------------------------------------------

#[tokio::test]
async fn delete_user_happy_path_removed() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x30);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let user_id = Uuid::from_u128(0xBEEF);
    svc.delete_user(&ctx(), tenant_id, user_id)
        .await
        .expect("happy path deprovision returns Ok");
    assert_eq!(idp.delete_call_count(), 1);
    let calls = idp.delete_calls_snapshot();
    assert_eq!(calls[0], (tenant_id, user_id));
}

#[tokio::test]
async fn delete_user_retry_remains_idempotent() {
    // AC #5: a subsequent retry of the same DELETE also returns 204.
    // The plugin contract folds removed-vs-absent into a single
    // `Ok(())` (plugin maps vendor 404 / 410 to `Ok(())` itself), so
    // back-to-back DELETE calls both surface as `Ok(())` to the
    // service. Pin the service-side guard that forwards each retry
    // to the plugin without short-circuiting.
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x34);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let user_id = Uuid::from_u128(0xBEEF_BEEF);

    svc.delete_user(&ctx(), tenant_id, user_id)
        .await
        .expect("first delete returns Ok(())");

    svc.delete_user(&ctx(), tenant_id, user_id)
        .await
        .expect("retry returns Ok(()) - idempotent contract");

    assert_eq!(
        idp.delete_call_count(),
        2,
        "service forwarded both retry attempts to the IdP"
    );
}

#[tokio::test]
async fn delete_user_idp_unavailable_does_not_become_idempotent_success() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x32);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    idp.set_delete_outcome(FakeUserOutcome::Unavailable);
    let svc = make_service(tenants, idp);

    let user_id = Uuid::from_u128(0xDEAD);
    let err = svc
        .delete_user(&ctx(), tenant_id, user_id)
        .await
        .expect_err("unavailable must NOT collapse to idempotent success");
    assert!(
        matches!(err, DomainError::IdpUnavailable { .. }),
        "unavailable must surface unchanged; got {err:?}"
    );
}

#[tokio::test]
async fn delete_user_unsupported_passes_through() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x33);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    idp.set_delete_outcome(FakeUserOutcome::Unsupported);
    let svc = make_service(tenants, idp);

    let err = svc
        .delete_user(&ctx(), tenant_id, Uuid::from_u128(0xBEEF))
        .await
        .expect_err("unsupported must surface unchanged");
    assert!(matches!(err, DomainError::UnsupportedOperation { .. }));
}

#[tokio::test]
async fn delete_user_rejects_unknown_tenant_no_idp_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let unknown = Uuid::from_u128(0x40);
    let err = svc
        .delete_user(&ctx(), unknown, Uuid::from_u128(0xBEEF))
        .await
        .expect_err("unknown tenant must reject");
    assert!(matches!(err, DomainError::NotFound { .. }));
    assert_eq!(idp.delete_call_count(), 0);
}

// ---- list_users ---------------------------------------------------

#[tokio::test]
async fn list_users_happy_path_returns_page_through_idp() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x50);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    idp.set_list_items(vec![
        IdpUser::new(Uuid::from_u128(0xA1), "alice"),
        IdpUser::new(Uuid::from_u128(0xA2), "bob"),
    ]);
    let svc = make_service(tenants, idp);

    let page = svc
        .list_users(&ctx(), tenant_id, ListUsersQuery::new(pagination()))
        .await
        .expect("happy path list");
    assert_eq!(page.items.len(), 2);
    assert_eq!(
        page.page_info.next_cursor, None,
        "two items fit in top=50 so the listing is exhausted in one page"
    );
}

#[tokio::test]
async fn list_users_user_id_filter_returns_one_or_empty() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x51);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let target = Uuid::from_u128(0xA1);
    let other = Uuid::from_u128(0xA2);
    idp.set_list_items(vec![
        IdpUser::new(target, "alice"),
        IdpUser::new(other, "bob"),
    ]);
    let svc = make_service(tenants, idp);

    let hit = svc
        .list_users(
            &ctx(),
            tenant_id,
            ListUsersQuery::new(filter_pagination()).with_user_id_filter(target),
        )
        .await
        .expect("filter by existing user id");
    assert_eq!(hit.items.len(), 1, "single-user filter returns 1 row");
    assert_eq!(hit.items[0].id, target);

    let absent = Uuid::from_u128(0xDEAD);
    let miss = svc
        .list_users(
            &ctx(),
            tenant_id,
            ListUsersQuery::new(filter_pagination()).with_user_id_filter(absent),
        )
        .await
        .expect("filter by absent user id is success-with-empty-list");
    assert!(
        miss.items.is_empty(),
        "absent user id must surface as empty list, NOT NotFound"
    );
}

#[tokio::test]
async fn list_users_idp_unavailable_does_not_serve_stale_page() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x52);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    idp.set_list_outcome(FakeUserOutcome::Unavailable);
    let svc = make_service(tenants, idp);

    let err = svc
        .list_users(&ctx(), tenant_id, ListUsersQuery::new(pagination()))
        .await
        .expect_err("unavailable must err");
    assert!(matches!(err, DomainError::IdpUnavailable { .. }));
}

#[tokio::test]
async fn list_users_rejects_unknown_tenant_no_idp_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let unknown = Uuid::from_u128(0x53);
    let err = svc
        .list_users(&ctx(), unknown, ListUsersQuery::new(pagination()))
        .await
        .expect_err("unknown tenant must reject");
    assert!(matches!(err, DomainError::NotFound { .. }));
    assert_eq!(idp.list_call_count(), 0);
}

#[tokio::test]
async fn list_users_rejects_deleted_tenant_no_idp_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x54);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Deleted, "gone");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let err = svc
        .list_users(&ctx(), tenant_id, ListUsersQuery::new(pagination()))
        .await
        .expect_err("deleted tenant must reject");
    assert!(matches!(err, DomainError::Validation { .. }));
    assert_eq!(idp.list_call_count(), 0);
}

// ---- get_user ------------------------------------------------------

#[tokio::test]
async fn get_user_happy_path_returns_user() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x55);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let target = Uuid::from_u128(0xB1);
    idp.set_list_items(vec![IdpUser::new(target, "alice")]);
    let svc = make_service(tenants, idp);

    let user = svc
        .get_user(&ctx(), tenant_id, target)
        .await
        .expect("get_user returns the matching user");
    assert_eq!(user.id, target);
    assert_eq!(user.username, "alice");
}

#[tokio::test]
async fn get_user_absent_user_id_collapses_to_not_found() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x56);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    // No items configured -- list_users returns an empty page.
    let svc = make_service(tenants, idp);

    let absent = Uuid::from_u128(0xBEEF);
    let err = svc
        .get_user(&ctx(), tenant_id, absent)
        .await
        .expect_err("absent user surfaces as UserNotFound (not empty page)");
    match err {
        DomainError::UserNotFound { resource, .. } => {
            assert_eq!(resource, absent.to_string());
        }
        other => panic!("expected UserNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn get_user_rejects_unknown_tenant_no_idp_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let unknown_tenant = Uuid::from_u128(0x57);
    let err = svc
        .get_user(&ctx(), unknown_tenant, Uuid::from_u128(0xB2))
        .await
        .expect_err("unknown tenant must reject before IdP call");
    assert!(matches!(err, DomainError::NotFound { .. }));
    assert_eq!(
        idp.list_call_count(),
        0,
        "tenant guard runs before the IdP plugin call"
    );
}

#[tokio::test]
async fn list_users_with_user_id_filter_and_cursor_rejects_validation_no_idp_call() {
    // The `user_id_filter = Some(_)` call is an authoritative
    // existence check (see SDK doc on `IdpListUsersRequest::user_id_filter`).
    // Forwarding a continuation cursor would let the provider step
    // past the matching row and turn the existence check into a
    // false negative (downstream feature-user-groups would think the
    // user does not exist). The service guard rejects this
    // combination at the AM boundary so the misuse surfaces as HTTP
    // 400 instead of silent miscorrelation.
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x55);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let user_id = Uuid::from_u128(0xA1);
    let with_cursor = IdpUserPagination::new(1, Some("opaque-continuation".to_owned()))
        .expect("top=1 + cursor is structurally valid pagination");
    let err = svc
        .list_users(
            &ctx(),
            tenant_id,
            ListUsersQuery::new(with_cursor).with_user_id_filter(user_id),
        )
        .await
        .expect_err("user_id_filter + cursor must reject at the AM boundary");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("cursor MUST be absent"),
                "validation detail MUST name the offending invariant; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
    assert_eq!(
        idp.list_call_count(),
        0,
        "AM-side guard MUST short-circuit before any IdP call"
    );
}

#[tokio::test]
async fn list_users_with_user_id_filter_and_no_cursor_passes_through() {
    // Happy-path counterpart to the rejection test: `cursor = None`
    // is the valid combo with `user_id_filter` and must reach the IdP.
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x56);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let user_id = Uuid::from_u128(0xA2);
    let _ = svc
        .list_users(
            &ctx(),
            tenant_id,
            ListUsersQuery::new(filter_pagination()).with_user_id_filter(user_id),
        )
        .await
        .expect("user_id_filter + cursor=None must reach the IdP");
    assert_eq!(idp.list_call_count(), 1);
}

#[tokio::test]
async fn list_users_with_user_id_filter_and_top_gt_one_rejects_validation_no_idp_call() {
    // Existence-check semantics require `top = 1` when a filter is
    // set; an oversized `top` would forward to a vendor that ignores
    // the filter, returning up to `top` unrelated rows, and surface
    // the caller-side bug as `Internal` (HTTP 500) downstream
    // instead of catching it at the AM seam.
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x57);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let user_id = Uuid::from_u128(0xA3);
    let err = svc
        .list_users(
            &ctx(),
            tenant_id,
            ListUsersQuery::new(pagination()).with_user_id_filter(user_id),
        )
        .await
        .expect_err("user_id_filter + top>1 must reject at the AM boundary");
    match err {
        DomainError::Validation { detail } => assert!(
            detail.contains("top MUST be 1"),
            "validation detail MUST name the offending invariant; got: {detail}"
        ),
        other => panic!("expected Validation, got {other:?}"),
    }
    assert_eq!(idp.list_call_count(), 0, "must not reach the IdP");
}

#[tokio::test]
async fn create_user_rejects_oversized_username_no_idp_call() {
    // AM-side cap of 255 characters fires before the IdP round-trip
    // so the provider never sees megabyte-scale identifiers. This
    // guard is the load-bearing fallback when the GTS user schema
    // is not yet registered (no DB CHECK exists for users).
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x60);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let oversized = "a".repeat(256);
    // Username-only payload: leave `email` / `display_name` unset
    // so the oversized-profile-field guard cannot fire first and
    // mask the username-specific cap.
    let p = IdpNewUser::new(oversized);
    let err = svc
        .create_user(&ctx(), tenant_id, p)
        .await
        .expect_err("256-char username must reject");
    match err {
        DomainError::Validation { detail } => assert!(
            detail.contains("username") && detail.contains("255 characters"),
            "validation detail MUST name the username cap; got: {detail}"
        ),
        other => panic!("expected Validation, got {other:?}"),
    }
    assert_eq!(idp.create_call_count(), 0, "must not reach the IdP");
}

#[tokio::test]
async fn create_user_rejects_whitespace_only_username_no_idp_call() {
    // `"   "` passes the schema's `minLength: 1` but is semantically
    // empty for a login identifier — caught explicitly at the AM
    // service layer so two callers writing `"alice"` and
    // `"  alice  "` cannot create one or two users depending on
    // vendor whitespace semantics.
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x61);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    // Username-only payload: leave the optional profile fields unset
    // so a sibling guard cannot fire first and mask the username-
    // specific "all-whitespace" check.
    let p = IdpNewUser::new("   ");
    let err = svc
        .create_user(&ctx(), tenant_id, p)
        .await
        .expect_err("whitespace-only username must reject");
    match err {
        DomainError::Validation { detail } => assert!(
            detail.contains("all-whitespace"),
            "validation detail MUST name the offending invariant; got: {detail}"
        ),
        other => panic!("expected Validation, got {other:?}"),
    }
    assert_eq!(idp.create_call_count(), 0, "must not reach the IdP");
}

/// Pins the plugin-contract guard on `create_user`: a provider
/// returning `Uuid::nil()` as the IdP-issued user id is a contract
/// violation that MUST surface as `Internal` rather than be
/// forwarded into `am.events` / downstream membership writes.
/// Closes deep-review #2.
#[tokio::test]
async fn create_user_rejects_nil_projection_id_from_plugin() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x84);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    idp.set_create_projection(IdpUser::new(Uuid::nil(), "alice"));
    let svc = make_service(tenants, idp.clone());

    let err = svc
        .create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect_err("nil projection id MUST surface as Internal");
    match err {
        DomainError::Internal { diagnostic, .. } => assert!(
            diagnostic.contains("Uuid::nil()") && diagnostic.contains("plugin contract violation"),
            "Internal diagnostic MUST name the contract violation; got: {diagnostic}"
        ),
        other => panic!("expected Internal, got {other:?}"),
    }
    assert_eq!(
        idp.create_call_count(),
        1,
        "IdP was called once; the guard fires on the response, not before"
    );
}

/// Pins the metadata round-trip on every user-ops call:
/// `UserService::resolve_active_tenant` MUST load
/// `tenant_idp_metadata` via `TenantRepo::find_idp_metadata` and
/// forward it verbatim into `TenantContext::metadata` on each `IdP`
/// method (`create_user`, `delete_user`, `list_users`).
/// Closes deep-review #1 — without this seam, a regression dropping
/// the metadata-load step would pass every existing test (all of
/// which seed `idp_metadata = None`).
#[tokio::test]
async fn user_ops_forward_tenant_idp_metadata_into_tenant_context_on_every_call() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x70);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let plugin_blob = json!({"realm": "acme-prod", "vendor_token": "opaque-blob-1"});
    tenants.seed_idp_metadata(tenant_id, Some(plugin_blob.clone()));

    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let user_id = Uuid::from_u128(0x00C0_FFEE);
    idp.set_create_projection(IdpUser::new(user_id, "alice"));
    idp.set_list_items(vec![IdpUser::new(user_id, "alice")]);
    let svc = make_service(tenants, idp.clone());

    svc.create_user(&ctx(), tenant_id, payload("alice"))
        .await
        .expect("provision happy path");
    svc.delete_user(&ctx(), tenant_id, user_id)
        .await
        .expect("deprovision happy path");
    svc.list_users(&ctx(), tenant_id, ListUsersQuery::new(pagination()))
        .await
        .expect("list happy path");

    let expected = vec![Some(plugin_blob)];
    assert_eq!(
        idp.create_metadata_snapshots(),
        expected,
        "create_user MUST forward tenant_idp_metadata blob on the IdP call"
    );
    assert_eq!(
        idp.delete_metadata_snapshots(),
        expected,
        "delete_user MUST forward tenant_idp_metadata blob on the IdP call"
    );
    assert_eq!(
        idp.list_metadata_snapshots(),
        expected,
        "list_users MUST forward tenant_idp_metadata blob on the IdP call"
    );
}

#[tokio::test]
async fn create_user_rejects_oversized_profile_fields_no_idp_call() {
    // Defence-in-depth caps on `email`, `display_name` run AFTER
    // tenant guard but BEFORE GTS round-trip / IdP call so
    // megabyte-scale optional fields don't reach the provider when
    // the GTS user schema is not registered.
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x62);
    seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeIdpUserProvisioner::new());
    let svc = make_service(tenants, idp.clone());

    let oversized = "a".repeat(256);

    let p_email = IdpNewUser::new("alice").with_email(oversized.clone());
    let err = svc
        .create_user(&ctx(), tenant_id, p_email)
        .await
        .expect_err("oversized email must reject");
    assert!(
        matches!(&err, DomainError::Validation { detail } if detail.contains("email")),
        "expected Validation naming email; got: {err:?}"
    );

    let p_display = IdpNewUser::new("alice").with_display_name(oversized);
    let err = svc
        .create_user(&ctx(), tenant_id, p_display)
        .await
        .expect_err("oversized display_name must reject");
    assert!(
        matches!(&err, DomainError::Validation { detail } if detail.contains("display_name")),
        "expected Validation naming display_name; got: {err:?}"
    );

    assert_eq!(idp.create_call_count(), 0, "no IdP call on any reject path");
}

// ---- delete_user → RG user-group membership cleanup ----------
//
// Tests the post-IdP-success cleanup of RG user-group memberships
// referencing the deprovisioned user. Cleanup is a two-step pipeline:
//
//   1. `list_groups($filter = tenant_id eq T AND type eq
//      USER_GROUP_RG_TYPE_CODE)` — drain all pages.
//   2. For each group: `remove_membership(group_id, USER_RG_TYPE_CODE,
//      user_id)`. `NotFound` is idempotent success.
//
// The cleanup short-circuits when the service is constructed without
// `with_rg_membership_cleanup`; the tests below wire a minimal fake
// RG client around `list_groups` + `remove_membership`.

mod cleanup {
    use super::*;
    use account_management_sdk::gts::{USER_GROUP_RG_TYPE_CODE, USER_RG_TYPE_CODE};
    use async_trait::async_trait;
    use modkit_odata::{ODataQuery, Page, PageInfo};
    use modkit_security::SecurityContext;
    use resource_group_sdk::{
        CreateGroupRequest, CreateTypeRequest, GroupHierarchy, ResourceGroup, ResourceGroupClient,
        ResourceGroupError, ResourceGroupMembership, ResourceGroupType, ResourceGroupWithDepth,
        UpdateGroupRequest, UpdateTypeRequest,
    };
    use std::sync::Mutex;

    use crate::domain::user::test_support::FakeUserOutcome;

    // -- minimal RG fake focused on list_groups + remove_membership --

    enum ListBehaviour {
        Pages(Vec<Page<ResourceGroup>>),
        Error(ResourceGroupError),
    }

    enum RemoveBehaviour {
        Ok,
        NotFound,
        Error(ResourceGroupError),
    }

    struct FakeMembershipRgClient {
        list_behaviour: Mutex<ListBehaviour>,
        remove_behaviour: RemoveBehaviour,
        removed: Mutex<Vec<(Uuid, String, String)>>,
        list_calls: Mutex<u32>,
    }

    impl FakeMembershipRgClient {
        /// One full page returning the given user-group rows.
        fn with_groups(rows: Vec<ResourceGroup>) -> Self {
            let page = Page {
                items: rows,
                page_info: PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: 100,
                },
            };
            Self {
                list_behaviour: Mutex::new(ListBehaviour::Pages(vec![page])),
                remove_behaviour: RemoveBehaviour::Ok,
                removed: Mutex::new(Vec::new()),
                list_calls: Mutex::new(0),
            }
        }

        /// Multiple pages chained via `next_cursor` tokens — exercises
        /// the cursor-advancement branch in `list_tenant_user_groups`.
        fn with_paged_groups(pages: Vec<Page<ResourceGroup>>) -> Self {
            Self {
                list_behaviour: Mutex::new(ListBehaviour::Pages(pages)),
                remove_behaviour: RemoveBehaviour::Ok,
                removed: Mutex::new(Vec::new()),
                list_calls: Mutex::new(0),
            }
        }

        fn empty() -> Self {
            Self::with_groups(Vec::new())
        }

        fn with_remove_behaviour(mut self, behaviour: RemoveBehaviour) -> Self {
            self.remove_behaviour = behaviour;
            self
        }

        fn with_list_error(error: ResourceGroupError) -> Self {
            Self {
                list_behaviour: Mutex::new(ListBehaviour::Error(error)),
                remove_behaviour: RemoveBehaviour::Ok,
                removed: Mutex::new(Vec::new()),
                list_calls: Mutex::new(0),
            }
        }

        fn removed_snapshot(&self) -> Vec<(Uuid, String, String)> {
            self.removed.lock().expect("lock").clone()
        }

        fn list_call_count(&self) -> u32 {
            *self.list_calls.lock().expect("lock")
        }
    }

    #[async_trait]
    impl ResourceGroupClient for FakeMembershipRgClient {
        async fn list_groups(
            &self,
            _ctx: &SecurityContext,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroup>, ResourceGroupError> {
            *self.list_calls.lock().expect("lock") += 1;
            let mut behaviour = self.list_behaviour.lock().expect("lock");
            match &mut *behaviour {
                ListBehaviour::Error(e) => Err(e.clone()),
                ListBehaviour::Pages(pages) => {
                    if pages.is_empty() {
                        return Ok(Page::empty(100));
                    }
                    Ok(pages.remove(0))
                }
            }
        }

        async fn remove_membership(
            &self,
            _ctx: &SecurityContext,
            group_id: Uuid,
            resource_type: &str,
            resource_id: &str,
        ) -> Result<(), ResourceGroupError> {
            self.removed.lock().expect("lock").push((
                group_id,
                resource_type.to_owned(),
                resource_id.to_owned(),
            ));
            match &self.remove_behaviour {
                RemoveBehaviour::Ok => Ok(()),
                RemoveBehaviour::NotFound => Err(ResourceGroupError::not_found("membership")),
                RemoveBehaviour::Error(e) => Err(e.clone()),
            }
        }

        // -- Unreachable: cleanup never calls these --

        async fn list_memberships(
            &self,
            _ctx: &SecurityContext,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroupMembership>, ResourceGroupError> {
            unreachable!(
                "cleanup uses list_groups(tenant) + per-group remove, never list_memberships"
            )
        }
        async fn create_type(
            &self,
            _ctx: &SecurityContext,
            _request: CreateTypeRequest,
        ) -> Result<ResourceGroupType, ResourceGroupError> {
            unreachable!()
        }
        async fn get_type(
            &self,
            _ctx: &SecurityContext,
            _code: &str,
        ) -> Result<ResourceGroupType, ResourceGroupError> {
            unreachable!()
        }
        async fn list_types(
            &self,
            _ctx: &SecurityContext,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroupType>, ResourceGroupError> {
            unreachable!()
        }
        async fn update_type(
            &self,
            _ctx: &SecurityContext,
            _code: &str,
            _request: UpdateTypeRequest,
        ) -> Result<ResourceGroupType, ResourceGroupError> {
            unreachable!()
        }
        async fn delete_type(
            &self,
            _ctx: &SecurityContext,
            _code: &str,
        ) -> Result<(), ResourceGroupError> {
            unreachable!()
        }
        async fn create_group(
            &self,
            _ctx: &SecurityContext,
            _request: CreateGroupRequest,
        ) -> Result<ResourceGroup, ResourceGroupError> {
            unreachable!()
        }
        async fn get_group(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<ResourceGroup, ResourceGroupError> {
            unreachable!()
        }
        async fn update_group(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
            _request: UpdateGroupRequest,
        ) -> Result<ResourceGroup, ResourceGroupError> {
            unreachable!()
        }
        async fn delete_group(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<(), ResourceGroupError> {
            unreachable!()
        }
        async fn get_group_descendants(
            &self,
            _ctx: &SecurityContext,
            _group_id: Uuid,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
            unreachable!()
        }
        async fn get_group_ancestors(
            &self,
            _ctx: &SecurityContext,
            _group_id: Uuid,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
            unreachable!()
        }
        async fn add_membership(
            &self,
            _ctx: &SecurityContext,
            _group_id: Uuid,
            _resource_type: &str,
            _resource_id: &str,
        ) -> Result<ResourceGroupMembership, ResourceGroupError> {
            unreachable!()
        }
    }

    fn make_service_with_cleanup(
        tenants: Arc<FakeTenantRepo>,
        idp: Arc<FakeIdpUserProvisioner>,
        rg: Arc<FakeMembershipRgClient>,
    ) -> UserService {
        // Register the seeded tenants' `tenant_type_uuid` schema so
        // `resolve_active_tenant` (which now treats `tenant_type` as
        // mandatory) succeeds for `seed_tenant` rows. Mirrors the
        // sibling `make_service` / `make_service_with_user_schema`
        // helpers above — without this, every cleanup test that goes
        // through `delete_user` fails with
        // `ServiceUnavailable { detail: "tenant_type resolution failed
        // ... GTS type-schema not found ..." }` before the RG-cleanup
        // path ever runs.
        let types_registry =
            Arc::new(MockTypesRegistryClient::new().with_type_schemas([test_tenant_type_schema()]));
        let rg: Arc<dyn ResourceGroupClient + Send + Sync> = rg;
        UserService::new(tenants, idp, types_registry, mock_enforcer())
            .with_rg_membership_cleanup(rg)
    }

    fn user_group(id: Uuid, tenant_id: Uuid) -> ResourceGroup {
        ResourceGroup {
            id,
            code: USER_GROUP_RG_TYPE_CODE.to_owned(),
            name: format!("ug-{id}"),
            hierarchy: GroupHierarchy {
                parent_id: None,
                tenant_id,
            },
            metadata: None,
        }
    }

    #[tokio::test]
    async fn cleanup_removes_membership_per_listed_group_on_removed_outcome() {
        let tenants = Arc::new(FakeTenantRepo::new());
        let tenant_id = Uuid::from_u128(0xC1);
        seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "t");
        let user_id = Uuid::from_u128(0xC2);
        let group_a = Uuid::from_u128(0xC3);
        let group_b = Uuid::from_u128(0xC4);

        let idp = Arc::new(FakeIdpUserProvisioner::new());
        idp.set_delete_outcome(FakeUserOutcome::Ok);
        let rg = Arc::new(FakeMembershipRgClient::with_groups(vec![
            user_group(group_a, tenant_id),
            user_group(group_b, tenant_id),
        ]));
        let svc = make_service_with_cleanup(tenants, idp, Arc::clone(&rg));

        svc.delete_user(&ctx(), tenant_id, user_id)
            .await
            .expect("deprovision succeeds");

        let removed = rg.removed_snapshot();
        assert_eq!(removed.len(), 2, "one remove_membership per listed group");
        let user_str = user_id.to_string();
        assert!(
            removed
                .iter()
                .all(|(_, t, r)| t == USER_RG_TYPE_CODE && r == &user_str),
            "every remove targets the deprovisioned user under USER_RG_TYPE_CODE"
        );
        let group_ids: std::collections::HashSet<Uuid> =
            removed.iter().map(|(g, _, _)| *g).collect();
        assert!(
            group_ids.contains(&group_a) && group_ids.contains(&group_b),
            "both listed groups received a remove call"
        );
    }

    #[tokio::test]
    async fn cleanup_with_no_user_groups_is_noop_success() {
        let tenants = Arc::new(FakeTenantRepo::new());
        let tenant_id = Uuid::from_u128(0xE1);
        seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "t");
        let user_id = Uuid::from_u128(0xE2);

        let idp = Arc::new(FakeIdpUserProvisioner::new());
        idp.set_delete_outcome(FakeUserOutcome::Ok);
        let rg = Arc::new(FakeMembershipRgClient::empty());
        let svc = make_service_with_cleanup(tenants, idp, Arc::clone(&rg));

        svc.delete_user(&ctx(), tenant_id, user_id)
            .await
            .expect("zero-groups deprovision succeeds");

        assert_eq!(rg.list_call_count(), 1, "one list_groups round-trip");
        assert!(
            rg.removed_snapshot().is_empty(),
            "no groups listed; nothing removed"
        );
    }

    #[tokio::test]
    async fn cleanup_remove_not_found_is_idempotent_success() {
        // The user wasn't a member of the listed group (or a peer
        // cleanup tick already removed the row). RG returns
        // `NotFound` on remove; we treat as success.
        let tenants = Arc::new(FakeTenantRepo::new());
        let tenant_id = Uuid::from_u128(0xF1);
        seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "t");
        let user_id = Uuid::from_u128(0xF2);
        let group_a = Uuid::from_u128(0xF3);

        let idp = Arc::new(FakeIdpUserProvisioner::new());
        idp.set_delete_outcome(FakeUserOutcome::Ok);
        let rg = Arc::new(
            FakeMembershipRgClient::with_groups(vec![user_group(group_a, tenant_id)])
                .with_remove_behaviour(RemoveBehaviour::NotFound),
        );
        let svc = make_service_with_cleanup(tenants, idp, Arc::clone(&rg));

        svc.delete_user(&ctx(), tenant_id, user_id)
            .await
            .expect("remove NotFound treated as idempotent success");
    }

    #[tokio::test]
    async fn cleanup_list_transport_error_surfaces_service_unavailable() {
        let tenants = Arc::new(FakeTenantRepo::new());
        let tenant_id = Uuid::from_u128(0xA1);
        seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "t");
        let user_id = Uuid::from_u128(0xA2);

        let idp = Arc::new(FakeIdpUserProvisioner::new());
        idp.set_delete_outcome(FakeUserOutcome::Ok);
        let rg = Arc::new(FakeMembershipRgClient::with_list_error(
            ResourceGroupError::ServiceUnavailable {
                message: "connection refused".to_owned(),
            },
        ));
        let svc = make_service_with_cleanup(tenants, idp, Arc::clone(&rg));

        let err = svc
            .delete_user(&ctx(), tenant_id, user_id)
            .await
            .expect_err("transport error must surface");
        match err {
            DomainError::ServiceUnavailable { .. } => {}
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cleanup_drains_multi_page_group_list() {
        // Exercises the cursor-advancement branch in
        // `list_tenant_user_groups`: page 1 returns one group + a
        // `next_cursor` token; the loop decodes the cursor and
        // fetches page 2 (one more group, terminal `next_cursor`).
        // Both groups receive a `remove_membership` call.
        use modkit_odata::{CursorV1, SortDir};

        let tenants = Arc::new(FakeTenantRepo::new());
        let tenant_id = Uuid::from_u128(0x1A1);
        seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "t");
        let user_id = Uuid::from_u128(0x1A2);
        let group_p1 = Uuid::from_u128(0x1A3);
        let group_p2 = Uuid::from_u128(0x1A4);

        let token = CursorV1 {
            k: vec!["id".to_owned()],
            o: SortDir::Asc,
            s: group_p1.to_string(),
            f: None,
            d: "fwd".to_owned(),
        }
        .encode()
        .expect("cursor encodes");

        let page1 = Page {
            items: vec![user_group(group_p1, tenant_id)],
            page_info: PageInfo {
                next_cursor: Some(token),
                prev_cursor: None,
                limit: 100,
            },
        };
        let page2 = Page {
            items: vec![user_group(group_p2, tenant_id)],
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 100,
            },
        };

        let idp = Arc::new(FakeIdpUserProvisioner::new());
        idp.set_delete_outcome(FakeUserOutcome::Ok);
        let rg = Arc::new(FakeMembershipRgClient::with_paged_groups(vec![
            page1, page2,
        ]));
        let svc = make_service_with_cleanup(tenants, idp, Arc::clone(&rg));

        svc.delete_user(&ctx(), tenant_id, user_id)
            .await
            .expect("multi-page deprovision succeeds");

        assert_eq!(
            rg.list_call_count(),
            2,
            "two list_groups round-trips for two pages"
        );
        let removed_groups: std::collections::HashSet<Uuid> =
            rg.removed_snapshot().iter().map(|(g, _, _)| *g).collect();
        assert!(
            removed_groups.contains(&group_p1) && removed_groups.contains(&group_p2),
            "both pages' groups received a remove call"
        );
    }

    #[tokio::test]
    async fn cleanup_remove_transport_error_surfaces_service_unavailable() {
        // Symmetric coverage for the per-row remove path. Listing
        // succeeds, the remove step fails with a transport error;
        // the deprovision call surfaces `ServiceUnavailable` so the
        // caller's retry path re-enters the flow.
        let tenants = Arc::new(FakeTenantRepo::new());
        let tenant_id = Uuid::from_u128(0xB1);
        seed_tenant(&tenants, tenant_id, None, TenantStatus::Active, "t");
        let user_id = Uuid::from_u128(0xB2);
        let group_a = Uuid::from_u128(0xB3);

        let idp = Arc::new(FakeIdpUserProvisioner::new());
        idp.set_delete_outcome(FakeUserOutcome::Ok);
        let rg = Arc::new(
            FakeMembershipRgClient::with_groups(vec![user_group(group_a, tenant_id)])
                .with_remove_behaviour(RemoveBehaviour::Error(
                    ResourceGroupError::ServiceUnavailable {
                        message: "connection refused".to_owned(),
                    },
                )),
        );
        let svc = make_service_with_cleanup(tenants, idp, Arc::clone(&rg));

        let err = svc
            .delete_user(&ctx(), tenant_id, user_id)
            .await
            .expect_err("remove transport error must surface");
        match err {
            DomainError::ServiceUnavailable { .. } => {}
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    }
}
