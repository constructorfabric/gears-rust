//! HTTP-level E2E tests for the
//! `/account-management/v1/tenants/{tenant_id}/service-accounts*` REST
//! surface.
//!
//! Scope: provision / list / rotate-secret / revoke through the real
//! router against the in-memory `FakeIdpPlugin` client registry. The
//! wire-level properties pinned here are the ones only an end-to-end
//! request can show: the status codes, the `Location` and
//! `Cache-Control: no-store` headers on the two credential-bearing
//! responses, the shape of the listing envelope, and that no response
//! body ever echoes provider error text. Service-side authorization,
//! input caps, and failure mapping are pinned by
//! `domain::service_account::service_tests`.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::doc_markdown
)]

mod common;

use axum::http::{StatusCode, header};
use tower::ServiceExt;
use uuid::Uuid;

use common::*;

fn build_sa_router(h: &Harness) -> axum::Router {
    let services = build_services_full(
        h,
        fake_idp(),
        empty_metadata_registry(),
        types_registry_for_users(),
    );
    build_test_router(&services)
}

fn collection_path(tenant: Uuid) -> String {
    format!("/account-management/v1/tenants/{tenant}/service-accounts")
}

/// Provision one account and return its `client_id`.
async fn provision(router: &axum::Router, tenant: Uuid, name: &str) -> String {
    let req = json_request(
        "POST",
        &collection_path(tenant),
        Some(serde_json::json!({"name": name})),
        ctx_for(tenant),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_body(resp).await;
    body["client_id"].as_str().expect("client_id").to_owned()
}

// ─── POST /tenants/{id}/service-accounts ───────────────────────────

#[tokio::test]
async fn provision_returns_201_with_credentials_location_and_no_store() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);

    let req = json_request(
        "POST",
        &collection_path(root),
        Some(serde_json::json!({"name": "ci", "scopes": ["platform.read"]})),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::CREATED);
    // The secret must not be cacheable by any intermediary.
    assert_eq!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    // A 201 identifies the created resource; the item URL answers
    // DELETE and prefixes the rotate-secret action.
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("Location header")
        .to_owned();
    let body = response_body(resp).await;
    let client_id = body["client_id"].as_str().expect("client_id");
    assert_eq!(location, format!("{}/{client_id}", collection_path(root)));
    assert_eq!(body["client_secret"], FAKE_CLIENT_SECRET);
    assert!(body["token_url"].is_string(), "token_url missing: {body}");
    assert!(body["subject_id"].is_string(), "subject_id missing: {body}");
}

/// A name already live in the tenant is a 400 — and critically, the
/// response must not resume or reveal the existing account.
#[tokio::test]
async fn provision_with_duplicate_name_returns_400_without_revealing_the_existing_account() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);
    let existing = provision(&router, root, "ci").await;

    let req = json_request(
        "POST",
        &collection_path(root),
        Some(serde_json::json!({"name": "ci"})),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = response_body(resp).await;
    let rendered = body.to_string();
    assert!(
        !rendered.contains(&existing),
        "the collision response must not reveal the existing account: {rendered}"
    );
    assert!(
        !rendered.contains(FAKE_CLIENT_SECRET),
        "no credential may appear in an error body: {rendered}"
    );
}

/// The provider's `detail` (and its field attribution) are untrusted
/// text: the envelope answers with AM's own fixed message instead. The
/// fake's collision detail quotes the submitted name, so seeing it
/// absent here is what proves the discard happened.
#[tokio::test]
async fn provision_rejection_does_not_echo_provider_detail() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);
    provision(&router, root, "ci").await;

    let req = json_request(
        "POST",
        &collection_path(root),
        Some(serde_json::json!({"name": "ci"})),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let body = response_body(resp).await;

    let rendered = body.to_string();
    assert!(
        !rendered.contains("already live in this tenant"),
        "provider detail leaked into the envelope: {rendered}"
    );
    assert!(
        rendered.contains("the provider rejected the request as invalid"),
        "expected AM's fixed invalid-input message: {rendered}"
    );
}

#[tokio::test]
async fn provision_rejects_unknown_body_fields() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);

    let req = json_request(
        "POST",
        &collection_path(root),
        Some(serde_json::json!({"name": "ci", "surprise": true})),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");

    // `deny_unknown_fields` on the DTO; axum's JSON extractor rejects
    // before the handler runs.
    assert!(
        resp.status().is_client_error(),
        "unknown field must be rejected, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn provision_on_missing_tenant_returns_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);

    let absent = Uuid::new_v4();
    let req = json_request(
        "POST",
        &collection_path(absent),
        Some(serde_json::json!({"name": "ci"})),
        ctx_for(absent),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ─── GET /tenants/{id}/service-accounts ────────────────────────────

#[tokio::test]
async fn list_returns_entries_with_verbatim_names_and_no_secrets() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);
    let ci = provision(&router, root, "ci").await;
    provision(&router, root, "backup").await;

    let req = json_request("GET", &collection_path(root), None, ctx_for(root));
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["service_accounts"]
        .as_array()
        .expect("service_accounts array");
    assert_eq!(items.len(), 2);
    // The verbatim `name` is the only contractual bridge from a
    // submitted name back to an opaque client id — it is what makes
    // ambiguous-create reconciliation possible.
    let ci_entry = items
        .iter()
        .find(|e| e["client_id"] == ci.as_str())
        .expect("provisioned entry present");
    assert_eq!(ci_entry["name"], "ci");
    assert_eq!(ci_entry["enabled"], true);
    assert!(
        !body.to_string().contains(FAKE_CLIENT_SECRET),
        "a listing must never carry secrets: {body}"
    );
}

#[tokio::test]
async fn list_on_tenant_without_accounts_returns_empty_collection() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);

    let req = json_request("GET", &collection_path(root), None, ctx_for(root));
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(
        body["service_accounts"].as_array().map(Vec::len),
        Some(0),
        "expected an empty collection, not a 404: {body}"
    );
}

/// The `(tenant_id, name)` uniqueness invariant is per tenant, not
/// global: two tenants may each run an account called `ci`, and each
/// listing shows only its own.
#[tokio::test]
async fn same_name_in_two_tenants_succeeds_and_each_listing_stays_tenant_scoped() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let router = build_sa_router(&h);

    let root_client = provision(&router, root, "ci").await;
    // The caller is authorized for `root`, whose subtree contains
    // `child`, so this second provision goes through the same grant.
    let child_client = provision(&router, child, "ci").await;
    assert_ne!(root_client, child_client);

    for (tenant, expected) in [(root, &root_client), (child, &child_client)] {
        let req = json_request("GET", &collection_path(tenant), None, ctx_for(root));
        let resp = router.clone().oneshot(req).await.expect("router");
        let body = response_body(resp).await;
        let items = body["service_accounts"]
            .as_array()
            .expect("service_accounts array");
        assert_eq!(items.len(), 1, "listing must be tenant-scoped: {body}");
        assert_eq!(items[0]["client_id"], expected.as_str());
        assert_eq!(items[0]["name"], "ci");
    }
}

// ─── adapters without service-account support ──────────────────────

/// An `IdP` adapter that implements only tenant provisioning — i.e. one
/// that never overrode the SDK's service-account defaults. Every
/// service-account call must then surface as `501`, never as a silent
/// success or a `503`.
struct TenantOnlyIdp;

#[async_trait::async_trait]
impl account_management_sdk::IdpPluginClient for TenantOnlyIdp {
    async fn provision_tenant(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _req: &account_management_sdk::IdpProvisionTenantRequest,
    ) -> Result<
        account_management_sdk::IdpProvisionResult,
        account_management_sdk::IdpProvisionFailure,
    > {
        Ok(account_management_sdk::IdpProvisionResult::new(None))
    }
}

#[tokio::test]
async fn adapter_without_service_account_support_returns_501() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services_full(
        &h,
        std::sync::Arc::new(TenantOnlyIdp),
        empty_metadata_registry(),
        types_registry_for_users(),
    );
    let router = build_test_router(&services);

    for (method, path, body) in [
        (
            "POST",
            collection_path(root),
            Some(serde_json::json!({"name": "ci"})),
        ),
        ("GET", collection_path(root), None),
        (
            "POST",
            format!("{}/svc-any/rotate-secret", collection_path(root)),
            None,
        ),
        ("DELETE", format!("{}/svc-any", collection_path(root)), None),
    ] {
        let req = json_request(method, &path, body, ctx_for(root));
        let resp = router.clone().oneshot(req).await.expect("router");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_IMPLEMENTED,
            "{method} {path} must surface as 501 on an adapter without support"
        );
    }
}

// ─── POST …/{client_id}/rotate-secret ────────────────────────────────

#[tokio::test]
async fn rotate_secret_returns_200_with_new_credentials_and_no_store() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);
    let client_id = provision(&router, root, "ci").await;

    let req = json_request(
        "POST",
        &format!("{}/{client_id}/rotate-secret", collection_path(root)),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let body = response_body(resp).await;
    assert_eq!(body["client_id"], client_id);
    assert_eq!(body["client_secret"], FAKE_CLIENT_SECRET);
}

/// Rotate does not fold absence into success, unlike revoke: it minted
/// no credential, so the caller must hear about it.
#[tokio::test]
async fn rotate_secret_on_absent_account_returns_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);

    let req = json_request(
        "POST",
        &format!("{}/svc-nope/rotate-secret", collection_path(root)),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// The `(tenant_id, client_id)` address is scoped: an account owned by
/// another tenant is not rotatable even given its exact client id.
#[tokio::test]
async fn rotate_secret_across_tenants_returns_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let router = build_sa_router(&h);
    let root_client = provision(&router, root, "ci").await;

    // Address the ROOT's account under the CHILD's path. The caller is
    // authorized for the child, so this is the address check speaking,
    // not the PEP gate.
    let req = json_request(
        "POST",
        &format!("{}/{root_client}/rotate-secret", collection_path(child)),
        None,
        ctx_for(child),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ─── DELETE …/{client_id} ────────────────────────────────────────────

#[tokio::test]
async fn revoke_returns_204_and_removes_the_account() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);
    let client_id = provision(&router, root, "ci").await;

    let req = json_request(
        "DELETE",
        &format!("{}/{client_id}", collection_path(root)),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let req = json_request("GET", &collection_path(root), None, ctx_for(root));
    let resp = router.oneshot(req).await.expect("router");
    let body = response_body(resp).await;
    assert_eq!(
        body["service_accounts"].as_array().map(Vec::len),
        Some(0),
        "revoked account must be gone: {body}"
    );
}

/// Idempotent by contract: the retry a client issues after a timeout
/// must not turn into a 404.
#[tokio::test]
async fn revoke_is_idempotent_on_a_repeated_delete() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);
    let client_id = provision(&router, root, "ci").await;
    let path = format!("{}/{client_id}", collection_path(root));

    for _ in 0..2 {
        let req = json_request("DELETE", &path, None, ctx_for(root));
        let resp = router.clone().oneshot(req).await.expect("router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn revoke_of_a_never_existing_account_returns_204() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);

    let req = json_request(
        "DELETE",
        &format!("{}/svc-never", collection_path(root)),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

/// After a revoke the name is free again — the uniqueness invariant is
/// over *live* accounts, which is what makes revoke + provision a
/// valid recovery from an ambiguous create.
#[tokio::test]
async fn revoke_frees_the_name_for_a_fresh_provision() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);
    let client_id = provision(&router, root, "ci").await;

    let req = json_request(
        "DELETE",
        &format!("{}/{client_id}", collection_path(root)),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Same name, fresh provision: must succeed.
    let _ = provision(&router, root, "ci").await;
}

// ─── item path shape ─────────────────────────────────────────────────

/// The item URL that `create` advertises in `Location` answers DELETE
/// but registers no GET — a documented, bounded seam (the plugin
/// contract exposes no by-id read; the collection listing covers reads).
#[tokio::test]
async fn item_path_registers_no_get() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);
    let client_id = provision(&router, root, "ci").await;

    let req = json_request(
        "GET",
        &format!("{}/{client_id}", collection_path(root)),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

// ─── per-verb permission independence ────────────────────────────────
//
// `dod-service-accounts-independent-permissions` is the whole reason this
// surface has four actions instead of a read/write/delete triad: minting a
// credential must be grantable separately from listing the inventory, and
// re-keying an existing account separately from creating new ones. Every
// other test in this file runs under `mock_enforcer`, which permits every
// action — so none of them can tell a per-verb gate from a blanket allow.
//
// These four probe it directly: grant exactly one action, then drive all
// four verbs and require that only the granted one gets through. Together
// they pin the claim in both directions — each verb consults its own
// permission, and no verb is reachable on another's grant.

/// One verb of the service-account surface, for the per-verb probes.
struct SaVerb {
    /// PEP action name — the wire vocabulary declared by
    /// `domain::service_account::service::pep::actions` (`pub(crate)`, so
    /// not nameable from an integration test); the catalog-vs-enforcement
    /// drift guard in `gts::permissions` keeps that list and this one
    /// honest.
    action: &'static str,
    method: &'static str,
    path: String,
    body: Option<serde_json::Value>,
    /// Status this verb returns when its own permission is granted.
    ok_status: StatusCode,
}

/// The four verbs, addressed against `root` and the fixture `client_id`.
fn sa_verbs(root: Uuid, client_id: &str) -> [SaVerb; 4] {
    [
        SaVerb {
            action: "create",
            method: "POST",
            path: collection_path(root),
            body: Some(serde_json::json!({"name": "minted-under-probe"})),
            ok_status: StatusCode::CREATED,
        },
        SaVerb {
            action: "list",
            method: "GET",
            path: collection_path(root),
            body: None,
            ok_status: StatusCode::OK,
        },
        SaVerb {
            action: "rotate_secret",
            method: "POST",
            path: format!("{}/{client_id}/rotate-secret", collection_path(root)),
            body: None,
            ok_status: StatusCode::OK,
        },
        SaVerb {
            action: "revoke",
            method: "DELETE",
            path: format!("{}/{client_id}", collection_path(root)),
            body: None,
            ok_status: StatusCode::NO_CONTENT,
        },
    ]
}

/// Grant exactly `granted`; assert that verb reaches its success status and
/// the other three are refused with 403.
///
/// The fixture account rotate/revoke address is provisioned through a
/// second, permissive router sharing the same `FakeIdpPlugin` registry —
/// the restricted router cannot mint it when `create` is not the granted
/// action. Only the granted verb mutates state, so the fixed iteration
/// order is safe: the PEP gate runs before any provider round-trip, so a
/// refused verb never reaches the registry either way.
async fn only_granted_verb_passes(granted: &str) {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;

    let idp = fake_idp();
    let permissive = build_test_router(&build_services_full(
        &h,
        std::sync::Arc::clone(&idp),
        empty_metadata_registry(),
        types_registry_for_users(),
    ));
    let client_id = provision(&permissive, root, "fixture").await;

    let restricted = build_test_router(&build_services_full_with_sa_enforcer(
        &h,
        std::sync::Arc::clone(&idp),
        empty_metadata_registry(),
        types_registry_for_users(),
        enforcer_allowing(&[granted]),
    ));

    for verb in sa_verbs(root, &client_id) {
        let SaVerb {
            action,
            method,
            path,
            body,
            ok_status,
        } = verb;
        let req = json_request(method, &path, body, ctx_for(root));
        let resp = restricted.clone().oneshot(req).await.expect("router");
        if action == granted {
            assert_eq!(
                resp.status(),
                ok_status,
                "{method} {path}: '{granted}' is granted, so the verb must succeed"
            );
        } else {
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "{method} {path}: only '{granted}' is granted, so '{action}' must be refused \
                 — a pass here means the verb rides another action's permission"
            );
        }
    }
}

// @cpt-begin:cpt-cf-account-management-dod-service-accounts-independent-permissions:p1:inst-dod-sa-independent-permissions-per-verb-probe
#[tokio::test]
async fn create_permission_alone_grants_only_create() {
    only_granted_verb_passes("create").await;
}

#[tokio::test]
async fn list_permission_alone_grants_only_list() {
    only_granted_verb_passes("list").await;
}

#[tokio::test]
async fn rotate_secret_permission_alone_grants_only_rotate_secret() {
    only_granted_verb_passes("rotate_secret").await;
}

#[tokio::test]
async fn revoke_permission_alone_grants_only_revoke() {
    only_granted_verb_passes("revoke").await;
}
// @cpt-end:cpt-cf-account-management-dod-service-accounts-independent-permissions:p1:inst-dod-sa-independent-permissions-per-verb-probe

// ─── Location header encoding ────────────────────────────────────────

/// `Location` embeds the adapter-chosen `client_id`. The contract declares
/// that id opaque, and AM bounds only the submitted `name`'s *length* —
/// charset is explicitly the adapter's to judge — so URL-reserved and
/// non-ASCII bytes reach the header unless the handler encodes them.
///
/// Concatenated raw, each of these is a distinct bug: `/` appends a second
/// path segment so the URL addresses a different resource, `?` and `#` push
/// the tail into a query or fragment, and a space, control byte, or
/// non-ASCII byte makes the header value itself invalid. Percent-encoding
/// the segment fixes all of them and is a no-op for the conventional
/// `svc-<tenant_id>-<name>` shape.
#[tokio::test]
async fn location_percent_encodes_an_opaque_client_id() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);

    // (submitted name, its percent-encoded form in the path segment)
    for (name, encoded) in [
        ("a/b", "a%2Fb"),
        ("a?b", "a%3Fb"),
        ("a#b", "a%23b"),
        ("a b", "a%20b"),
        ("a\u{7f}b", "a%7Fb"),
        // `caf\u{e9}` — escaped rather than literal, per `non_ascii_literal`.
        ("caf\u{e9}", "caf%C3%A9"),
    ] {
        let req = json_request(
            "POST",
            &collection_path(root),
            Some(serde_json::json!({ "name": name })),
            ctx_for(root),
        );
        let resp = router.clone().oneshot(req).await.expect("router");

        // A raw control byte or space would have made the header value
        // invalid, which surfaces as a 500 rather than a 201.
        assert_eq!(
            resp.status(),
            StatusCode::CREATED,
            "name {name:?}: an opaque client_id must not break the response"
        );

        let location = resp
            .headers()
            .get(header::LOCATION)
            .unwrap_or_else(|| panic!("name {name:?}: Location must be present"))
            .to_str()
            .unwrap_or_else(|e| panic!("name {name:?}: Location must be a valid header: {e}"));

        assert_eq!(
            location,
            format!("{}/svc-{root}-{encoded}", collection_path(root)),
            "name {name:?}: client_id must be percent-encoded into the path segment"
        );

        // The encoded id must occupy exactly one segment: the `/` case
        // would otherwise silently address a different resource.
        assert_eq!(
            location.matches('/').count(),
            collection_path(root).matches('/').count() + 1,
            "name {name:?}: Location must gain exactly one path segment"
        );
    }
}

/// The conventional client-id shape is unreserved throughout, so encoding
/// must leave it byte-identical — the fix must not start mangling the
/// normal case.
#[tokio::test]
async fn location_leaves_a_conventional_client_id_unchanged() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_sa_router(&h);

    let req = json_request(
        "POST",
        &collection_path(root),
        Some(serde_json::json!({"name": "ci-runner-01"})),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");

    assert_eq!(resp.status(), StatusCode::CREATED);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .expect("Location")
        .to_str()
        .expect("valid header");
    assert_eq!(
        location,
        format!("{}/svc-{root}-ci-runner-01", collection_path(root)),
        "an unreserved client_id must pass through verbatim"
    );
    assert!(!location.contains('%'), "nothing to encode: {location}");
}
