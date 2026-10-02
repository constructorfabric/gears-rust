//! HTTP-level E2E tests for the
//! `/account-management/v1/tenants/{tenant_id}/children` sub-resource.
//!
//! Pins the OData parsing seam (filter, orderby, limit clamp) flowing
//! through the real router into the service-side `list_children`
//! repository call.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::doc_markdown
)]

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use tower::ServiceExt;
use uuid::Uuid;

use common::*;

// ─── Happy path ──────────────────────────────────────────────────────

#[tokio::test]
async fn list_children_returns_200_with_page() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let c1 = Uuid::new_v4();
    let c2 = Uuid::new_v4();
    let c3 = Uuid::new_v4();
    seed_active_child(&h, c1, root, "alpha", 1).await;
    seed_active_child(&h, c2, root, "bravo", 1).await;
    seed_active_child(&h, c3, root, "charlie", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 3, "expected 3 children, got body={body}");
    let names: Vec<&str> = items
        .iter()
        .map(|m| m["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"bravo"));
    assert!(names.contains(&"charlie"));
}

// ─── Clamp / pagination ──────────────────────────────────────────────

#[tokio::test]
async fn list_children_clamps_top_to_service_max() {
    // The handler runs `clamp_listing_top(query, max_list_children_top())`
    // before the service call. With the default config (`max_top=200`)
    // a request that asks for half a million rows must be silently
    // clamped to 200, surfaced through the `page_info.limit` field on
    // the response.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let max_top = services.tenant_service.max_list_children_top();
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?$top=999999"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let limit = body["page_info"]["limit"]
        .as_u64()
        .expect("page_info.limit must be present and numeric");
    assert_eq!(
        u32::try_from(limit).expect("limit fits in u32"),
        max_top,
        "handler-side clamp must rewrite oversize $top to the service max",
    );
}

// ─── Filter / orderby ────────────────────────────────────────────────

#[tokio::test]
async fn list_children_filter_by_status_active() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let active = Uuid::new_v4();
    let suspended = Uuid::new_v4();
    seed_active_child(&h, active, root, "active", 1).await;
    insert_tenant(
        &h.provider,
        suspended,
        Some(root),
        "suspended",
        SUSPENDED,
        false,
        1,
    )
    .await
    .expect("seed suspended child");
    insert_closure(&h.provider, suspended, suspended, 0, SUSPENDED)
        .await
        .expect("seed suspended self-row");
    insert_closure(&h.provider, root, suspended, 0, SUSPENDED)
        .await
        .expect("seed (root, suspended) closure");

    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/children?%24filter=status%20eq%20%27active%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    // The filter narrows to active only; soft-deleted are excluded by
    // default anyway, suspended is filtered out by the explicit
    // `eq 'active'` (string contract — storage SMALLINT is impl-side).
    let statuses: Vec<&str> = items
        .iter()
        .map(|m| m["status"].as_str().expect("status"))
        .collect();
    assert!(
        statuses.iter().all(|s| *s == "active"),
        "filter=status eq 'active' must only surface active rows, got {statuses:?}",
    );
}

#[tokio::test]
async fn list_children_filter_by_status_deleted_surfaces_soft_deleted() {
    // The default list_children call hides `status=deleted` rows; the
    // `?$filter=status eq 'deleted'` opt-in surfaces them.
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let deleted = Uuid::new_v4();
    insert_tenant(&h.provider, deleted, Some(root), "gone", DELETED, false, 1)
        .await
        .expect("seed deleted child");
    insert_closure(&h.provider, deleted, deleted, 0, DELETED)
        .await
        .expect("seed deleted self-row");
    insert_closure(&h.provider, root, deleted, 0, DELETED)
        .await
        .expect("seed (root, deleted) closure");

    let services = build_services(&h);
    let router = build_test_router(&services);

    // Default list (no filter): deleted row is hidden.
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children"),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items");
    assert!(
        items.is_empty(),
        "default listing hides soft-deleted rows: {body}",
    );

    // Opt-in surface for deleted rows: `?$filter=status eq 'deleted'`.
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/children?%24filter=status%20eq%20%27deleted%27"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(
        items.len(),
        1,
        "deleted filter must surface the row: {body}"
    );
    assert_eq!(items[0]["status"], "deleted");
}

#[tokio::test]
async fn list_children_orderby_created_at_descending() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let c1 = Uuid::new_v4();
    let c2 = Uuid::new_v4();
    seed_active_child(&h, c1, root, "first", 1).await;
    // Force a non-trivial gap between created_at stamps by sleeping
    // briefly. `OffsetDateTime::now_utc()` is high-resolution on
    // modern systems but two same-microsecond inserts would defeat
    // the orderby assertion. Sleep is bounded; no risk of flakiness.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    seed_active_child(&h, c2, root, "second", 1).await;

    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?%24orderby=created_at%20desc"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    // Descending: latest first.
    assert_eq!(
        items[0]["id"],
        c2.to_string(),
        "orderby=created_at desc must surface the newest row first: {body}"
    );
    assert_eq!(items[1]["id"], c1.to_string());
}

// ─── Validation ──────────────────────────────────────────────────────

#[tokio::test]
async fn list_children_invalid_filter_syntax_returns_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?%24filter=garbage"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ─── recursive=true ──────────────────────────────────────────────────

/// `root ─ alpha ─ alpha-junior`, `root ─ charlie`. Returns
/// `(root, alpha, alpha_junior, charlie)`. Every row is typed with the
/// harness tenant type so `tenant_type` resolves on items and ancestors.
async fn seed_two_levels(h: &Harness) -> (Uuid, Uuid, Uuid, Uuid) {
    let root = Uuid::new_v4();
    seed_root(h, root).await;
    let alpha = Uuid::new_v4();
    let alpha_junior = Uuid::new_v4();
    let charlie = Uuid::new_v4();
    seed_active_child(h, alpha, root, "alpha", 1).await;
    seed_active_child(h, charlie, root, "charlie", 1).await;
    seed_active_child(h, alpha_junior, alpha, "alpha-junior", 2).await;
    // `seed_active_child` writes only `(parent, child)`; the grandchild
    // also needs `(root, grandchild)` to sit inside root's subtree scope.
    insert_closure(&h.provider, root, alpha_junior, 0, ACTIVE)
        .await
        .expect("seed (root, alpha-junior)");
    (root, alpha, alpha_junior, charlie)
}

#[tokio::test]
async fn list_children_recursive_returns_descendants_with_ancestors() {
    let h = setup_sqlite().await.expect("sqlite");
    let (root, alpha, alpha_junior, _charlie) = seed_two_levels(&h).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?recursive=true"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 3, "alpha, charlie, alpha-junior: body={body}");

    let by_name: HashMap<&str, &serde_json::Value> = items
        .iter()
        .map(|m| (m["name"].as_str().expect("name"), m))
        .collect();
    assert_eq!(by_name["alpha"]["ancestors"], serde_json::json!([]));
    assert_eq!(by_name["charlie"]["ancestors"], serde_json::json!([]));
    let chain = by_name["alpha-junior"]["ancestors"]
        .as_array()
        .expect("ancestors array");
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0]["id"].as_str().expect("id"), alpha.to_string());
    assert_eq!(chain[0]["name"], "alpha");
    assert!(
        chain[0]["tenant_type"].is_string(),
        "typed seed must resolve the ancestor type"
    );
    assert_eq!(
        by_name["alpha-junior"]["id"].as_str().expect("id"),
        alpha_junior.to_string()
    );
}

#[tokio::test]
async fn list_children_without_recursive_has_no_ancestors_key() {
    let h = setup_sqlite().await.expect("sqlite");
    let (root, ..) = seed_two_levels(&h).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "direct children only: body={body}");
    for item in items {
        assert!(
            item.as_object()
                .is_some_and(|o| !o.contains_key("ancestors")),
            "non-recursive items must not carry `ancestors`: {item}"
        );
    }
}

#[tokio::test]
async fn list_children_recursive_with_name_filter_matches_across_levels() {
    let h = setup_sqlite().await.expect("sqlite");
    let (root, alpha, _alpha_junior, _charlie) = seed_two_levels(&h).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    // `$filter=contains(name,'junior')` — comma and quotes percent-encoded.
    let req = json_request(
        "GET",
        &format!(
            "/account-management/v1/tenants/{root}/children?recursive=true&%24filter=contains(name%2C%27junior%27)"
        ),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "body={body}");
    assert_eq!(items[0]["name"], "alpha-junior");
    assert_eq!(
        items[0]["ancestors"][0]["id"].as_str().expect("id"),
        alpha.to_string()
    );
}

#[tokio::test]
async fn list_children_recursive_rejects_invalid_flag_with_400() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    for bad in ["maybe", "True", "1"] {
        let req = json_request(
            "GET",
            &format!("/account-management/v1/tenants/{root}/children?recursive={bad}"),
            None,
            ctx_for(root),
        );
        let resp = router.clone().oneshot(req).await.expect("router");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "recursive={bad}");
    }
}

#[tokio::test]
async fn list_children_recursive_false_equals_absent() {
    let h = setup_sqlite().await.expect("sqlite");
    let (root, ..) = seed_two_levels(&h).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?recursive=false"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(
        items.len(),
        2,
        "explicit false is the direct listing: body={body}"
    );
    assert!(items.iter().all(|i| i.get("ancestors").is_none()));
}

#[tokio::test]
async fn list_children_recursive_clamps_oversized_top_to_service_max() {
    // Same clamp seam as `list_children_clamps_top_to_service_max`, on
    // the recursive branch: `$top=999999` must come back as the
    // operator cap in `page_info.limit`.
    let h = setup_sqlite().await.expect("sqlite");
    let (root, ..) = seed_two_levels(&h).await;
    let services = build_services(&h);
    let max_top = services.tenant_service.max_list_children_top();
    let router = build_test_router(&services);

    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?recursive=true&$top=999999"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    let limit = body["page_info"]["limit"]
        .as_u64()
        .expect("page_info.limit");
    assert_eq!(u32::try_from(limit).expect("fits"), max_top);
}

#[tokio::test]
async fn list_children_recursive_requires_the_list_children_action() {
    // `mock_enforcer` permits everything; only an action-aware enforcer
    // proves the recursive branch is gated by `list_children` and not,
    // say, by `read`.
    let h = setup_sqlite().await.expect("sqlite");
    let (root, ..) = seed_two_levels(&h).await;

    let denied = build_test_router(&build_services_with_tenant_enforcer(
        &h,
        enforcer_allowing(&["read"]),
    ));
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?recursive=true"),
        None,
        ctx_for(root),
    );
    let resp = denied.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = response_body(resp).await;
    assert_eq!(
        body["context"]["reason"], "CROSS_TENANT_DENIED",
        "body={body}"
    );

    let granted = build_test_router(&build_services_with_tenant_enforcer(
        &h,
        enforcer_allowing(&["list_children"]),
    ));
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?recursive=true"),
        None,
        ctx_for(root),
    );
    let resp = granted.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_children_recursive_unknown_root_is_404() {
    let h = setup_sqlite().await.expect("sqlite");
    let root = Uuid::new_v4();
    seed_root(&h, root).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let ghost = Uuid::new_v4();
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{ghost}/children?recursive=true"),
        None,
        ctx_for(root),
    );
    let resp = router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Fetch the first page at `limit=1` and return its `next_cursor`.
async fn first_cursor(router: &axum::Router, root: Uuid, recursive: bool) -> String {
    let flag = if recursive { "&recursive=true" } else { "" };
    let req = json_request(
        "GET",
        &format!("/account-management/v1/tenants/{root}/children?limit=1{flag}"),
        None,
        ctx_for(root),
    );
    let resp = router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_body(resp).await;
    body["page_info"]["next_cursor"]
        .as_str()
        .expect("more rows remain")
        .to_owned()
}

#[tokio::test]
async fn list_children_cursor_is_bound_to_the_mode_it_was_minted_in() {
    // Both modes sort by `(created_at, id)`, so a cursor replayed in the
    // other mode would silently skip rows; it must be a 400 instead.
    let h = setup_sqlite().await.expect("sqlite");
    let (root, ..) = seed_two_levels(&h).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let recursive_cursor = first_cursor(&router, root, true).await;
    let direct_cursor = first_cursor(&router, root, false).await;

    let send = |cursor: &str, recursive: bool| {
        let flag = if recursive { "&recursive=true" } else { "" };
        json_request(
            "GET",
            &format!(
                "/account-management/v1/tenants/{root}/children?limit=1&cursor={cursor}{flag}"
            ),
            None,
            ctx_for(root),
        )
    };

    let same = router
        .clone()
        .oneshot(send(&recursive_cursor, true))
        .await
        .expect("router");
    assert_eq!(same.status(), StatusCode::OK, "same mode keeps paging");

    let cross = router
        .clone()
        .oneshot(send(&recursive_cursor, false))
        .await
        .expect("router");
    assert_eq!(
        cross.status(),
        StatusCode::BAD_REQUEST,
        "recursive cursor in direct mode"
    );

    let cross_back = router
        .clone()
        .oneshot(send(&direct_cursor, true))
        .await
        .expect("router");
    assert_eq!(
        cross_back.status(),
        StatusCode::BAD_REQUEST,
        "direct cursor in recursive mode"
    );
}

#[tokio::test]
async fn list_children_legacy_cursor_is_rejected_only_in_recursive_mode() {
    // A direct-listing cursor issued before mode binding has no filter
    // fingerprint. Pagination skips a missing fingerprint, so recursive
    // mode must reject it itself; the direct listing keeps accepting it.
    let h = setup_sqlite().await.expect("sqlite");
    let (root, ..) = seed_two_levels(&h).await;
    let services = build_services(&h);
    let router = build_test_router(&services);

    let minted = first_cursor(&router, root, false).await;
    let mut cursor = toolkit_odata::CursorV1::decode(&minted).expect("decode");
    cursor.f = None;
    let legacy = cursor.encode().expect("encode");

    let send = |recursive: bool| {
        let flag = if recursive { "&recursive=true" } else { "" };
        json_request(
            "GET",
            &format!(
                "/account-management/v1/tenants/{root}/children?limit=1&cursor={legacy}{flag}"
            ),
            None,
            ctx_for(root),
        )
    };
    let recursive = router.clone().oneshot(send(true)).await.expect("router");
    assert_eq!(recursive.status(), StatusCode::BAD_REQUEST);
    let direct = router.clone().oneshot(send(false)).await.expect("router");
    assert_eq!(
        direct.status(),
        StatusCode::OK,
        "legacy cursors keep working in direct mode"
    );
}
