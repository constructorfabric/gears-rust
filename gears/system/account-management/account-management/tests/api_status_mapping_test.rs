//! Wire-level error-envelope mapping per `DomainError` variant.
//!
//! Complements:
//! * [`tests/api_envelope_test.rs`] — shape pinning that is identical
//!   across every endpoint family (`type` / `title` / `status` /
//!   `detail` field presence, `application/problem+json` content
//!   type).
//! * [`src/infra/sdk_error_mapping_tests.rs`] — unit-level pinning
//!   that `DomainError → AccountManagementError → CanonicalError`
//!   produces the expected category, resource type, and precondition
//!   token for every variant.
//!
//! This file closes the gap between those two: it drives a real HTTP
//! request that triggers each variant's production code path and pins
//! the wire envelope — status code, canonical `type` URI, and the
//! relevant `context.*` field (resource type, violation token,
//! reason). A regression in either the handler-side `IntoResponse` or
//! the canonical-error mapper would change the wire envelope without
//! changing any unit test; this sweep catches that class of drift.
//!
//! # Variants covered here
//!
//! * `RootTenantCannotDelete` — DELETE on the platform root.
//! * `RootTenantCannotConvert` — POST conversion on the platform root.
//! * `TenantHasChildren` — DELETE a parent that still has children.
//! * `AlreadyResolved` — re-cancel a cancelled conversion request.
//!
//! # Variants covered elsewhere
//!
//! * `PendingExists → 409` — covered by
//!   `tests/api_conversions_test.rs::request_*_when_pending_exists_returns_409`.
//! * `InvalidActorForTransition → 400` — covered by
//!   `tests/api_conversions_test.rs::patch_own_conversion_approved_by_initiator_returns_403_or_400`.
//! * `MetadataEntryNotFound → 404` — covered by
//!   `tests/api_metadata_test.rs::get_metadata_*_returns*404*`.
//! * `NotFound`, `Validation`, generic shape — `tests/api_envelope_test.rs`.
//!
//! # Variants NOT covered at the HTTP level
//!
//! * `Conflict` / `Aborted` / `ServiceUnavailable` / `IdpUnavailable`
//!   need either concurrent transactions, a failing PDP, or a failing
//!   `IdP` plugin to surface from a real handler. Their canonical
//!   shape is pinned by the unit-level mapping tests; introducing
//!   bespoke harnesses to fault-inject those failures end-to-end is
//!   out of scope for this sweep.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use axum::http::StatusCode;
use toolkit_gts::{gts_id, gts_uri};
use tower::ServiceExt;
use uuid::Uuid;

use common::*;

const TENANT_GTS: &str = gts_id!("cf.core.am.tenant.v1~");
const CONVERSION_REQUEST_GTS: &str = gts_id!("cf.core.am.conversion_request.v1~");

const INVALID_ARGUMENT_TYPE: &str =
    gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~");
const FAILED_PRECONDITION_TYPE: &str =
    gts_uri!("cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~");

#[tokio::test]
async fn root_tenant_cannot_delete_envelope() {
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
    let (status, body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], 400);
    assert_eq!(
        body["type"], INVALID_ARGUMENT_TYPE,
        "RootTenantCannotDelete MUST map to canonical InvalidArgument: {body}"
    );
    assert_eq!(
        body["context"]["resource_type"], TENANT_GTS,
        "envelope MUST be keyed on the tenant resource: {body}"
    );
    // InvalidArgument carries `field_violations[].reason` (the
    // FailedPrecondition envelope uses `violations[].type` instead;
    // see the AlreadyResolved test below for the contrast).
    assert_eq!(
        body["context"]["field_violations"][0]["reason"], "ROOT_TENANT_CANNOT_DELETE",
        "violation reason MUST be `ROOT_TENANT_CANNOT_DELETE`: {body}"
    );
    assert_eq!(
        body["context"]["field_violations"][0]["field"], "tenant_id",
        "violation field MUST point at `tenant_id`: {body}"
    );
}

#[tokio::test]
async fn root_tenant_cannot_convert_envelope() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    // POST a child-side conversion on the platform root tenant.
    // The platform root has `parent_id IS NULL`; the service-side
    // guard rejects this with `RootTenantCannotConvert`, which the
    // mapper routes through the invalid-argument category.
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{root}/conversions"),
        Some(serde_json::json!({ "target_mode": "self_managed" })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], 400);
    assert_eq!(
        body["type"], INVALID_ARGUMENT_TYPE,
        "RootTenantCannotConvert MUST map to canonical InvalidArgument: {body}"
    );
    assert_eq!(
        body["context"]["resource_type"], TENANT_GTS,
        "envelope MUST be keyed on the tenant resource (not conversion): {body}"
    );
    assert_eq!(
        body["context"]["field_violations"][0]["reason"], "ROOT_TENANT_CANNOT_CONVERT",
        "violation reason MUST be `ROOT_TENANT_CANNOT_CONVERT`: {body}"
    );
    assert_eq!(
        body["context"]["field_violations"][0]["field"], "tenant_id",
        "violation field MUST point at `tenant_id`: {body}"
    );
}

#[tokio::test]
async fn tenant_has_children_envelope() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let parent = Uuid::new_v4();
    seed_active_child(&h, parent, root, "parent", 1).await;
    let grandchild = Uuid::new_v4();
    seed_active_child(&h, grandchild, parent, "grandchild", 2).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    // DELETE the parent — `parent` still has `grandchild` underneath,
    // so the soft-delete guard rejects via `TenantHasChildren`.
    let req = json_request(
        "DELETE",
        &format!("/account-management/v1/tenants/{parent}"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], 400);
    assert_eq!(
        body["type"], FAILED_PRECONDITION_TYPE,
        "TenantHasChildren MUST map to canonical FailedPrecondition: {body}"
    );
    assert_eq!(
        body["context"]["resource_type"], TENANT_GTS,
        "envelope MUST be keyed on the tenant resource: {body}"
    );
    assert_eq!(
        body["context"]["violations"][0]["type"], "TENANT_HAS_CHILDREN",
        "violation token MUST be `TENANT_HAS_CHILDREN`: {body}"
    );
}

#[tokio::test]
async fn already_resolved_envelope_on_re_cancel() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let child = Uuid::new_v4();
    seed_active_child(&h, child, root, "child", 1).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    // Create a child-side conversion and cancel it.
    let post = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{child}/conversions"),
        Some(serde_json::json!({ "target_mode": "self_managed" })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(post).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let request_id = response_body(resp).await["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let cancel = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{child}/conversions/{request_id}"),
        Some(serde_json::json!({ "status": "cancelled" })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(cancel).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "first cancel MUST succeed before the precedence test fires"
    );

    // Re-cancel — the row is already in a terminal state, so
    // `AlreadyResolved` fires (precedes the actor check by contract).
    let re_cancel = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{child}/conversions/{request_id}"),
        Some(serde_json::json!({ "status": "cancelled" })),
        ctx_for(root),
    );
    let resp = router.oneshot(re_cancel).await.expect("router");
    let (status, body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], 400);
    assert_eq!(
        body["type"], FAILED_PRECONDITION_TYPE,
        "AlreadyResolved MUST map to canonical FailedPrecondition: {body}"
    );
    assert_eq!(
        body["context"]["resource_type"], CONVERSION_REQUEST_GTS,
        "envelope MUST be keyed on the conversion-request resource: {body}"
    );
}
