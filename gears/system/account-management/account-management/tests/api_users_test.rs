//! HTTP-level E2E tests for the
//! `/account-management/v1/tenants/{tenant_id}/users*` REST surface.
//!
//! Scope: provision / list / deprovision through the real router
//! against the in-memory `FakeIdpPlugin` echo. Service-side username
//! validation and IdP failure mapping are pinned by
//! `domain::user::service_tests`.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::doc_markdown
)]

mod common;

use axum::http::StatusCode;
use tower::ServiceExt;
use uuid::Uuid;

use common::*;

fn build_users_router(h: &Harness) -> axum::Router {
    // `create_user` is fail-closed on a missing `gts.cf.core.am.user.v1~`
    // schema, so the users tests use the user-aware variant of the
    // types-registry helper.
    let services = build_services_full(
        h,
        fake_idp(),
        empty_metadata_registry(),
        types_registry_for_users(),
    );
    build_test_router(&services)
}

// ─── POST /tenants/{id}/users ────────────────────────────────────────

#[tokio::test]
async fn provision_user_returns_201_with_idp_user_dto() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let body = serde_json::json!({"username": "alice"});
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{root}/users"),
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_body(resp).await;
    assert_eq!(body["username"], "alice");
    assert!(body["id"].is_string(), "id must be present: {body}");
}

#[tokio::test]
async fn provision_user_with_full_profile_returns_201() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let body = serde_json::json!({
        "username": "bob",
        "email": "bob@example.com",
        "display_name": "Bob Q.",
    });
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{root}/users"),
        Some(body),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_body(resp).await;
    assert_eq!(body["username"], "bob");
    assert_eq!(body["email"], "bob@example.com");
    assert_eq!(body["display_name"], "Bob Q.");
}

// ─── DELETE /tenants/{id}/users/{user_id} ────────────────────────────

#[tokio::test]
async fn deprovision_user_returns_204_no_content() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let some_user = Uuid::new_v4();
    let req = json_request(
        "DELETE",
        &format!("/account-management/v1/tenants/{root}/users/{some_user}"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn deprovision_user_already_absent_returns_204_idempotent() {
    // Per `cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard`:
    // a second DELETE on a user the IdP already considers absent must
    // still surface 204. The stateful `FakeIdpPlugin::deprovision_user`
    // maps both removed-and-already-absent to `Ok(())` per the SDK
    // trait contract, so two consecutive DELETEs on the same id both
    // see 204 regardless of whether the row was ever provisioned.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let some_user = Uuid::new_v4();
    let path = format!("/account-management/v1/tenants/{root}/users/{some_user}");

    let req = json_request("DELETE", &path, None, ctx_for(root));
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let req = json_request("DELETE", &path, None, ctx_for(root));
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

// ─── GET /tenants/{id}/users ─────────────────────────────────────────

#[tokio::test]
async fn list_users_returns_200_with_page() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/users"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert!(body["items"].is_array(), "items must be an array: {body}");
    assert!(
        body["page_info"].is_object(),
        "page_info must be an object: {body}",
    );
}

#[tokio::test]
async fn list_users_filtered_by_user_id_returns_200() {
    // Per the OData lowering in `lower_odata_to_list_users_query`:
    // `$filter=id eq <uuid>` is the canonical point-lookup / existence-
    // check shape; with `$top=1` the handler emits an empty page for an
    // absent id (authoritative absent signal per FEATURE §5.5 DoD).
    // The populated-uid filter is exercised by
    // `user_lifecycle_round_trip_against_stateful_fake` below.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let probe = Uuid::new_v4();
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=id%20eq%20{probe}&limit=1"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert!(items.is_empty(), "unknown user_id MUST return empty page");
}

#[tokio::test]
async fn user_lifecycle_round_trip_against_stateful_fake() {
    // End-to-end coverage for the create → list → list-filtered →
    // delete → list-empty round-trip against the stateful in-memory
    // IdP fake. Pre-fix the harness's `FakeIdpPlugin::list_users`
    // returned `Page::empty(50)` and `deprovision_user` silently
    // ignored its argument, so regressions in user_id filtering,
    // list-after-create visibility, or delete cleanup could ship
    // green.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    // POST /users — provision two users.
    for username in ["alice", "bob"] {
        let req = json_request(
            "POST",
            &format!("/account-management/v1/tenants/{root}/users"),
            Some(serde_json::json!({ "username": username })),
            ctx_for(root),
        );
        let resp = router.clone().oneshot(req).await.expect("router");
        assert_eq!(resp.status(), StatusCode::CREATED, "create {username}");
    }

    // GET /users — both visible.
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/users"),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "post-create list MUST surface both users");

    let alice_id: Uuid = items
        .iter()
        .find(|u| u["username"] == "alice")
        .and_then(|u| u["id"].as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .expect("alice id");

    // GET /users?$filter=id eq <alice>&$top=1 — point-lookup returns exactly one.
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=id%20eq%20{alice_id}&limit=1"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "filtered list MUST return one row");
    assert_eq!(items[0]["username"], "alice");

    // DELETE /users/<alice> — 204.
    let req = json_request(
        "DELETE",
        &format!("/account-management/v1/tenants/{root}/users/{alice_id}"),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // GET /users?$filter=id eq <alice>&$top=1 — empty after delete.
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=id%20eq%20{alice_id}&limit=1"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert!(
        items.is_empty(),
        "alice MUST be gone after delete; got {items:?}",
    );

    // GET /users — bob still visible.
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/users"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "bob remains after alice's delete");
    assert_eq!(items[0]["username"], "bob");
}

// ─── Tenant existence ────────────────────────────────────────────────

#[tokio::test]
async fn list_users_for_unknown_tenant_returns_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let unknown = Uuid::new_v4();
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{unknown}/users"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ─── OData $filter / $orderby HTTP shape ─────────────────────────────

#[tokio::test]
async fn list_users_http_with_username_eq_filter_returns_200_and_invokes_plugin() {
    // The fake-side filter walker is id-eq-only; we don't assert on the
    // returned items here. The test pins the wire contract:
    // `?$filter=username eq 'alice'` parses, lowers to typed FilterNode,
    // and reaches the plugin (200, well-formed Page envelope).
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=username%20eq%20%27alice%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert!(
        body.get("items").is_some(),
        "response carries items[]: {body}"
    );
}

#[tokio::test]
async fn list_users_http_with_unknown_filter_field_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=foo%20eq%20%27x%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "unknown filter field MUST surface as 400"
    );
    let body = response_body(resp).await;
    // The canonical Problem envelope surfaces the bad-field detail in
    // `detail` and/or in `context.field_violations[].description`. The
    // current shape carries the "$filter: Unknown field: foo" string
    // inside the field-violation description; accept either location.
    let detail = body["detail"].as_str().unwrap_or("");
    let violations = body
        .get("context")
        .and_then(|c| c.get("field_violations"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let any_violation_mentions = violations.iter().any(|v| {
        let desc = v.get("description").and_then(|d| d.as_str()).unwrap_or("");
        desc.contains("foo") || desc.to_lowercase().contains("filter")
    });
    assert!(
        detail.contains("foo")
            || detail.to_lowercase().contains("filter")
            || any_violation_mentions,
        "Problem body should mention the bad field or $filter: {body}"
    );
}

#[tokio::test]
async fn list_users_http_with_substring_op_on_uuid_field_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    // `startswith(id, '11')` — id is kind=Uuid, substring ops only valid on String fields.
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=startswith%28id%2C%2711%27%29"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "substring op on Uuid field MUST surface as 400"
    );
}

#[tokio::test]
async fn list_users_http_with_string_value_on_uuid_field_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    // `?$filter=id eq 'abc'` — String value where Uuid is required.
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=id%20eq%20%27abc%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "string value on Uuid field MUST surface as 400"
    );
}

#[tokio::test]
async fn list_users_http_with_contains_first_name_returns_200() {
    // Wire-shape pin: case-insensitive `contains(first_name, 'ali')`
    // parses, lowers, and reaches the plugin. Actual case-insensitive
    // matching semantics are unit-tested in static-idp; the AM
    // integration FakeIdpPlugin is id-eq-only.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=contains%28first_name%2C%27ali%27%29"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_users_http_with_orderby_last_name_asc_returns_200() {
    // Wire-shape pin for $orderby. The fake plugin does not honour the
    // forwarded order beyond what its own list ordering produces; the
    // 200 + well-formed envelope confirms the route + extractor +
    // lowering pipeline.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24orderby=last_name%20asc"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_users_http_with_unknown_orderby_field_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24orderby=foo%20asc"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "unknown $orderby field MUST surface as 400"
    );
}

#[tokio::test]
async fn list_users_http_default_no_filter_no_orderby_returns_200() {
    // Plain `GET /users` (no $filter, no $orderby, no limit, no cursor)
    // must succeed and return a Page envelope. Regression guard against
    // a future extractor refactor that accidentally requires query params.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/users"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert!(body.get("items").is_some());
}

// ─── PATCH /tenants/{id}/users/{user_id} ─────────────────────────────

/// Provision `username` in `root` and return the IdP-assigned id from
/// the 201 response body.
async fn provision_and_id(router: &axum::Router, root: Uuid, body: serde_json::Value) -> Uuid {
    let req = json_request(
        "POST",
        &format!("/account-management/v1/tenants/{root}/users"),
        Some(body),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED, "provision precondition");
    let body = response_body(resp).await;
    Uuid::parse_str(body["id"].as_str().expect("id string")).expect("id uuid")
}

#[tokio::test]
async fn update_user_patches_attributes_returns_200() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let id = provision_and_id(&router, root, serde_json::json!({ "username": "alice" })).await;

    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{id}"),
        Some(serde_json::json!({ "email": "alice@example.com", "display_name": "Alice A." })),
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["id"].as_str().expect("id"), id.to_string());
    assert_eq!(body["username"], "alice");
    assert_eq!(body["email"], "alice@example.com");
    assert_eq!(body["display_name"], "Alice A.");

    // Confirm the mutation persisted at the IdP via the point-lookup.
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/users\
             ?%24filter=id%20eq%20{id}&limit=1"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["items"][0]["email"], "alice@example.com");
}

#[tokio::test]
async fn update_user_null_clears_nullable_field() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let id = provision_and_id(
        &router,
        root,
        serde_json::json!({ "username": "bob", "email": "bob@example.com", "display_name": "Bob" }),
    )
    .await;

    // JSON Merge Patch: explicit null clears `email`, `display_name`
    // omitted stays unchanged.
    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{id}"),
        Some(serde_json::json!({ "email": null })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert!(
        body.get("email").is_none() || body["email"].is_null(),
        "email MUST be cleared: {body}"
    );
    assert_eq!(body["display_name"], "Bob", "display_name MUST be retained");
}

#[tokio::test]
async fn update_user_rename_username_returns_200() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let id = provision_and_id(&router, root, serde_json::json!({ "username": "carol" })).await;

    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{id}"),
        Some(serde_json::json!({ "username": "carol2" })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["username"], "carol2");
    assert_eq!(
        body["id"].as_str().expect("id"),
        id.to_string(),
        "id is stable across rename"
    );
}

#[tokio::test]
async fn update_user_rename_collision_returns_409() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    provision_and_id(&router, root, serde_json::json!({ "username": "alice" })).await;
    let bob_id = provision_and_id(&router, root, serde_json::json!({ "username": "bob" })).await;

    // Rename bob → "alice" collides with the existing login.
    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{bob_id}"),
        Some(serde_json::json!({ "username": "alice" })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn update_user_unknown_user_returns_404() {
    // Unlike DELETE, a PATCH against an absent user is a 404 — the
    // provider's NotFound is NOT folded into success.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let ghost = Uuid::new_v4();
    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{ghost}"),
        Some(serde_json::json!({ "email": "ghost@example.com" })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_user_empty_patch_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let id = provision_and_id(&router, root, serde_json::json!({ "username": "dave" })).await;

    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{id}"),
        Some(serde_json::json!({})),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty patch MUST be 400");
}

#[tokio::test]
async fn update_user_null_username_returns_400() {
    // `username` is the required login identifier: an explicit null
    // (clear) is rejected at the wire boundary.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let id = provision_and_id(&router, root, serde_json::json!({ "username": "erin" })).await;

    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{id}"),
        Some(serde_json::json!({ "username": null })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "null username MUST be 400");
}

#[tokio::test]
async fn update_user_unknown_field_returns_400_or_422() {
    // `deny_unknown_fields` locks the wire envelope: a client that
    // PATCHes an immutable field (e.g. `id`) sees an explicit client
    // error rather than a silently-dropped mutation.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let id = provision_and_id(&router, root, serde_json::json!({ "username": "frank" })).await;

    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{id}"),
        Some(serde_json::json!({ "id": Uuid::new_v4() })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert!(
        matches!(
            resp.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "unknown field MUST surface as 400/422, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn update_user_for_unknown_tenant_returns_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let unknown = Uuid::new_v4();
    let some_user = Uuid::new_v4();
    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{unknown}/users/{some_user}"),
        Some(serde_json::json!({ "email": "x@example.com" })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    let (status, _body) = response_problem(resp).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_user_password_only_returns_200_and_never_echoes_password() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let router = build_users_router(&h);

    let id = provision_and_id(&router, root, serde_json::json!({ "username": "grace" })).await;

    let req = json_request(
        "PATCH",
        &format!("/account-management/v1/tenants/{root}/users/{id}"),
        Some(serde_json::json!({ "password": { "value": "s3cret!", "temporary": true } })),
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    assert_eq!(body["username"], "grace");
    assert!(
        body.get("password").is_none(),
        "password MUST NOT be echoed: {body}"
    );
}
