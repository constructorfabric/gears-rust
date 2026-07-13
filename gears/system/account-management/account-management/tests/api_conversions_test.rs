//! HTTP-level E2E tests for the
//! `/account-management/v1/tenants/{tenant_id}/conversions*` and
//! `.../child-conversions*` REST surface.
//!
//! Scope: the dual-consent lifecycle through the real router. The
//! deep service-level matrix (wrong-actor / re-entry / TX-side
//! type re-evaluation) is pinned by `conversion_integration.rs` plus
//! the in-source `service_tests`.

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
use toolkit_gts::gts_uri;
use tower::ServiceExt;
use uuid::Uuid;

use common::*;

const SELF_MANAGED: &str = "self_managed";
const MANAGED: &str = "managed";

// ─── POST /tenants/{child_id}/conversions (own / child-side) ─────────

#[tokio::test]
async fn request_own_conversion_returns_201_with_dto() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let body = serde_json::json!({ "target_mode": SELF_MANAGED });
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .expect("Location header")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        location.contains("/conversions/"),
        "Location must include conversions path: {location}"
    );
    let body = response_body(resp).await;
    assert_eq!(body["tenant_id"], child.to_string());
    assert_eq!(body["target_mode"], SELF_MANAGED);
    assert_eq!(body["status"], "pending");
    assert_eq!(body["initiator_side"], "child");
}

/// Pin the partial-unique-index collision on the wire: a second
/// `POST /conversions` for the same tenant while a `Pending` row
/// already exists MUST surface as HTTP 409 carrying the existing
/// row's id in the canonical envelope. The mapping
/// `DomainError::PendingExists → CanonicalError::AlreadyExists 409`
/// is unit-tested in `sdk_error_mapping_tests.rs:250`; this test
/// ties the wire status, `type`, and `context.resource_name` to that
/// contract so a future regression in the handler-side `IntoResponse`
/// or in the canonical-error mapper cannot silently downgrade the
/// response to 400 / 422.
#[tokio::test]
async fn request_own_conversion_when_pending_exists_returns_409() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let body = serde_json::json!({ "target_mode": SELF_MANAGED });
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(body.clone()),
        ctx_for(root),
    );
    let resp = router
        .clone()
        .oneshot(req)
        .await
        .expect("first POST: router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let first = response_body(resp).await;
    let existing_id = first["id"]
        .as_str()
        .expect("201 body MUST carry the conversion-request id")
        .to_owned();

    let req2 = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(body),
        ctx_for(root),
    );
    let resp2 = router.oneshot(req2).await.expect("second POST: router");
    let (status, body) = response_problem(resp2).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "duplicate-on-create MUST return 409 (not 400 / 422 / 500): body={body}"
    );
    assert_eq!(
        body["status"], 409,
        "RFC 9457 envelope `status` field MUST mirror the HTTP status: {body}"
    );
    // `type` is `gts://<gts_type>` per toolkit_canonical_errors::Problem;
    // the `already_exists` category surfaces with the canonical chain.
    assert_eq!(
        body["type"],
        gts_uri!("cf.core.errors.err.v1~cf.core.err.already_exists.v1~"),
        "envelope `type` MUST point at the canonical AlreadyExists GTS id: {body}"
    );
    // `context.resource_type` pins the AM-side resource that collided;
    // `context.resource_name` carries the EXISTING pending row's id so
    // the caller can cancel / reject it before retrying without a
    // separate lookup.
    assert_eq!(
        body["context"]["resource_type"],
        gts_id!("cf.core.am.conversion_request.v1~"),
        "envelope `context.resource_type` MUST be the conversion-request \
         resource (not the tenant resource): {body}"
    );
    assert_eq!(
        body["context"]["resource_name"], existing_id,
        "envelope `context.resource_name` MUST carry the existing pending \
         request's id so the caller can drive cancel/reject before retry: \
         {body}"
    );
}

/// Cross-side variant: the partial-unique-index covers `(tenant_id)`
/// regardless of `initiator_side`, so a parent-side re-post against a
/// child that already has a pending child-side request MUST also
/// surface as 409 with the existing row's id. Without this the wire
/// contract for parent-driven flows could drift to a different status
/// while the same DB constraint is enforced underneath.
#[tokio::test]
async fn request_child_conversion_when_pending_exists_returns_409() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let own_body = serde_json::json!({ "target_mode": SELF_MANAGED });
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(own_body),
        ctx_for(root),
    );
    let resp = router
        .clone()
        .oneshot(req)
        .await
        .expect("child-side POST: router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let first = response_body(resp).await;
    let existing_id = first["id"]
        .as_str()
        .expect("201 body MUST carry the conversion-request id")
        .to_owned();

    let parent_body = serde_json::json!({
        "child_tenant_id": child.to_string(),
        "target_mode": SELF_MANAGED,
    });
    let req2 = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{root}/child-conversions"),
        Some(parent_body),
        ctx_for(root),
    );
    let resp2 = router
        .oneshot(req2)
        .await
        .expect("parent-side POST: router");
    let (status, body) = response_problem(resp2).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "parent-side re-post against a tenant with an existing child-side \
         pending MUST return 409: body={body}"
    );
    assert_eq!(body["status"], 409, "envelope status MUST be 409: {body}");
    assert_eq!(
        body["context"]["resource_name"], existing_id,
        "envelope MUST carry the existing pending row's id regardless of \
         which side re-posted: {body}"
    );
}

// ─── POST /tenants/{parent_id}/child-conversions (parent-side) ───────

#[tokio::test]
async fn request_child_conversion_returns_201_with_dto() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let body = serde_json::json!({
        "child_tenant_id": child.to_string(),
        "target_mode": SELF_MANAGED,
    });
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{root}/child-conversions"),
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_body(resp).await;
    assert_eq!(body["tenant_id"], child.to_string());
    assert_eq!(body["target_mode"], SELF_MANAGED);
    assert_eq!(body["status"], "pending");
    assert_eq!(body["initiator_side"], "parent");
}

// ─── GET /tenants/{child_id}/conversions ─────────────────────────────

#[tokio::test]
async fn list_own_conversions_returns_200_with_page() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    // Seed via POST.
    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let get = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn list_own_conversions_filter_by_status_eq_pending() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let get = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?%24filter=status%20eq%20%27pending%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items");
    assert!(
        items.iter().all(|i| i["status"] == "pending"),
        "filter=status eq 'pending' must surface only pending rows: {body}",
    );
}

// ─── GET /tenants/{parent_id}/child-conversions ──────────────────────

#[tokio::test]
async fn list_child_conversions_returns_200_with_page() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    // Seed parent-initiated conversion.
    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{root}/child-conversions"),
        Some(serde_json::json!({
            "child_tenant_id": child.to_string(),
            "target_mode": SELF_MANAGED,
        })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let get = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/child-conversions"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn list_child_conversions_orderby_created_at_desc() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{root}/child-conversions"),
        Some(serde_json::json!({
            "child_tenant_id": child.to_string(),
            "target_mode": SELF_MANAGED,
        })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let get = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/child-conversions?%24orderby=created_at%20desc"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
}

// ─── GET /tenants/{child_id}/conversions/{request_id} ────────────────

#[tokio::test]
async fn get_own_conversion_returns_200_with_dto() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created_body = response_body(resp).await;
    let request_id = created_body["id"].as_str().expect("created id").to_owned();

    let get = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{child}/conversions/{request_id}"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["id"], request_id);
    assert_eq!(body["status"], "pending");
}

#[tokio::test]
async fn get_own_conversion_unknown_id_returns_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let unknown = Uuid::new_v4();
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{child}/conversions/{unknown}"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ─── PATCH own conversion ────────────────────────────────────────────

#[tokio::test]
async fn patch_own_conversion_cancelled_returns_200() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    let created_body = response_body(resp).await;
    let request_id = created_body["id"].as_str().expect("id").to_owned();

    let patch = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{child}/conversions/{request_id}"),
        Some(serde_json::json!({ "status": "cancelled" })),
        ctx_for(root),
    );
    let resp = router.oneshot(patch).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["status"], "cancelled");
}

#[tokio::test]
async fn patch_own_conversion_approved_by_initiator_returns_403_or_400() {
    // Child-side caller cannot approve their own request — the
    // counterparty (parent) is the only admissible approver. Service
    // surfaces `InvalidActorForTransition` which maps to a 4xx
    // (failed_precondition → 400 in AM's mapping table).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    let created_body = response_body(resp).await;
    let request_id = created_body["id"].as_str().expect("id").to_owned();

    // Try to approve from the child side.
    let patch = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{child}/conversions/{request_id}"),
        Some(serde_json::json!({ "status": "approved" })),
        ctx_for(root),
    );
    let resp = router.oneshot(patch).await.expect("router");
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "child-side approve MUST map to failed_precondition (400), got {status}"
    );
    let body = response_body(resp).await;
    assert_eq!(
        body["context"]["violations"][0]["type"], "INVALID_ACTOR_FOR_TRANSITION",
        "expected INVALID_ACTOR_FOR_TRANSITION violation in envelope, got {body}",
    );
}

// ─── PATCH child conversion (parent-side) ────────────────────────────

#[tokio::test]
async fn patch_child_conversion_approved_by_parent_returns_200() {
    // Child-initiated → parent approves (counterparty).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    // Child initiates.
    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    let created_body = response_body(resp).await;
    let request_id = created_body["id"].as_str().expect("id").to_owned();

    // Parent approves via the parent-side path.
    let patch = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/child-conversions/{request_id}"),
        Some(serde_json::json!({ "status": "approved" })),
        ctx_for(root),
    );
    let resp = router.oneshot(patch).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["status"], "approved");
}

#[tokio::test]
async fn patch_child_conversion_rejected_by_parent_returns_200() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    let created_body = response_body(resp).await;
    let request_id = created_body["id"].as_str().expect("id").to_owned();

    let patch = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/child-conversions/{request_id}"),
        Some(serde_json::json!({ "status": "rejected" })),
        ctx_for(root),
    );
    let resp = router.oneshot(patch).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["status"], "rejected");
}

#[tokio::test]
async fn patch_conversion_unknown_id_returns_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let unknown = Uuid::new_v4();
    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{child}/conversions/{unknown}"),
        Some(serde_json::json!({ "status": "cancelled" })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ─── Validation ──────────────────────────────────────────────────────

#[tokio::test]
async fn patch_conversion_status_invalid_value_is_rejected() {
    // `ConversionPatchStatusDto` is narrowed to approve / cancel /
    // reject; serde rejects everything else at deserialise time
    // (axum's Json extractor surfaces serde failures as 422).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let some_id = Uuid::new_v4();
    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{child}/conversions/{some_id}"),
        Some(serde_json::json!({ "status": "garbage" })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert!(
        matches!(
            resp.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY,
        ),
        "invalid status enum value MUST surface as 400/422, got {}",
        resp.status(),
    );
}

#[tokio::test]
async fn request_conversion_empty_body_is_rejected() {
    // `RequestOwnConversionDto.target_mode` is required.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({})),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert!(
        matches!(
            resp.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY,
        ),
        "missing target_mode MUST surface as 400/422, got {}",
        resp.status(),
    );
}

#[tokio::test]
async fn request_conversion_invalid_target_mode_is_rejected() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": "garbage" })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert!(
        matches!(
            resp.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY,
        ),
        "invalid target_mode enum value MUST surface as 400/422, got {}",
        resp.status(),
    );
}

#[tokio::test]
async fn request_conversion_wrong_direction_returns_400() {
    // The child is currently managed (`self_managed=false`), so the
    // only admissible `target_mode` is `self_managed`. Asking for
    // `managed` flips back to the current state — service rejects
    // with `Validation`.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": MANAGED })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_conversions_invalid_filter_syntax_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{child}/conversions?%24filter=junk"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ─── OData enum-column rejection contracts ───────────────────────────
//
// The wire-side `status` / `target_mode` / `initiator_side` filters
// accept a small enum-string vocabulary that AM's
// `ConversionRequestODataMapper` translates to the storage `SMALLINT`
// ordinal. Three rejection paths exist in
// [`crate::infra::storage::repo_impl::conversion::ConversionRequestODataMapper`]:
//
// * `is_orderable = false` rejects `$orderby` on the categorical
//   columns — wire is alphabetical, storage is numeric ordinal, no
//   consistent ordering.
// * `reject_ordered` blocks `gt` / `ge` / `lt` / `le` so a caller
//   cannot smuggle in a comparison against the hidden ordinal.
// * `map_value` rejects unknown enum-string values at parse time
//   instead of silently translating to `None`.
//
// The unit-level coverage pins the mapper in isolation; these tests
// pin the wire-level mapping so a regression in the surrounding REST
// surface (handler-side error mapping, parser ordering) surfaces as a
// failing test rather than a silent 200 with bogus data.

#[tokio::test]
async fn list_conversions_orderby_status_is_rejected() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{child}/conversions?%24orderby=status%20desc"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "$orderby=status MUST be rejected -- wire-vs-storage ordering disagrees"
    );
}

#[tokio::test]
async fn list_conversions_orderby_target_mode_is_rejected() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?%24orderby=target_mode%20desc"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_conversions_orderby_initiator_side_is_rejected() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?%24orderby=initiator_side%20desc"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_conversions_filter_status_gt_is_rejected() {
    // `gt` on a categorical column would compare against the hidden
    // `SMALLINT` ordinal — exactly the bug the wire envelope is
    // designed to prevent. MUST surface as 400, not "200 with junk".
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?\
             %24filter=status%20gt%20%27pending%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "$filter=status gt 'X' MUST be rejected -- wire alphabetical vs storage ordinal"
    );
}

#[tokio::test]
async fn list_conversions_filter_target_mode_gt_is_rejected() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?\
             %24filter=target_mode%20gt%20%27managed%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_conversions_filter_initiator_side_lt_is_rejected() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?\
             %24filter=initiator_side%20lt%20%27parent%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_conversions_filter_status_unknown_value_is_rejected() {
    // The mapper enumerates the admissible string set and Err's on
    // anything else. Without this guard, an unknown value would
    // silently translate to `None` in the underlying odata layer and
    // either widen the read or surface a confusing internal error.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?\
             %24filter=status%20eq%20%27halfway%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "unknown `status` value MUST be rejected at parse time"
    );
}

#[tokio::test]
async fn list_conversions_filter_target_mode_unknown_value_is_rejected() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?\
             %24filter=target_mode%20eq%20%27partially_managed%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_conversions_filter_initiator_side_unknown_value_is_rejected() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{child}/conversions?\
             %24filter=initiator_side%20eq%20%27cousin%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ─── Audit-trail / cross-tenant invariants ───────────────────────────

#[tokio::test]
async fn dual_consent_audit_records_distinct_initiator_and_approver() {
    // Audit invariant: `requested_by` and `approved_by` MUST be the
    // two distinct UUIDs that actually drove each side of the
    // dual-consent flow. The rest of the suite reuses a single
    // `ctx_for(root)` (subject_id = 0xCAFE) for both sides; this test
    // wires distinct subjects via `ctx_for_with_subject` so the audit
    // columns are observable as different.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let initiator = Uuid::from_u128(0xA11CE);
    let approver = Uuid::from_u128(0xB0B);
    assert_ne!(initiator, approver, "test setup: subjects must differ");

    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for_with_subject(root, initiator),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = response_body(resp).await;
    let request_id = created["id"].as_str().expect("id").to_owned();
    assert_eq!(
        created["requested_by"],
        initiator.to_string(),
        "POST must stamp requested_by with the caller's subject_id",
    );

    let patch = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/child-conversions/{request_id}"),
        Some(serde_json::json!({ "status": "approved" })),
        ctx_for_with_subject(root, approver),
    );
    let resp = router.oneshot(patch).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["status"], "approved");
    assert_eq!(
        body["requested_by"],
        initiator.to_string(),
        "approve must preserve the initiator's requested_by",
    );
    assert_eq!(
        body["approved_by"],
        approver.to_string(),
        "approve must stamp approved_by with the approver's subject_id",
    );
    assert_ne!(
        body["requested_by"], body["approved_by"],
        "audit trail MUST distinguish initiator from approver",
    );
}

#[tokio::test]
async fn list_own_conversions_does_not_leak_sibling_subtree() {
    // Cross-subtree isolation: a conversion under `subtree_a` MUST NOT
    // be visible to a caller scoped to `subtree_b`. The DB enforces a
    // single root via `ux_tenants_single_root`, so the two isolated
    // subtrees are seeded as siblings under the same `root`.
    //
    //   root
    //   ├── subtree_a   (subject_tenant_id of caller_a)
    //   │   └── leaf_a  (target of the conversion)
    //   └── subtree_b   (subject_tenant_id of caller_b)
    //
    // The mock PDP returns `InTenantSubtree(subject_tenant_id)`, so a
    // ctx built for `subtree_b` clamps reads to that subtree —
    // `leaf_a` is outside it and must be invisible.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let subtree_a = Uuid::new_v4();
    seed_active_child(&h, subtree_a, root, "subtree_a", 1).await;
    let leaf_a = Uuid::new_v4();
    seed_active_child(&h, leaf_a, subtree_a, "leaf_a", 2).await;
    let subtree_b = Uuid::new_v4();
    seed_active_child(&h, subtree_b, root, "subtree_b", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    // Seed a pending conversion under `subtree_a`.
    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{leaf_a}/conversions"),
        Some(serde_json::json!({ "target_mode": SELF_MANAGED })),
        ctx_for(subtree_a),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);

    // List `leaf_a`'s conversions through a `subtree_b` ctx — the
    // URL-bound `leaf_a` is outside `subtree_b`'s subtree clamp. The
    // test PDP is permissive, so the PEP never returns 403; the
    // secure-orm subtree clamp is the sole gate and collapses leaf_a
    // out of subtree_b's view → 404. Pinning to exactly NOT_FOUND
    // catches the case where a regression accidentally lets the row
    // through the clamp and the wire flips to 403 — the resulting
    // posture is authorization-channel not existence-channel, which
    // is a wire-shaped information leak.
    let get = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{leaf_a}/conversions"),
        None,
        ctx_for(subtree_b),
    );
    let resp = router.oneshot(get).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "cross-subtree access to leaf_a's listing MUST be a clamp 404 \
         (the test PDP is permissive, so a 403 here signals a posture leak)",
    );
}

#[tokio::test]
async fn list_child_conversions_does_not_leak_sibling_subtree() {
    // Parent-side analogue of
    // `list_own_conversions_does_not_leak_sibling_subtree`. Same
    // tenant shape: a parent-initiated conversion seeded under
    // `subtree_a` must be invisible to a caller scoped to
    // `subtree_b`.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let subtree_a = Uuid::new_v4();
    seed_active_child(&h, subtree_a, root, "subtree_a", 1).await;
    let leaf_a = Uuid::new_v4();
    seed_active_child(&h, leaf_a, subtree_a, "leaf_a", 2).await;
    let subtree_b = Uuid::new_v4();
    seed_active_child(&h, subtree_b, root, "subtree_b", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    // Parent (`subtree_a`) initiates a conversion on its own child
    // (`leaf_a`).
    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{subtree_a}/child-conversions"),
        Some(serde_json::json!({
            "child_tenant_id": leaf_a.to_string(),
            "target_mode": SELF_MANAGED,
        })),
        ctx_for(subtree_a),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);

    // `subtree_b`'s ctx asks for `subtree_a`'s child-conversions
    // listing — the URL-bound parent is outside `subtree_b`'s subtree
    // clamp. Pinned to NOT_FOUND (not "404 OR 403") for the same
    // posture-leak reason documented in the sibling
    // `list_own_conversions_does_not_leak_sibling_subtree` test
    // above.
    let get = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{subtree_a}/child-conversions"),
        None,
        ctx_for(subtree_b),
    );
    let resp = router.oneshot(get).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "cross-subtree access to subtree_a's parent-side listing MUST be a clamp 404",
    );
}
