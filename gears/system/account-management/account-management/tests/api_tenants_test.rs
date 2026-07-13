//! HTTP-level E2E tests for the `/account-management/v1/tenants*` REST
//! surface (excluding the `/children` sub-resource — covered in
//! `api_children_test.rs`).
//!
//! Scope: route matching, status codes, response envelopes, and the
//! handler-layer composition (`Location` header, post-write
//! projections, immutable-field rejection). Service-side semantics
//! (closure-table maintenance, hierarchy depth gating, soft-delete
//! cascade) are pinned by `lifecycle_integration.rs`.

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
use toolkit_gts::gts_id;
use tower::ServiceExt;
use uuid::Uuid;

use common::*;

const SAMPLE_TENANT_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~");

// ─── POST /tenants ───────────────────────────────────────────────────

#[tokio::test]
async fn create_tenant_returns_201_with_location_header() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let body = serde_json::json!({
        "name": "acme",
        "parent_id": root.to_string(),
        "tenant_type": SAMPLE_TENANT_TYPE,
    });
    let req = json_request(
        "POST",
        "/account-management/v1/tenants",
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .expect("Location header on 201")
        .to_str()
        .expect("ASCII")
        .to_owned();
    assert!(
        location.starts_with("/account-management/v1/tenants/"),
        "Location must point at GET /tenants/{{id}}, got {location}",
    );
    let body = response_body(resp).await;
    assert_eq!(body["name"], "acme");
    assert_eq!(body["status"], "active");
    assert_eq!(
        body["parent_id"],
        serde_json::Value::String(root.to_string())
    );
    assert!(
        body["id"].is_string(),
        "response must carry the server-allocated id"
    );
}

#[tokio::test]
async fn create_tenant_missing_parent_returns_400() {
    // Validation-style 400: `tenant_type` valid, parent missing. AM
    // routes `parent tenant not found` through the `validation` arm
    // (see service-level `create_tenant`).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let missing_parent = Uuid::new_v4();
    let body = serde_json::json!({
        "name": "acme",
        "parent_id": missing_parent.to_string(),
        "tenant_type": SAMPLE_TENANT_TYPE,
    });
    let req = json_request(
        "POST",
        "/account-management/v1/tenants",
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.is_object(), "envelope must be an object: {body}");
}

#[tokio::test]
async fn create_tenant_with_unknown_field_returns_validation_error() {
    // `TenantCreateRequestDto` declares `#[serde(deny_unknown_fields)]`
    // so a stray JSON member surfaces as a wire-layer rejection rather
    // than being silently dropped (the AM `child_id` collision risk).
    // axum's `Json` extractor reports serde failures as HTTP 422
    // Unprocessable Entity (the toolkit canonical wire convention).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let body = serde_json::json!({
        "name": "acme",
        "parent_id": root.to_string(),
        "tenant_type": SAMPLE_TENANT_TYPE,
        "child_id": Uuid::new_v4().to_string(),
    });
    let req = json_request(
        "POST",
        "/account-management/v1/tenants",
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    // 400 (custom toolkit validation) OR 422 (axum Json serde wire
    // rejection) — both are acceptable on `deny_unknown_fields`.
    assert!(
        matches!(
            resp.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY,
        ),
        "expected 400 or 422 on unknown field, got {}",
        resp.status(),
    );
}

// ─── GET /tenants/{id} ───────────────────────────────────────────────

#[tokio::test]
async fn get_tenant_malformed_uuid_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        "/account-management/v1/tenants/not-a-uuid",
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    // Axum's `Path<Uuid>` extractor rejects malformed values with 400.
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_tenant_not_found_returns_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let unknown = Uuid::new_v4();
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{unknown}"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_tenant_returns_200_with_dto() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["id"], root.to_string());
    assert_eq!(body["status"], "active");
    assert_eq!(body["self_managed"], false);
    assert_eq!(body["depth"], 0);
    assert!(body["created_at"].is_string());
    assert!(body["updated_at"].is_string());
}

// ─── PATCH /tenants/{id} ─────────────────────────────────────────────

#[tokio::test]
async fn update_tenant_status_payload_is_rejected_at_the_wire() {
    // Regression guard for the silent half-apply pre-fix: the PATCH
    // DTO used to carry `status: Option<TenantPatchStatusDto>` and
    // lower it to a no-op, so a mixed `{"name":_, "status":_}` PATCH
    // would 200 with the rename applied and the status silently
    // dropped. The fix removed the field; `#[serde(deny_unknown_fields)]`
    // now turns any `status` payload into a wire-layer rejection.
    // Lifecycle transitions go through `/suspend`, `/unsuspend` and
    // `DELETE`. axum's `Json` extractor surfaces serde failures as
    // 400 or 422 depending on the failure shape; accept either.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    for body in [
        serde_json::json!({ "status": "suspended" }),
        serde_json::json!({ "name": "renamed", "status": "suspended" }),
    ] {
        let req = json_request(
            "PATCH",
            &format!("/account-management/v1/tenants/{child}"),
            Some(body.clone()),
            ctx_for(root),
        );
        let resp = router.clone().oneshot(req).await.expect("router");
        assert!(
            matches!(
                resp.status(),
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY,
            ),
            "PATCH body `{body}` MUST be rejected; got {}",
            resp.status(),
        );
    }
}

#[tokio::test]
async fn update_tenant_empty_body_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let body = serde_json::json!({});
    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{child}"),
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_tenant_with_immutable_field_is_rejected() {
    // `TenantUpdateRequestDto` declares `#[serde(deny_unknown_fields)]`
    // so a PATCH that tries to mutate `parent_id` surfaces as a wire-
    // layer rejection rather than being silently dropped. axum's
    // `Json` extractor reports the serde failure as HTTP 422.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let body = serde_json::json!({ "parent_id": Uuid::new_v4().to_string() });
    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{child}"),
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert!(
        matches!(
            resp.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY,
        ),
        "expected 400 or 422 on PATCH with unknown field, got {}",
        resp.status(),
    );
}

// ─── DELETE /tenants/{id} ────────────────────────────────────────────

#[tokio::test]
async fn delete_tenant_returns_204_and_persists_soft_delete() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "DELETE",
        &format!("/account-management/v1/tenants/{child}"),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    assert!(
        bytes.is_empty(),
        "204 response MUST carry an empty body, got {} bytes",
        bytes.len(),
    );

    // The soft-delete projection is still observable via the GET
    // surface — re-read to pin that the DELETE actually flipped the
    // status and armed `deleted_at`.
    let get = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{child}"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["status"], "deleted");
    assert_eq!(body["id"], child.to_string());
    assert!(
        body["deleted_at"].is_string(),
        "post-delete projection MUST carry deleted_at: {body}"
    );
}

#[tokio::test]
async fn delete_root_tenant_returns_400() {
    // `root_tenant_cannot_delete` per the service-side precondition;
    // the wire-side mapping routes it through `validation` -> 400.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "DELETE",
        &format!("/account-management/v1/tenants/{root}"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ─── POST /tenants/{id}/suspend + /unsuspend (AIP-136 sub-resource) ──
//
// Sub-resource fallback for the AIP-136 colon-method shape
// (`{tenant_id}:suspend` / `:unsuspend`) — axum 0.8.x pins
// `matchit = "=0.8.4"` which cannot split `{param}:suffix` in a single
// segment. Tracked: tokio-rs/axum#3702 / tokio-rs/axum#3140. The
// service-side semantics (idempotency, status-transition matrix) are
// pinned in `domain::tenant::service::service_tests`; these tests
// pin the wire shape, status codes, and the post-write projection.

#[tokio::test]
async fn suspend_tenant_returns_200_with_suspended_status() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/suspend"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["id"], child.to_string());
    assert_eq!(body["status"], "suspended");
    assert_eq!(
        body["parent_id"],
        serde_json::Value::String(root.to_string())
    );
    assert!(
        body["updated_at"].is_string(),
        "post-suspend projection MUST carry updated_at: {body}"
    );
}

#[tokio::test]
async fn suspend_tenant_is_idempotent_on_suspended_tenant() {
    // Mirrors the service-level
    // `suspend_tenant_double_call_is_observably_identical` invariant
    // end-to-end: a retry of the same suspend MUST also return 200,
    // MUST observe `status=suspended`, and MUST NOT bump
    // `updated_at` (the service short-circuits on same-to-same inside
    // the SERIALIZABLE write).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let first_req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/suspend"),
        None,
        ctx_for(root),
    );
    let first_resp = router.clone().oneshot(first_req).await.expect("router");
    assert_eq!(first_resp.status(), StatusCode::OK);
    let first_body = response_body(first_resp).await;
    let first_updated_at = first_body["updated_at"].clone();
    assert_eq!(first_body["status"], "suspended");

    // Cross a wall-clock tick so a non-idempotent implementation that
    // re-stamped `updated_at` per call would visibly diverge.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let second_req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/suspend"),
        None,
        ctx_for(root),
    );
    let second_resp = router.oneshot(second_req).await.expect("router");
    assert_eq!(second_resp.status(), StatusCode::OK);
    let second_body = response_body(second_resp).await;
    assert_eq!(second_body["status"], "suspended");
    assert_eq!(
        first_updated_at, second_body["updated_at"],
        "second identical suspend MUST NOT touch `updated_at` \
         (idempotency)"
    );
}

#[tokio::test]
async fn suspend_tenant_rejects_deleted_tenant_with_400() {
    // `DomainError::Conflict` (target is `Deleted`) maps to
    // `CanonicalError::FailedPrecondition`, rendered as HTTP 400 (see
    // `infra::sdk_error_mapping` -- the only `409` paths on the AM
    // surface are `AlreadyExists` and `Aborted`/serialization-conflict).
    // The task brief named this "_409" but the wire reality matches the
    // brief's own "Error model" subsection that lists Conflict as
    // "HTTP 400 `failed_precondition`".
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    // Soft-delete the leaf via the production DELETE handler so the
    // row is a real `status=deleted` row (with `deleted_at` armed),
    // not a hand-stamped fixture state.
    let delete_req = json_request(
        "DELETE",
        &format!("/account-management/v1/tenants/{child}"),
        None,
        ctx_for(root),
    );
    let delete_resp = router.clone().oneshot(delete_req).await.expect("router");
    assert_eq!(delete_resp.status(), StatusCode::NO_CONTENT);

    let suspend_req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/suspend"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(suspend_req).await.expect("router");
    let (status, body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.is_object(),
        "expected canonical Problem envelope, got {body}"
    );
}

#[tokio::test]
async fn unsuspend_tenant_returns_200_with_active_status() {
    // Round-trip: seed active, suspend via the wire surface, then
    // unsuspend via the wire surface. Pins the wire shape AND that
    // the unsuspend route does in fact flip the row back to `active`.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let suspend_req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/suspend"),
        None,
        ctx_for(root),
    );
    let suspend_resp = router.clone().oneshot(suspend_req).await.expect("router");
    assert_eq!(suspend_resp.status(), StatusCode::OK);
    let suspended_body = response_body(suspend_resp).await;
    assert_eq!(suspended_body["status"], "suspended");

    let unsuspend_req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/unsuspend"),
        None,
        ctx_for(root),
    );
    let unsuspend_resp = router.oneshot(unsuspend_req).await.expect("router");
    assert_eq!(unsuspend_resp.status(), StatusCode::OK);
    let active_body = response_body(unsuspend_resp).await;
    assert_eq!(active_body["status"], "active");
    assert_eq!(active_body["id"], child.to_string());
}

#[tokio::test]
async fn unsuspend_tenant_is_idempotent_on_active_tenant() {
    // Symmetric to `suspend_tenant_is_idempotent_on_suspended_tenant`.
    // The handler docstring at `handlers/tenants.rs:233` documents the
    // unsuspend contract as "Idempotent on already-active rows" — pin
    // it end-to-end: a retry MUST return 200 with `status=active` and
    // MUST NOT bump `updated_at` (service short-circuits same-to-same).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let first_req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/unsuspend"),
        None,
        ctx_for(root),
    );
    let first_resp = router.clone().oneshot(first_req).await.expect("router");
    assert_eq!(first_resp.status(), StatusCode::OK);
    let first_body = response_body(first_resp).await;
    let first_updated_at = first_body["updated_at"].clone();
    assert_eq!(first_body["status"], "active");

    // Cross a wall-clock tick so a non-idempotent implementation that
    // re-stamped `updated_at` per call would visibly diverge.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let second_req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/unsuspend"),
        None,
        ctx_for(root),
    );
    let second_resp = router.oneshot(second_req).await.expect("router");
    assert_eq!(second_resp.status(), StatusCode::OK);
    let second_body = response_body(second_resp).await;
    assert_eq!(second_body["status"], "active");
    assert_eq!(
        first_updated_at, second_body["updated_at"],
        "second identical unsuspend MUST NOT touch `updated_at` \
         (idempotency)"
    );
}

#[tokio::test]
async fn suspend_tenant_not_found_returns_404() {
    // A `POST /tenants/{unknown_uuid}/suspend` MUST collapse to 404 with
    // the canonical Problem envelope rather than leaking via 500 or
    // silent 200. Pins the existence-channel for the AIP-136 sub-resource
    // method (mirrors `get_tenant_not_found_returns_404`).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let unknown = Uuid::new_v4();
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{unknown}/suspend"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_object(), "envelope must be an object: {body}");
}

#[tokio::test]
async fn unsuspend_tenant_not_found_returns_404() {
    // Mirror of `suspend_tenant_not_found_returns_404` for the
    // unsuspend custom method.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let unknown = Uuid::new_v4();
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{unknown}/unsuspend"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_object(), "envelope must be an object: {body}");
}

// ─── Auth wiring (missing SecurityContext) ───────────────────────────

#[tokio::test]
async fn unauthenticated_request_missing_security_context_fails() {
    // Handlers extract `Extension<SecurityContext>`. When the gateway
    // does NOT inject the extension, the axum extractor short-circuits
    // BEFORE the handler runs. The exact status varies by axum
    // configuration but is one of the canonical auth-side codes
    // (`400 Bad Request` / `401 Unauthorized` / `403 Forbidden` /
    // `500 Internal Server Error`). The test pins only the structural
    // contract — the call is rejected and no tenant is leaked.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request_no_ctx(
        "GET",
        &format!("/account-management/v1/tenants/{root}"),
        None,
    );
    let resp = router.oneshot(req).await.expect("router");
    // The call MUST NOT return 200 — the missing extension MUST short-
    // circuit before the handler can read the tenant row.
    assert_ne!(resp.status(), StatusCode::OK);
    assert!(
        matches!(
            resp.status(),
            StatusCode::BAD_REQUEST
                | StatusCode::UNAUTHORIZED
                | StatusCode::FORBIDDEN
                | StatusCode::INTERNAL_SERVER_ERROR
        ),
        "unexpected status on missing SecurityContext: {}",
        resp.status(),
    );
}
