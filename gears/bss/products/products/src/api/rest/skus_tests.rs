#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::router;
use crate::api::rest::{ApiState, categories};
use crate::domain::{recognized::UsageTypeAnswer, references::RefKind};
use crate::infra::storage::repo;
use crate::test_support::{
    StubUsageTypes, at, authed_ctx, body_json, get, patch, post, problem_code, raw_i64,
    repo_connection, request_as, rest_app, rest_app_with_catalog, violation_for,
};
use axum::{Router, http::StatusCode};
use bss_products_sdk::models::{Lifecycle, SkuContent};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::api::OpenApiRegistry;
use uuid::Uuid;

fn doors(s: Arc<ApiState>, o: &dyn OpenApiRegistry) -> Router {
    categories::router(Arc::clone(&s), o).merge(router(s, o))
}
async fn category(app: &Router, tenant: Uuid) -> Uuid {
    let r = post(
        app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code":"hosting","name":"Hosting"}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    serde_json::from_value(body_json(r).await["id"].clone()).unwrap()
}
fn new(cat: Uuid, code: &str, name: &str) -> Value {
    json!({"code":code,"name":name,"type":"usage","category_id":cat})
}

#[tokio::test]
async fn create_read_patch_and_duplicates_keep_etags_and_codes() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/skus",
        new(cat, "STOR", "Storage"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let tag = r.headers()["etag"].to_str().unwrap().to_owned();
    let s = body_json(r).await;
    assert_eq!(s["lifecycle"], "draft");
    assert_eq!(s["type"], "usage");
    assert!(s["created_at"].as_str().unwrap().contains('T'));
    let url = format!("/bss-products/v1/skus/{}", s["id"].as_str().unwrap());
    let r = get(&app, tenant, &url).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["etag"], tag);
    assert_eq!(body_json(r).await["sku"], s);
    for (code, name, reason) in [
        ("STOR", "Other", "SKU_CODE_TAKEN"),
        ("OTHER", "Storage", "SKU_NAME_TAKEN"),
    ] {
        let r = post(&app, tenant, "/bss-products/v1/skus", new(cat, code, name)).await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(problem_code(&body_json(r).await), reason);
    }
    let r = patch(&app, tenant, &url, json!({"name":"Renamed"}), None).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert!(violation_for(&body_json(r).await, "If-Match").is_some());
    let r = patch(
        &app,
        tenant,
        &url,
        json!({"name":"Renamed"}),
        Some("\"99\""),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "STALE_REVISION");
    let r = patch(
        &app,
        tenant,
        &url,
        json!({"type":"recurring","gl_code":"4010","billing_timing":"advance"}),
        Some(&tag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let tag2 = r.headers()["etag"].to_str().unwrap().to_owned();
    let s = body_json(r).await;
    assert_eq!(s["type"], "recurring");
    assert_eq!(s["type_change_pending"], false);
    assert_eq!(s["billing_timing"], "advance");
    let r = patch(
        &app,
        tenant,
        &url,
        json!({"gl_code":null,"billing_timing":null}),
        Some(&tag2),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let s = body_json(r).await;
    assert_eq!(s["gl_code"], Value::Null);
    assert_eq!(s["billing_timing"], Value::Null);
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE action LIKE 'sku.%'"
        )
        .await,
        3
    );
    assert_eq!(
        get(&app, Uuid::new_v4(), &url).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn retired_missing_and_foreign_categories_refuse_assignment() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    assert_eq!(
        post(
            &app,
            tenant,
            &format!("/bss-products/v1/categories/{cat}/retire"),
            json!({})
        )
        .await
        .status(),
        StatusCode::OK
    );
    let r = post(&app, tenant, "/bss-products/v1/skus", new(cat, "A", "A")).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_RETIRED");
    assert_eq!(
        post(
            &app,
            tenant,
            "/bss-products/v1/skus",
            new(Uuid::new_v4(), "A", "A")
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let foreign = Uuid::new_v4();
    let (db, scope) = repo_connection(&dsn, foreign).await;
    let foreign_cat = repo::insert_category(
        &db.conn().unwrap(),
        &scope,
        foreign,
        crate::domain::category::NewCategory {
            code: "foreign".into(),
            name: "Foreign".into(),
            is_default: false,
            sort_order: 0,
        },
        at(9),
    )
    .await
    .unwrap();
    assert_eq!(
        post(
            &app,
            tenant,
            "/bss-products/v1/skus",
            new(foreign_cat.id, "A", "A")
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn published_and_locked_drafts_refuse_direct_edits() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let (db, scope) = repo_connection(&dsn, tenant).await;
    let conn = db.conn().unwrap();
    let s = crate::test_support::seed_rest_sku(&conn, &scope, tenant, cat, "A").await;
    repo::set_lifecycle(
        &conn,
        &scope,
        tenant,
        s.id,
        &[Lifecycle::Draft],
        Lifecycle::Published,
        at(9),
    )
    .await
    .unwrap();
    let r = patch(
        &app,
        tenant,
        &format!("/bss-products/v1/skus/{}", s.id),
        json!({"name":"changed"}),
        Some("\"2\""),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let b = body_json(r).await;
    assert_eq!(problem_code(&b), "NOT_A_DRAFT");
    assert!(b.to_string().contains("/changes"));
    let s = crate::test_support::seed_rest_sku(&conn, &scope, tenant, cat, "B").await;
    assert!(
        repo::try_lock_sku(&conn, &scope, tenant, s.id, Uuid::new_v4(), s.revision)
            .await
            .unwrap()
    );
    let r = patch(
        &app,
        tenant,
        &format!("/bss-products/v1/skus/{}", s.id),
        json!({"name":"changed"}),
        Some("\"1\""),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "ROW_LOCKED_PENDING");
}

#[tokio::test]
async fn card_and_reference_details_read_the_live_registry() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let (db, scope) = repo_connection(&dsn, tenant).await;
    let conn = db.conn().unwrap();
    let s = crate::test_support::seed_rest_sku(&conn, &scope, tenant, cat, "A").await;
    repo::set_lifecycle(
        &conn,
        &scope,
        tenant,
        s.id,
        &[Lifecycle::Draft],
        Lifecycle::Published,
        at(9),
    )
    .await
    .unwrap();
    for (i, kind) in [
        RefKind::PriceBookEntry,
        RefKind::PriceBookEntry,
        RefKind::PlanItem,
    ]
    .into_iter()
    .enumerate()
    {
        let r = repo::reserve_reference(
            &conn,
            &scope,
            tenant,
            s.id,
            "pricing",
            kind,
            Uuid::new_v4(),
            tenant,
            at(9),
        )
        .await
        .unwrap();
        if i < 2 {
            repo::confirm_reference(&conn, &scope, tenant, r.id, at(10))
                .await
                .unwrap();
        }
    }
    let released = repo::reserve_reference(
        &conn,
        &scope,
        tenant,
        s.id,
        "pricing",
        RefKind::SoldAs,
        Uuid::new_v4(),
        tenant,
        at(9),
    )
    .await
    .unwrap();
    repo::release_reference(
        &conn,
        &scope,
        tenant,
        released.id,
        tenant,
        Some("abandoned attempt"),
        true,
        at(11),
    )
    .await
    .unwrap();
    let url = format!("/bss-products/v1/skus/{}", s.id);
    let card = body_json(get(&app, tenant, &url).await).await;
    assert_eq!(
        card["references"],
        json!({"price_book_entries":2,"plans":1,"reserved":1,"by_owner":{"pricing":{"price_book_entry":2,"plan_item":1,"reserved":1}}})
    );
    let refs = body_json(get(&app, tenant, &format!("{url}/references")).await).await;
    assert_eq!(refs["summary"], card["references"]);
    assert_eq!(refs["items"].as_array().unwrap().len(), 3);
    for item in refs["items"].as_array().unwrap() {
        assert_eq!(item["owner"], "pricing");
        assert!(item["reserved_at"].as_str().unwrap().contains('T'));
    }
    assert_eq!(
        get(&app, Uuid::new_v4(), &format!("{url}/references"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let history_url = format!("{url}/references?include_released=true");
    let history = body_json(get(&app, tenant, &history_url).await).await;
    assert_eq!(history["summary"], card["references"]);
    assert_eq!(history["items"].as_array().unwrap().len(), 4);
    let row = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == released.id.to_string())
        .unwrap();
    assert_eq!(row["state"], "released");
    assert_eq!(row["released_by"], tenant.to_string());
    assert_eq!(row["release_reason"], "abandoned attempt");
    assert_eq!(row["forced"], true);
    assert!(row["released_at"].as_str().unwrap().contains('T'));
    assert_eq!(
        get(&app, Uuid::new_v4(), &history_url).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(
            &app,
            tenant,
            &format!("{url}/references?include_released=invalid")
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    let live = body_json(
        get(
            &app,
            tenant,
            &format!("{url}/references?include_released=false"),
        )
        .await,
    )
    .await;
    assert_eq!(live["items"].as_array().unwrap().len(), 3);
}

/// P-D-214: the versions answer one shape each — the history always an array, the version in force
/// on a date one object at its own path.
#[tokio::test]
async fn versions_answer_an_array_and_as_of_answers_the_one_in_force() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let (db, scope) = repo_connection(&dsn, tenant).await;
    let conn = db.conn().unwrap();
    let s = crate::test_support::seed_rest_sku(&conn, &scope, tenant, cat, "A").await;
    let mut content = SkuContent::from(&s);
    for (v, day) in [(1, 2), (2, 20), (3, 20)] {
        content.gl_code = Some(format!("40{v}"));
        repo::append_version(
            &conn,
            &scope,
            tenant,
            s.id,
            v,
            crate::test_support::utc(2026, 9, day, 0, 0, 0).date(),
            &content,
            at(9),
        )
        .await
        .unwrap();
    }
    let url = format!("/bss-products/v1/skus/{}/versions", s.id);
    let r = get(&app, tenant, &format!("{url}/as-of?date=2026-09-15")).await;
    assert_eq!(r.status(), StatusCode::OK);
    let v = body_json(r).await;
    assert_eq!(v["published_version"], 1);
    assert_eq!(v["effective_from"], "2026-09-02");
    assert_eq!(v["content"]["gl_code"], "401");
    assert_eq!(
        body_json(get(&app, tenant, &format!("{url}/as-of?date=2026-09-20")).await).await["published_version"],
        3
    );
    let history = body_json(get(&app, tenant, &url).await).await;
    let versions: Vec<&Value> = history.as_array().unwrap().iter().collect();
    assert_eq!(
        versions
            .iter()
            .map(|v| &v["published_version"])
            .collect::<Vec<_>>(),
        [1, 2, 3],
        "{history}"
    );
    let r = get(&app, tenant, &format!("{url}/as-of?date=2026-09-01")).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert_eq!(problem_code(&body_json(r).await), "NO_VERSION_IN_FORCE");
    for query in [
        "/as-of?date=bad",
        "/as-of",
        "/as-of?date=",
        "/as-of?date=2026-09-15&as_of=2026-09-15",
        "/as-of?date=2026-09-15&date=2026-09-16",
        // The old spelling is refused, never answered with the history's array.
        "?as_of=2026-09-15",
        "?limit=1",
    ] {
        let r = get(&app, tenant, &format!("{url}{query}")).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{query}");
    }
    for query in ["", "/as-of?date=2026-09-15"] {
        assert_eq!(
            get(&app, Uuid::new_v4(), &format!("{url}{query}"))
                .await
                .status(),
            StatusCode::NOT_FOUND,
            "another tenant: {query}"
        );
    }
    // A SKU never published has no version: the history is an empty array, never an object.
    let draft = crate::test_support::seed_rest_sku(&conn, &scope, tenant, cat, "B").await;
    let empty = body_json(
        get(
            &app,
            tenant,
            &format!("/bss-products/v1/skus/{}/versions", draft.id),
        )
        .await,
    )
    .await;
    assert_eq!(empty, serde_json::json!([]));
}

#[tokio::test]
async fn filtered_code_cursor_never_skips_the_first_row_of_the_next_page() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let (db, scope) = repo_connection(&dsn, tenant).await;
    let conn = db.conn().unwrap();
    for code in ["stor-c", "stor-a", "stor-b", "other"] {
        let s = crate::test_support::seed_rest_sku(&conn, &scope, tenant, cat, code).await;
        repo::set_lifecycle(
            &conn,
            &scope,
            tenant,
            s.id,
            &[Lifecycle::Draft],
            Lifecycle::Published,
            at(9),
        )
        .await
        .unwrap();
    }
    // P-D-210: the filters are OData now; the cursor is the pager's, and carries the filter.
    let filter = format!(
        "type%20eq%20%27usage%27%20and%20lifecycle%20eq%20%27published%27%20and%20category_id%20eq%20{cat}"
    );
    let query = format!("/bss-products/v1/skus?q=stor&%24filter={filter}&limit=2");
    let page = body_json(get(&app, tenant, &query).await).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["code"], "stor-a");
    assert_eq!(page["items"][1]["code"], "stor-b");
    let next = page["page_info"]["next_cursor"].as_str().unwrap();
    let page = body_json(
        get(
            &app,
            tenant,
            &format!("/bss-products/v1/skus?q=stor&%24filter={filter}&limit=2&cursor={next}"),
        )
        .await,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1, "{page}");
    assert_eq!(page["items"][0]["code"], "stor-c");
    assert_eq!(page["page_info"]["next_cursor"], Value::Null);
}

/// P-D-259: a raw ref is 400 `DERIVED_USAGE_TYPE_REQUIRED` before the catalog is asked, whether
/// the catalog would resolve it, refuse it, or is unconfigured.
#[tokio::test]
async fn a_raw_usage_ref_is_refused_before_the_catalog() {
    for (answer, source) in [
        (UsageTypeAnswer::Unavailable, "test"),
        (UsageTypeAnswer::Unresolved, "test"),
        (UsageTypeAnswer::Unresolved, "unconfigured"),
    ] {
        let tenant = Uuid::new_v4();
        let catalog = Arc::new(StubUsageTypes::always(answer));
        let (app, dsn) = rest_app_with_catalog(tenant, doors, catalog.clone(), source).await;
        let cat = category(&app, tenant).await;
        let mut body = new(cat, "A", "A");
        body["usage_type_ref"] = json!("usage:new");
        let r = post(&app, tenant, "/bss-products/v1/skus", body).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let body = body_json(r).await;
        assert_eq!(problem_code(&body), "DERIVED_USAGE_TYPE_REQUIRED");
        assert!(violation_for(&body, "usage_type_ref").is_some());
        let s =
            body_json(post(&app, tenant, "/bss-products/v1/skus", new(cat, "B", "B")).await).await;
        let url = format!("/bss-products/v1/skus/{}", s["id"].as_str().unwrap());
        let r = patch(
            &app,
            tenant,
            &url,
            json!({"usage_type_ref":"usage:new"}),
            Some("\"1\""),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            problem_code(&body_json(r).await),
            "DERIVED_USAGE_TYPE_REQUIRED"
        );
        assert_eq!(
            raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_sku").await,
            1
        );
        assert_eq!(catalog.asked.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn invalid_enum_fields_and_lifecycle_edits_are_400_and_audit_failure_rolls_back() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let s = body_json(post(&app, tenant, "/bss-products/v1/skus", new(cat, "A", "A")).await).await;
    let url = format!("/bss-products/v1/skus/{}", s["id"].as_str().unwrap());
    for field in ["type", "lifecycle", "billing_timing"] {
        let r = patch(&app, tenant, &url, json!({field:"bad"}), Some("\"1\"")).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert!(violation_for(&body_json(r).await, field).is_some());
    }
    let r = patch(
        &app,
        tenant,
        &url,
        json!({"lifecycle":"published"}),
        Some("\"1\""),
    )
    .await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert!(violation_for(&body_json(r).await, "lifecycle").is_some());
    assert_eq!(
        get(
            &app,
            tenant,
            "/bss-products/v1/skus?%24filter=type%20eq%20%27bad%27"
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&app, tenant, "/bss-products/v1/skus?limit=0")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let r = post(&app, tenant, "/bss-products/v1/skus", json!({"type":42})).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    crate::test_support::drop_table(&dsn, "products_audit_log").await;
    let r = patch(
        &app,
        tenant,
        &url,
        json!({"name":"rollback"}),
        Some("\"1\""),
    )
    .await;
    assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_json(get(&app, tenant, &url).await).await["sku"]["name"],
        "A"
    );
}

#[tokio::test]
async fn a_raw_ref_on_a_draft_patch_is_refused_without_asking_the_catalog() {
    let tenant = Uuid::new_v4();
    let catalog = Arc::new(StubUsageTypes::scripted([
        UsageTypeAnswer::Resolved(crate::test_support::probe_binding()),
        UsageTypeAnswer::Unresolved,
    ]));
    let (app, _) = rest_app_with_catalog(tenant, doors, catalog.clone(), "test").await;
    let cat = category(&app, tenant).await;
    let s = body_json(post(&app, tenant, "/bss-products/v1/skus", new(cat, "A", "A")).await).await;
    let url = format!("/bss-products/v1/skus/{}", s["id"].as_str().unwrap());
    let r = patch(
        &app,
        tenant,
        &url,
        json!({"description":"changed"}),
        Some("\"1\""),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let r = patch(
        &app,
        tenant,
        &url,
        json!({"usage_type_ref":"usage:new"}),
        Some("\"2\""),
    )
    .await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        problem_code(&body_json(r).await),
        "DERIVED_USAGE_TYPE_REQUIRED"
    );
    assert_eq!(catalog.asked.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reassignment_and_rename_apply_the_same_category_and_uniqueness_guards() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let first =
        body_json(post(&app, tenant, "/bss-products/v1/skus", new(cat, "A", "A")).await).await;
    post(&app, tenant, "/bss-products/v1/skus", new(cat, "B", "B")).await;
    let other = body_json(
        post(
            &app,
            tenant,
            "/bss-products/v1/categories",
            json!({"code":"other", "name":"Other"}),
        )
        .await,
    )
    .await;
    let other_id = other["id"].as_str().unwrap();
    assert_eq!(
        post(
            &app,
            tenant,
            &format!("/bss-products/v1/categories/{other_id}/retire"),
            json!({})
        )
        .await
        .status(),
        StatusCode::OK
    );
    let url = format!("/bss-products/v1/skus/{}", first["id"].as_str().unwrap());
    for (body, status, code) in [
        (
            json!({"category_id":other_id}),
            StatusCode::CONFLICT,
            Some("CATEGORY_RETIRED"),
        ),
        (
            json!({"category_id":Uuid::new_v4()}),
            StatusCode::NOT_FOUND,
            None,
        ),
        (
            json!({"name":"B"}),
            StatusCode::CONFLICT,
            Some("SKU_NAME_TAKEN"),
        ),
    ] {
        let r = patch(&app, tenant, &url, body, Some("\"1\"")).await;
        assert_eq!(r.status(), status);
        if let Some(code) = code {
            assert_eq!(problem_code(&body_json(r).await), code);
        }
    }
    let current = body_json(get(&app, tenant, &url).await).await;
    assert_eq!(current["sku"]["revision"], 1);
    assert_eq!(current["sku"]["category_id"], json!(cat));
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE action = 'sku.draft_update'"
        )
        .await,
        0
    );
}

// D-404 (review chains MEDIUM-1 = docs F2): a SKU draft belongs to its author. Another author,
// who may also approve, cannot rewrite it and then approve it as someone else's work.
#[tokio::test]
async fn only_the_drafts_author_may_patch_it() {
    let tenant = Uuid::new_v4();
    let (app, _dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/skus",
        new(cat, "STOR", "Storage"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let tag = r.headers()["etag"].to_str().unwrap().to_owned();
    let url = format!(
        "/bss-products/v1/skus/{}",
        body_json(r).await["id"].as_str().unwrap()
    );
    let bob = authed_ctx(tenant);
    let r = request_as(
        &app,
        &bob,
        axum::http::Method::PATCH,
        &url,
        Some(json!({"name":"Bob's"})),
        Some(&tag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(problem_code(&body_json(r).await), "NOT_DRAFT_AUTHOR");
    assert_eq!(
        body_json(get(&app, tenant, &url).await).await["sku"]["name"],
        "Storage"
    );
    let r = patch(&app, tenant, &url, json!({"name":"Renamed"}), Some(&tag)).await;
    assert_eq!(r.status(), StatusCode::OK, "the author still edits");
}

/// P-D-196: an omitted `category_id` stays null, even when the tenant has a default category
/// (no fallback to `is_default`), and so does an explicit null.
#[tokio::test]
async fn a_sku_without_a_category_is_created_with_null_and_no_default_fallback() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code":"default","name":"Default","is_default":true}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    for (code, body) in [
        (
            "OMITTED",
            json!({"code":"OMITTED","name":"Omitted","type":"recurring"}),
        ),
        (
            "NULL",
            json!({"code":"NULL","name":"Null","type":"recurring","category_id":null}),
        ),
    ] {
        let r = post(&app, tenant, "/bss-products/v1/skus", body).await;
        assert_eq!(r.status(), StatusCode::CREATED, "{code}");
        let s = body_json(r).await;
        assert_eq!(s["category_id"], Value::Null, "{code}: {s}");
        let url = format!("/bss-products/v1/skus/{}", s["id"].as_str().unwrap());
        let card = body_json(get(&app, tenant, &url).await).await;
        assert_eq!(card["sku"]["category_id"], Value::Null, "{code}");
        assert_eq!(
            raw_i64(
                &dsn,
                &format!(
                    "SELECT COUNT(*) AS v FROM products_sku WHERE code = '{code}' AND category_id IS NULL"
                )
            )
            .await,
            1,
            "{code}"
        );
    }
}

/// P-D-196, plan review L13: the draft PATCH clears the category with an explicit null
/// (`double_option`); an omitted field keeps it, and a category can be set again.
#[tokio::test]
async fn the_draft_patch_clears_the_category_with_null_and_an_omitted_field_keeps_it() {
    let tenant = Uuid::new_v4();
    let (app, _dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let r = post(&app, tenant, "/bss-products/v1/skus", new(cat, "A", "A")).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let tag = r.headers()["etag"].to_str().unwrap().to_owned();
    let url = format!(
        "/bss-products/v1/skus/{}",
        body_json(r).await["id"].as_str().unwrap()
    );
    let r = patch(&app, tenant, &url, json!({"name":"Renamed"}), Some(&tag)).await;
    assert_eq!(r.status(), StatusCode::OK);
    let tag = r.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(
        body_json(r).await["category_id"],
        json!(cat),
        "omitted keeps it"
    );
    let r = patch(&app, tenant, &url, json!({"category_id":null}), Some(&tag)).await;
    assert_eq!(r.status(), StatusCode::OK);
    let tag = r.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(
        body_json(r).await["category_id"],
        Value::Null,
        "null clears it"
    );
    assert_eq!(
        body_json(get(&app, tenant, &url).await).await["sku"]["category_id"],
        Value::Null
    );
    let r = patch(&app, tenant, &url, json!({"category_id":cat}), Some(&tag)).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(body_json(r).await["category_id"], json!(cat), "set again");
}

/// P-D-196: browse by a category matches only that category's SKUs, so a SKU without a category
/// never matches it; the unfiltered list includes it.
#[tokio::test]
async fn browse_by_a_category_excludes_skus_without_one_and_the_unfiltered_list_includes_them() {
    let tenant = Uuid::new_v4();
    let (app, _dsn) = rest_app(tenant, doors).await;
    let cat = category(&app, tenant).await;
    let r = post(&app, tenant, "/bss-products/v1/skus", new(cat, "A", "A")).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/skus",
        json!({"code":"B","name":"B","type":"usage"}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let codes = |list: Value| -> Vec<String> {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["code"].as_str().unwrap().to_owned())
            .collect()
    };
    let by_category = body_json(
        get(
            &app,
            tenant,
            &format!("/bss-products/v1/skus?%24filter=category_id%20eq%20{cat}"),
        )
        .await,
    )
    .await;
    assert_eq!(codes(by_category), ["A"]);
    // P-D-196's owed `category=none` browse is `category_id eq null` (P-D-210).
    let without = body_json(
        get(
            &app,
            tenant,
            "/bss-products/v1/skus?%24filter=category_id%20eq%20null",
        )
        .await,
    )
    .await;
    assert_eq!(codes(without), ["B"]);
    let all = body_json(get(&app, tenant, "/bss-products/v1/skus").await).await;
    assert_eq!(codes(all), ["A", "B"]);
}

// ------------------------------------------------------------------ P-D-197: pricing's usage

/// What the scripted port does when it is asked.
#[derive(Clone, Copy)]
enum PortAnswer {
    /// The counts the test set, for the ids it knows.
    Counts,
    /// 403: the caller holds no pricing `price_book_entry:read`.
    Refuses,
    /// 503: pricing cannot answer.
    Fails,
    /// The call never finishes as an answer.
    Panics,
    /// The call never returns at all.
    Hangs,
}
/// Pricing's port as a counting double: every call's tenant and ids, in call order.
struct UsagePort {
    answer: PortAnswer,
    counts:
        std::sync::Mutex<std::collections::BTreeMap<Uuid, bss_products_sdk::sku_usage::SkuUsage>>,
    calls: std::sync::Mutex<Vec<(Uuid, Vec<Uuid>)>>,
    /// Hanging calls whose future was dropped: the caller aborted them.
    abandoned: std::sync::atomic::AtomicUsize,
}
/// Counts one abandoned call when the hanging call's future is dropped.
struct Abandoned<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for Abandoned<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl UsagePort {
    fn new(answer: PortAnswer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            counts: std::sync::Mutex::default(),
            calls: std::sync::Mutex::default(),
            abandoned: std::sync::atomic::AtomicUsize::default(),
        })
    }
    fn set(&self, usage: bss_products_sdk::sku_usage::SkuUsage) {
        self.counts.lock().unwrap().insert(usage.sku_id, usage);
    }
    fn calls(&self) -> Vec<(Uuid, Vec<Uuid>)> {
        self.calls.lock().unwrap().clone()
    }
}
#[async_trait::async_trait]
impl bss_products_sdk::sku_usage::SkuUsageV1 for UsagePort {
    async fn usage(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        tenant: Uuid,
        sku_ids: &[Uuid],
    ) -> Result<
        Vec<bss_products_sdk::sku_usage::SkuUsage>,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        self.calls.lock().unwrap().push((tenant, sku_ids.to_vec()));
        match self.answer {
            PortAnswer::Counts => {
                let counts = self.counts.lock().unwrap();
                Ok(sku_ids
                    .iter()
                    .filter_map(|id| counts.get(id).cloned())
                    .collect())
            }
            PortAnswer::Refuses => Err(bss_products_sdk::sku_usage::sku_usage_denied()),
            PortAnswer::Fails => Err(bss_products_sdk::sku_usage::sku_usage_unavailable(
                "pricing is down",
            )),
            PortAnswer::Panics => panic!("the SKU usage port broke"),
            PortAnswer::Hangs => {
                let _abandoned = Abandoned(&self.abandoned);
                std::future::pending().await
            }
        }
    }
    /// The SKU reads here never filter by usage: a call is a defect of the read.
    async fn usage_sets(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant: Uuid,
    ) -> Result<
        bss_products_sdk::sku_usage::SkuUsageSets,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        panic!("a SKU read without a usage filter asked for the usage sets")
    }
    /// The SKU reads here take no picker key: a call is a defect of the read.
    async fn sku_ids_in(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _tenant: Uuid,
        _scope: bss_products_sdk::sku_usage::UsageScope,
    ) -> Result<Vec<Uuid>, toolkit::api::canonical_prelude::CanonicalError> {
        panic!("a SKU read asked for a picker scope")
    }
}
/// A router and its state over a fresh database, with no usage port registered.
async fn usage_app(tenant: Uuid) -> (Router, Arc<ApiState>) {
    let (db, _, _, dsn) = crate::test_support::test_db().await;
    let (app, state) = crate::test_support::rest_app_on_db(
        tenant,
        doors,
        crate::test_support::resolved_usage_types(),
        "test",
        db,
    )
    .await;
    // The router holds the database's temporary directory.
    (app.layer(axum::Extension(dsn)), state)
}
/// A category-less draft SKU through the door (P-D-196): its id.
async fn sku_named(app: &Router, tenant: Uuid, code: &str) -> Uuid {
    let r = post(
        app,
        tenant,
        "/bss-products/v1/skus",
        json!({"code":code,"name":code,"type":"recurring"}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    serde_json::from_value(body_json(r).await["id"].clone()).unwrap()
}
fn counts(sku_id: Uuid, entries: u64, plans: u64) -> bss_products_sdk::sku_usage::SkuUsage {
    bss_products_sdk::sku_usage::SkuUsage {
        sku_id,
        entries,
        currencies: vec!["EUR".into(), "USD".into()],
        prices: bss_products_sdk::sku_usage::PriceCounts {
            approved: 3,
            pending: 1,
            draft: 2,
        },
        plans,
    }
}

/// P-D-197: with pricing's port registered, the card and every list item carry its usage, and
/// the list asks the port once per page with the ids of that page.
#[tokio::test]
async fn the_sku_reads_carry_pricing_usage_with_one_port_call_per_list_page() {
    use bss_products_sdk::sku_usage::SkuUsageV1;
    let tenant = Uuid::new_v4();
    let (app, state) = usage_app(tenant).await;
    let a = sku_named(&app, tenant, "A").await;
    let b = sku_named(&app, tenant, "B").await;
    let c = sku_named(&app, tenant, "C").await;
    let port = UsagePort::new(PortAnswer::Counts);
    port.set(counts(a, 2, 1));
    port.set(counts(b, 5, 3));
    port.set(bss_products_sdk::sku_usage::SkuUsage {
        sku_id: c,
        ..Default::default()
    });
    state.hub.register::<dyn SkuUsageV1>(port.clone());
    let card = get(&app, tenant, &format!("/bss-products/v1/skus/{a}")).await;
    assert_eq!(card.status(), StatusCode::OK);
    let card = body_json(card).await;
    assert_eq!(card["sku"]["id"], a.to_string());
    assert!(card["references"].is_object(), "{card}");
    assert_eq!(
        card["usage"],
        json!({
            "entries": 2,
            "currencies": ["EUR", "USD"],
            "prices": {"approved": 3, "pending": 1, "draft": 2},
            "plans": 1,
        })
    );
    assert_eq!(port.calls(), vec![(tenant, vec![a])]);
    let page = body_json(get(&app, tenant, "/bss-products/v1/skus?limit=2").await).await;
    let items = page["items"].as_array().unwrap();
    let next = page["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["code"], "A", "the SKU's own fields stay flat");
    assert_eq!(items[0]["usage"]["entries"], 2);
    assert_eq!(items[1]["code"], "B");
    assert_eq!(items[1]["usage"]["entries"], 5);
    assert_eq!(items[1]["usage"]["plans"], 3);
    let rest = body_json(
        get(
            &app,
            tenant,
            &format!("/bss-products/v1/skus?limit=2&cursor={next}"),
        )
        .await,
    )
    .await;
    assert_eq!(
        rest["items"][0]["usage"],
        json!({
            "entries": 0,
            "currencies": [],
            "prices": {"approved": 0, "pending": 0, "draft": 0},
            "plans": 0,
        }),
        "an unpriced SKU reads zeros"
    );
    assert_eq!(
        port.calls(),
        vec![(tenant, vec![a]), (tenant, vec![a, b]), (tenant, vec![c])],
        "one batch call per page, with that page's ids"
    );
}

/// P-D-197: no port registered — the SKU reads answer as before, with `usage: null`.
#[tokio::test]
async fn without_a_usage_port_the_sku_reads_answer_usage_null() {
    let tenant = Uuid::new_v4();
    let (app, _state) = usage_app(tenant).await;
    let a = sku_named(&app, tenant, "A").await;
    let card = get(&app, tenant, &format!("/bss-products/v1/skus/{a}")).await;
    assert_eq!(card.status(), StatusCode::OK);
    let card = body_json(card).await;
    assert_eq!(card.get("usage"), Some(&Value::Null), "{card}");
    let list = get(&app, tenant, "/bss-products/v1/skus").await;
    assert_eq!(list.status(), StatusCode::OK);
    let list = body_json(list).await;
    assert_eq!(list["items"][0].get("usage"), Some(&Value::Null), "{list}");
    assert_eq!(list["items"][0]["code"], "A");
}

/// P-D-197: a port that refuses the caller, cannot answer, or breaks leaves `usage: null`, and
/// the SKU read still answers 200.
#[tokio::test]
async fn a_usage_port_that_refuses_fails_or_breaks_leaves_usage_null() {
    use bss_products_sdk::sku_usage::SkuUsageV1;
    for answer in [PortAnswer::Refuses, PortAnswer::Fails, PortAnswer::Panics] {
        let tenant = Uuid::new_v4();
        let (app, state) = usage_app(tenant).await;
        let a = sku_named(&app, tenant, "A").await;
        let port = UsagePort::new(answer);
        state.hub.register::<dyn SkuUsageV1>(port.clone());
        let card = get(&app, tenant, &format!("/bss-products/v1/skus/{a}")).await;
        assert_eq!(card.status(), StatusCode::OK);
        let card = body_json(card).await;
        assert_eq!(card.get("usage"), Some(&Value::Null), "{card}");
        assert_eq!(card["sku"]["code"], "A");
        let list = get(&app, tenant, "/bss-products/v1/skus").await;
        assert_eq!(list.status(), StatusCode::OK);
        let list = body_json(list).await;
        assert_eq!(list["items"][0].get("usage"), Some(&Value::Null), "{list}");
        assert_eq!(
            port.calls().len(),
            2,
            "the port was asked: null is its answer, not its absence"
        );
    }
}

/// A SKU read that must answer while the port hangs; the test's own bound, well past the door's.
async fn answered(app: &Router, tenant: Uuid, uri: &str) -> axum::response::Response {
    tokio::time::timeout(std::time::Duration::from_secs(10), get(app, tenant, uri))
        .await
        .expect("the SKU read answers although the port never does")
}

/// P-D-197: a port call that never returns is bounded. Once the bound elapses the SKU read
/// answers 200 with `usage: null`, on the card and on the list.
#[tokio::test]
async fn a_usage_port_that_never_answers_leaves_usage_null_once_its_bound_elapses() {
    use bss_products_sdk::sku_usage::SkuUsageV1;
    let tenant = Uuid::new_v4();
    let (app, state) = usage_app(tenant).await;
    let a = sku_named(&app, tenant, "A").await;
    let port = UsagePort::new(PortAnswer::Hangs);
    state.hub.register::<dyn SkuUsageV1>(port.clone());
    let card = answered(&app, tenant, &format!("/bss-products/v1/skus/{a}")).await;
    assert_eq!(card.status(), StatusCode::OK);
    let card = body_json(card).await;
    assert_eq!(card.get("usage"), Some(&Value::Null), "{card}");
    assert_eq!(card["sku"]["code"], "A");
    let list = answered(&app, tenant, "/bss-products/v1/skus").await;
    assert_eq!(list.status(), StatusCode::OK);
    let list = body_json(list).await;
    assert_eq!(list["items"][0].get("usage"), Some(&Value::Null), "{list}");
    assert_eq!(port.calls().len(), 2, "the port was asked each time");
    for _ in 0..100 {
        if port.abandoned.load(std::sync::atomic::Ordering::SeqCst) == 2 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("a call past its bound is aborted, not left running");
}

/// P-D-197: a SKU read dropped before the bound (its client went away) takes the port call with
/// it; a call that outlives its read would hold pricing's connection with nobody to abort it.
#[tokio::test]
async fn a_sku_read_dropped_before_the_bound_aborts_its_port_call() {
    use bss_products_sdk::sku_usage::SkuUsageV1;
    let tenant = Uuid::new_v4();
    let (app, state) = usage_app(tenant).await;
    let a = sku_named(&app, tenant, "A").await;
    let port = UsagePort::new(PortAnswer::Hangs);
    state.hub.register::<dyn SkuUsageV1>(port.clone());
    let uri = format!("/bss-products/v1/skus/{a}");
    let mut read = Box::pin(get(&app, tenant, &uri));
    let asked = async {
        while port.calls().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    };
    tokio::select! {
        _ = &mut read => panic!("the read answered before its port call hung"),
        () = asked => {}
    }
    drop(read);
    for _ in 0..100 {
        if port.abandoned.load(std::sync::atomic::Ordering::SeqCst) == 1 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("a port call whose read was dropped is aborted, not left running");
}
