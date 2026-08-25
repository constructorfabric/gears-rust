//! Unit tests for [`ServiceAccountService`].
//!
//! Every test wires the service against [`FakeTenantRepo`] +
//! [`FakeServiceAccountProvider`]. Pins:
//!
//! * Guard ordering: the PEP gate and the tenant existence + `Active`
//!   precondition both run BEFORE any provider call, so a denied caller
//!   or a non-existent / non-`Active` tenant surfaces without the
//!   provider being touched (`*_call_count == 0`).
//! * Object-level scoping: a caller whose compiled scope does not cover
//!   the target tenant gets `NotFound` — the tenant is invisible, not
//!   merely forbidden.
//! * Error mapping: each `IdpServiceAccountFailure` variant lands on
//!   its documented `DomainError`, with `Ambiguous` as a distinct 409
//!   rather than being collapsed into the 503 clean-failure arm.
//! * The untrusted-text posture: a credential-shaped adapter `detail`
//!   (and an adapter-attributed `field`) reaches neither the mapped
//!   error nor any log record — only the fixed AM-owned messages and
//!   safe metadata do.
//! * Revoke idempotency: a provider-reported absent account is
//!   success; rotate against the same absence is a 404 carrying the
//!   `client_id`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::sync::Arc;

use account_management_sdk::IdpServiceAccountSummary;
use secrecy::ExposeSecret as _;
use serde_json::json;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::MockTypesRegistryClient;
use types_registry_sdk::{GtsTypeId, GtsTypeSchema};
use uuid::Uuid;

use crate::domain::error::{DomainError, UnsupportedResource};
use crate::domain::idp::{
    SA_AMBIGUOUS_MESSAGE, SA_INVALID_INPUT_MESSAGE, SA_UNSUPPORTED_MESSAGE, SA_UPSTREAM_MESSAGE,
};
use crate::domain::service_account::service::ServiceAccountService;
use crate::domain::service_account::test_support::idp::{ADAPTER_DETAIL, FAKE_SECRET};
use crate::domain::service_account::test_support::{
    FakeServiceAccountOutcome, FakeServiceAccountProvider,
};
use crate::domain::tenant::model::{TenantModel, TenantStatus};
use crate::domain::tenant::test_support::{FakeTenantRepo, deny_all_enforcer, mock_enforcer};

use super::service::MAX_NAME_CHARS;

/// Canonical chained `tenant_type` every seeded tenant carries; the
/// matching schema is pre-registered so the shared tenant guard can
/// resolve the typed `GtsTypeId`.
const TEST_TENANT_TYPE_ID: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~");

fn test_tenant_type_uuid() -> Uuid {
    gts::GtsId::try_new(TEST_TENANT_TYPE_ID)
        .expect("hardcoded chain is valid")
        .to_uuid()
}

fn test_tenant_type_schema() -> GtsTypeSchema {
    fn build(type_id: &str) -> GtsTypeSchema {
        let parent = GtsTypeSchema::derive_parent_type_id(type_id)
            .map(|p| std::sync::Arc::new(build(p.as_ref())));
        GtsTypeSchema::try_new(GtsTypeId::new(type_id), json!({}), None, parent)
            .expect("synthetic tenant_type schema is valid")
    }
    build(TEST_TENANT_TYPE_ID)
}

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("epoch")
}

/// Subject tenant the `mock_enforcer`'s `InTenantSubtree` predicate is
/// rooted at; `seed_tenant` materialises the closure rows that make
/// seeded tenants visible under the compiled scope.
const fn ctx_subject_root() -> Uuid {
    Uuid::from_u128(0xCAFE_BABE)
}

fn ctx() -> SecurityContext {
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

fn make_service(
    tenants: Arc<FakeTenantRepo>,
    idp: Arc<FakeServiceAccountProvider>,
) -> ServiceAccountService {
    let types_registry =
        Arc::new(MockTypesRegistryClient::new().with_type_schemas([test_tenant_type_schema()]));
    ServiceAccountService::new(tenants, idp, types_registry, mock_enforcer())
}

/// Same wiring, but with a PDP that denies every decision — pins that
/// the gate runs before the tenant read and the provider call.
fn make_denied_service(
    tenants: Arc<FakeTenantRepo>,
    idp: Arc<FakeServiceAccountProvider>,
) -> ServiceAccountService {
    let types_registry =
        Arc::new(MockTypesRegistryClient::new().with_type_schemas([test_tenant_type_schema()]));
    ServiceAccountService::new(tenants, idp, types_registry, deny_all_enforcer())
}

fn seed_tenant(fake: &FakeTenantRepo, id: Uuid, status: TenantStatus, name: &str) {
    let now = fixed_now();
    fake.insert_tenant_raw(TenantModel {
        id,
        parent_id: None,
        name: name.to_owned(),
        status,
        self_managed: false,
        tenant_type_uuid: test_tenant_type_uuid(),
        depth: 0,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    });
    let subject_root = ctx_subject_root();
    fake.seed_closure(subject_root, id, 0, status);
    if subject_root != id {
        fake.seed_closure(id, id, 0, status);
    }
}

/// Seed one active tenant and return `(service, fake_provider, tenant_id)`.
fn active_tenant_fixture() -> (ServiceAccountService, Arc<FakeServiceAccountProvider>, Uuid) {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x1);
    seed_tenant(&tenants, tenant_id, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeServiceAccountProvider::new());
    let svc = make_service(tenants, Arc::clone(&idp));
    (svc, idp, tenant_id)
}

fn summary(client_id: &str, name: &str) -> IdpServiceAccountSummary {
    IdpServiceAccountSummary::new(
        client_id.to_owned(),
        name.to_owned(),
        true,
        vec!["platform.read".to_owned()],
    )
}

// ---- create ---------------------------------------------------------

#[tokio::test]
async fn create_happy_path_returns_credentials_and_forwards_inputs() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    let creds = svc
        .create(
            &ctx(),
            tenant_id,
            "ci".to_owned(),
            vec!["platform.read".to_owned()],
        )
        .await
        .expect("create succeeds");

    assert_eq!(creds.client_secret.expose_secret(), FAKE_SECRET);
    assert!(creds.client_id.ends_with("-ci"));
    assert_eq!(
        idp.create_calls_snapshot(),
        vec![(tenant_id, "ci".to_owned())]
    );
    assert_eq!(
        idp.create_scopes_snapshot(),
        vec![vec!["platform.read".to_owned()]]
    );
}

#[tokio::test]
async fn create_forwards_plugin_private_tenant_metadata() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x2);
    seed_tenant(&tenants, tenant_id, TenantStatus::Active, "acme");
    // Whatever the plugin stashed at provision_tenant time must be
    // replayed verbatim on every subsequent call, machine identities
    // included — adapters key vendor-side realm/org routing off it.
    let blob = json!({"realm": "acme-realm"});
    tenants.seed_idp_metadata(tenant_id, Some(blob.clone()));
    let idp = Arc::new(FakeServiceAccountProvider::new());
    let svc = make_service(tenants, Arc::clone(&idp));

    svc.create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect("create succeeds");

    assert_eq!(idp.create_metadata_snapshots(), vec![Some(blob)]);
}

#[tokio::test]
async fn create_trims_the_name_before_forwarding() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    svc.create(&ctx(), tenant_id, "  ci  ".to_owned(), vec![])
        .await
        .expect("create succeeds");

    assert_eq!(
        idp.create_calls_snapshot(),
        vec![(tenant_id, "ci".to_owned())]
    );
}

#[tokio::test]
async fn create_rejects_whitespace_only_name_without_calling_provider() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    let err = svc
        .create(&ctx(), tenant_id, "   ".to_owned(), vec![])
        .await
        .expect_err("all-whitespace name is rejected");

    assert!(matches!(err, DomainError::Validation { .. }));
    assert_eq!(idp.create_call_count(), 0);
}

#[tokio::test]
async fn create_rejects_oversized_name_without_calling_provider() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    let err = svc
        .create(&ctx(), tenant_id, "n".repeat(65), vec![])
        .await
        .expect_err("oversized name is rejected");

    assert!(matches!(err, DomainError::Validation { .. }));
    assert_eq!(idp.create_call_count(), 0);
}

#[tokio::test]
async fn create_rejects_oversized_scope_list_without_calling_provider() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    let too_many: Vec<String> = (0..33).map(|i| format!("scope.{i}")).collect();
    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), too_many)
        .await
        .expect_err("33 scopes exceed the cap");
    assert!(matches!(err, DomainError::Validation { .. }));

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec!["s".repeat(257)])
        .await
        .expect_err("oversized scope is rejected");
    assert!(matches!(err, DomainError::Validation { .. }));

    assert_eq!(idp.create_call_count(), 0);
}

#[tokio::test]
async fn create_on_missing_tenant_is_not_found_without_calling_provider() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeServiceAccountProvider::new());
    let svc = make_service(tenants, Arc::clone(&idp));

    let err = svc
        .create(&ctx(), Uuid::from_u128(0xDEAD), "ci".to_owned(), vec![])
        .await
        .expect_err("absent tenant is rejected");

    assert!(matches!(err, DomainError::NotFound { .. }));
    assert_eq!(idp.create_call_count(), 0);
}

#[tokio::test]
async fn create_on_suspended_tenant_is_validation_without_calling_provider() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x3);
    seed_tenant(&tenants, tenant_id, TenantStatus::Suspended, "acme");
    let idp = Arc::new(FakeServiceAccountProvider::new());
    let svc = make_service(tenants, Arc::clone(&idp));

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("non-Active tenant is rejected");

    assert!(matches!(err, DomainError::Validation { .. }));
    assert_eq!(idp.create_call_count(), 0);
}

#[tokio::test]
async fn tenant_type_resolution_failure_uses_fixed_public_detail() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x31);
    seed_tenant(&tenants, tenant_id, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeServiceAccountProvider::new());
    // An empty registry returns a canonical NotFound carrying the requested
    // UUID. The shared guard must retain that text only in its operator log.
    let types_registry = Arc::new(MockTypesRegistryClient::new());
    let idp_client: Arc<dyn account_management_sdk::IdpPluginClient> = idp.clone();
    let svc = ServiceAccountService::new(tenants, idp_client, types_registry, mock_enforcer());

    let domain_err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("missing tenant type is unavailable");
    let canonical = CanonicalError::from(domain_err);

    assert_eq!(
        canonical.detail(),
        "tenant type resolution is temporarily unavailable"
    );
    assert!(
        !canonical
            .detail()
            .contains(&test_tenant_type_uuid().to_string())
    );
    assert_eq!(idp.create_call_count(), 0);
}

#[tokio::test]
async fn create_denied_by_pdp_never_reaches_the_provider() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x4);
    seed_tenant(&tenants, tenant_id, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeServiceAccountProvider::new());
    let svc = make_denied_service(tenants, Arc::clone(&idp));

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("PDP denies");

    assert!(matches!(err, DomainError::CrossTenantDenied { .. }));
    assert_eq!(idp.create_call_count(), 0);
}

/// A grant covering a *different* subtree must not authorize this
/// target. The tenant read runs under the compiled scope, so the target
/// is invisible (404) rather than merely forbidden — which is also what
/// keeps tenant topology from leaking through a 403.
#[tokio::test]
async fn create_outside_the_callers_subtree_is_not_found() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    let err = svc
        .create(
            &ctx_for(Uuid::from_u128(0xFEED)),
            tenant_id,
            "ci".to_owned(),
            vec![],
        )
        .await
        .expect_err("out-of-subtree target is invisible");

    assert!(matches!(err, DomainError::NotFound { .. }));
    assert_eq!(idp.create_call_count(), 0);
}

/// Tenant visibility is checked before caller-input caps. Otherwise an
/// oversized name would let an unauthorized caller distinguish an existing
/// out-of-subtree tenant from an absent one by error shape.
#[tokio::test]
async fn create_outside_subtree_with_oversized_name_is_still_not_found() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    let err = svc
        .create(
            &ctx_for(Uuid::from_u128(0xFEED)),
            tenant_id,
            "x".repeat(MAX_NAME_CHARS + 1),
            vec![],
        )
        .await
        .expect_err("tenant visibility must win over input validation");

    assert!(matches!(err, DomainError::NotFound { .. }));
    assert_eq!(idp.create_call_count(), 0);
}

// ---- provider failure mapping ---------------------------------------

/// Every 404 this surface emits MUST name what was addressed. The shared
/// mapping used to hardcode `resource: String::new()`, and only `rotate`
/// pre-empted it — so every other path produced an AIP-193 envelope whose
/// `resource_name` was blank. On `create` the address is the submitted
/// `name`: no `client_id` exists yet, and `name` is the correlation key the
/// ambiguous-create recovery searches on.
#[tokio::test]
async fn create_not_found_names_the_submitted_name_as_the_resource() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_create_outcome(FakeServiceAccountOutcome::NotFound);

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("provider reports not-found");

    let DomainError::ServiceAccountNotFound { resource, detail } = err else {
        panic!("expected ServiceAccountNotFound");
    };
    assert_eq!(
        resource, "ci",
        "the 404 envelope must name the addressed account"
    );
    assert!(
        !detail.contains(ADAPTER_DETAIL),
        "adapter text must not leak"
    );
}

/// `revoke` folds `NotFound` into success, so its 404s come from other
/// variants — but when one is emitted it must carry the `client_id` the
/// caller addressed, not an empty string.
#[tokio::test]
async fn revoke_failure_names_the_addressed_client_id_as_the_resource() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_revoke_outcome(FakeServiceAccountOutcome::Ambiguous);

    let err = svc
        .revoke(&ctx(), tenant_id, "svc-ci")
        .await
        .expect_err("an ambiguous revoke is not absence-equivalent");

    assert!(
        matches!(err, DomainError::ServiceAccountAmbiguous { .. }),
        "expected ServiceAccountAmbiguous, got {err:?}"
    );
}

#[tokio::test]
async fn create_maps_invalid_input_to_fixed_message_and_drops_adapter_text() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_create_outcome(FakeServiceAccountOutcome::InvalidInput);

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("provider rejects");

    let DomainError::ServiceAccountInvalidInput { detail } = err else {
        panic!("expected ServiceAccountInvalidInput");
    };
    assert_eq!(detail, SA_INVALID_INPUT_MESSAGE);
    assert!(!detail.contains(ADAPTER_DETAIL));
}

#[tokio::test]
async fn create_maps_clean_failure_to_idp_unavailable() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_create_outcome(FakeServiceAccountOutcome::CleanFailure);

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("provider fails cleanly");

    let DomainError::IdpUnavailable { detail } = err else {
        panic!("expected IdpUnavailable");
    };
    assert_eq!(detail, SA_UPSTREAM_MESSAGE);
}

/// The ambiguous outcome is the one that must NOT collapse into the
/// clean-failure arm: a half-applied provision makes "retry the same
/// request" wrong, so it carries its own variant (409) and a message
/// that steers the caller into reconciliation.
#[tokio::test]
async fn create_maps_ambiguous_to_its_own_variant_steering_reconciliation() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_create_outcome(FakeServiceAccountOutcome::Ambiguous);

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("provider is uncertain");

    let DomainError::ServiceAccountAmbiguous { detail } = err else {
        panic!("expected ServiceAccountAmbiguous");
    };
    assert_eq!(detail, SA_AMBIGUOUS_MESSAGE);
    assert!(detail.contains("reconcile"));
}

#[tokio::test]
async fn create_maps_unsupported_to_unsupported_operation() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_create_outcome(FakeServiceAccountOutcome::Unsupported);

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("adapter ships no service-account support");

    let DomainError::UnsupportedOperation { detail, resource } = err else {
        panic!("expected UnsupportedOperation");
    };
    assert_eq!(detail, SA_UNSUPPORTED_MESSAGE);
    // A 501 from the machine-identity half must not claim to be about a
    // tenant: `resource_type` is what a client keys the distinction off.
    assert_eq!(resource, UnsupportedResource::ServiceAccount);
}

/// A credential-shaped adapter `detail` is pure ASCII graphic text, so
/// no character filter could distinguish it from operator prose. The
/// only safe answer is to drop it entirely — from the response AND from
/// the log record, which is what separates this surface from the
/// digest-and-forward posture on the tenant / user halves.
#[tokio::test]
#[tracing_test::traced_test]
async fn adapter_detail_reaches_neither_the_error_nor_the_logs() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_create_outcome(FakeServiceAccountOutcome::CleanFailure);

    let err = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("provider fails cleanly");

    assert!(!err.to_string().contains(ADAPTER_DETAIL));
    assert!(
        !logs_contain(ADAPTER_DETAIL),
        "the adapter detail must not reach the log record either"
    );
    assert!(
        logs_contain("failure=\"clean_failure\"") && logs_contain("detail_len="),
        "the log record must still carry the safe metadata"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn adapter_attributed_field_is_not_logged_either() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_create_outcome(FakeServiceAccountOutcome::InvalidInput);

    let _ = svc
        .create(&ctx(), tenant_id, "ci".to_owned(), vec![])
        .await
        .expect_err("provider rejects");

    assert!(
        !logs_contain(ADAPTER_DETAIL),
        "neither the detail nor the adapter-attributed field may be logged"
    );
    assert!(
        logs_contain("field_present=true") && logs_contain("failure=\"invalid_input\""),
        "the log record must still say a field was attributed, without its value"
    );
}

// ---- list -----------------------------------------------------------

#[tokio::test]
async fn list_returns_the_tenants_accounts() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_list_items(vec![summary("svc-1", "ci"), summary("svc-2", "backup")]);

    let items = svc.list(&ctx(), tenant_id).await.expect("list succeeds");

    assert_eq!(items.len(), 2);
    // The verbatim `name` is the correlation key an ambiguous-create
    // recovery searches on, so it must survive the pass-through.
    assert_eq!(items[0].name, "ci");
    assert_eq!(items[1].name, "backup");
    assert_eq!(idp.list_call_count(), 1);
}

#[tokio::test]
async fn list_on_missing_tenant_is_not_found_without_calling_provider() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let idp = Arc::new(FakeServiceAccountProvider::new());
    let svc = make_service(tenants, Arc::clone(&idp));

    let err = svc
        .list(&ctx(), Uuid::from_u128(0xDEAD))
        .await
        .expect_err("absent tenant is rejected");

    assert!(matches!(err, DomainError::NotFound { .. }));
    assert_eq!(idp.list_call_count(), 0);
}

/// A listing retains nothing, so an uncertain provider outcome is a CLEAN
/// failure to the caller — 503 "retry the read", not the 409
/// `AMBIGUOUS_OUTCOME` the shared mapping produces for the mutating
/// operations. 409 would be actively misleading here: it instructs the
/// caller to reconcile by listing, which is the call that just failed.
#[tokio::test]
#[tracing_test::traced_test]
async fn list_maps_a_provider_ambiguous_to_unavailable_not_the_409_reconcile_steer() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_list_outcome(FakeServiceAccountOutcome::Ambiguous);

    let err = svc
        .list(&ctx(), tenant_id)
        .await
        .expect_err("an ambiguous listing must surface as a failure");

    assert!(
        matches!(err, DomainError::IdpUnavailable { .. }),
        "expected IdpUnavailable (503), got {err:?}"
    );
    assert!(
        !matches!(err, DomainError::ServiceAccountAmbiguous { .. }),
        "list must never steer the caller to reconcile a read"
    );
    assert!(
        !err.to_string().contains(ADAPTER_DETAIL) && !logs_contain(ADAPTER_DETAIL),
        "the list-specific remap must discard adapter detail from error and logs"
    );
    assert!(
        logs_contain("failure=\"ambiguous\"") && logs_contain("detail_len="),
        "the list-specific remap must retain safe failure metadata"
    );
}

// ---- rotate_secret --------------------------------------------------

#[tokio::test]
async fn rotate_secret_returns_new_credentials_for_the_addressed_account() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    let creds = svc
        .rotate_secret(&ctx(), tenant_id, "svc-abc")
        .await
        .expect("rotate succeeds");

    assert_eq!(creds.client_id, "svc-abc");
    assert_eq!(creds.client_secret.expose_secret(), FAKE_SECRET);
    assert_eq!(
        idp.rotate_calls_snapshot(),
        vec![(tenant_id, "svc-abc".to_owned())]
    );
}

/// Rotate does NOT fold absence into success (unlike revoke): a rotate
/// that found nothing minted no usable credential. The 404 carries the
/// `client_id` — the caller's own path input — as the resource.
#[tokio::test]
async fn rotate_secret_on_absent_account_is_not_found_with_the_client_id() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_rotate_outcome(FakeServiceAccountOutcome::NotFound);

    let err = svc
        .rotate_secret(&ctx(), tenant_id, "svc-gone")
        .await
        .expect_err("absent account is a 404");

    let DomainError::ServiceAccountNotFound { resource, detail } = err else {
        panic!("expected ServiceAccountNotFound");
    };
    assert_eq!(resource, "svc-gone");
    assert!(!detail.contains(ADAPTER_DETAIL));
}

// ---- revoke ---------------------------------------------------------

#[tokio::test]
async fn revoke_deletes_the_account() {
    let (svc, idp, tenant_id) = active_tenant_fixture();

    svc.revoke(&ctx(), tenant_id, "svc-abc")
        .await
        .expect("revoke succeeds");

    assert_eq!(
        idp.revoke_calls_snapshot(),
        vec![(tenant_id, "svc-abc".to_owned())]
    );
}

/// Idempotency by error-mapping: the adapter reports vendor "client does
/// not exist" as `NotFound`, and AM decides that means success, so a
/// repeated DELETE stays safe to retry.
#[tokio::test]
async fn revoke_on_absent_account_is_success() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_revoke_outcome(FakeServiceAccountOutcome::NotFound);

    svc.revoke(&ctx(), tenant_id, "svc-gone")
        .await
        .expect("absent account is success-equivalent");
}

#[tokio::test]
async fn revoke_still_surfaces_non_absence_failures() {
    let (svc, idp, tenant_id) = active_tenant_fixture();
    idp.set_revoke_outcome(FakeServiceAccountOutcome::CleanFailure);

    let err = svc
        .revoke(&ctx(), tenant_id, "svc-abc")
        .await
        .expect_err("a clean failure is not idempotent success");

    assert!(matches!(err, DomainError::IdpUnavailable { .. }));
}

#[tokio::test]
async fn revoke_denied_by_pdp_never_reaches_the_provider() {
    let tenants = Arc::new(FakeTenantRepo::new());
    let tenant_id = Uuid::from_u128(0x5);
    seed_tenant(&tenants, tenant_id, TenantStatus::Active, "acme");
    let idp = Arc::new(FakeServiceAccountProvider::new());
    let svc = make_denied_service(tenants, Arc::clone(&idp));

    let err = svc
        .revoke(&ctx(), tenant_id, "svc-abc")
        .await
        .expect_err("PDP denies");

    assert!(matches!(err, DomainError::CrossTenantDenied { .. }));
    assert_eq!(idp.revoke_call_count(), 0);
}
