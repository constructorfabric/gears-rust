#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::router;
use crate::test_support::{
    body_json, get, patch, post, problem_code, raw_i64, rest_app, violation_for,
};
use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn create_list_patch_and_the_duplicate_and_stale_paths() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code":" hosting ","name":" Hosting ","is_default":true}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let etag = r.headers()["etag"].to_str().unwrap().to_owned();
    let c = body_json(r).await;
    assert_eq!(c["code"], "hosting");
    assert_eq!(c["is_default"], true);
    let url = format!("/bss-products/v1/categories/{}", c["id"].as_str().unwrap());
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code":"hosting","name":"Again"}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_CODE_TAKEN");
    let r = patch(&app, tenant, &url, json!({"name":"Cloud"}), None).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert!(violation_for(&body_json(r).await, "If-Match").is_some());
    let r = patch(&app, tenant, &url, json!({"name":"Cloud"}), Some("\"99\"")).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "STALE_REVISION");
    let r = patch(
        &app,
        tenant,
        &url,
        json!({"name":"Cloud", "sort_order":2}),
        Some(&etag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_ne!(r.headers()["etag"], etag);
    assert_eq!(body_json(r).await["name"], "Cloud");
    let list = body_json(get(&app, tenant, "/bss-products/v1/categories").await).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_audit_log").await,
        2
    );
    let other = body_json(get(&app, Uuid::new_v4(), "/bss-products/v1/categories").await).await;
    assert_eq!(other["items"], json!([]));
}

#[tokio::test]
async fn retire_refuses_used_categories_and_audit_failure_rolls_back() {
    use crate::test_support::{drop_table, repo_connection, seed_rest_sku};
    use sea_orm::{ConnectionTrait, Database};
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let c = body_json(
        post(
            &app,
            tenant,
            "/bss-products/v1/categories",
            json!({"code":"hosting","name":"Hosting"}),
        )
        .await,
    )
    .await;
    let id: Uuid = serde_json::from_value(c["id"].clone()).unwrap();
    let (db, scope) = repo_connection(&dsn, tenant).await;
    seed_rest_sku(&db.conn().unwrap(), &scope, tenant, id, "A").await;
    let url = format!("/bss-products/v1/categories/{id}/retire");
    let r = post(&app, tenant, &url, json!({})).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_IN_USE");
    let raw = Database::connect(&dsn).await.unwrap();
    raw.execute_unprepared("DELETE FROM products_sku")
        .await
        .unwrap();
    raw.close().await.unwrap();
    let r = post(&app, tenant, &url, json!({})).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(body_json(r).await["status"], "retired");
    assert_eq!(
        post(
            &app,
            tenant,
            &format!("/bss-products/v1/categories/{}/retire", Uuid::new_v4()),
            json!({})
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    drop_table(&dsn, "products_audit_log").await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code":"rollback","name":"Rollback"}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_category WHERE code = 'rollback'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn malformed_fields_are_400_and_authentication_precedes_body_validation() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app(tenant, router).await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code":42,"name":"Bad"}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let r = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bss-products/v1/categories")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

/// P-D-196: a SKU without a category never blocks a category's retirement, nor counts as the
/// category's use; a SKU that points at the category still does.
#[tokio::test]
async fn a_sku_without_a_category_never_blocks_a_retirement() {
    use crate::infra::storage::repo;
    use crate::test_support::repo_connection;
    use std::sync::Arc;
    fn doors(
        s: Arc<crate::api::rest::ApiState>,
        o: &dyn toolkit::api::OpenApiRegistry,
    ) -> axum::Router {
        router(Arc::clone(&s), o).merge(crate::api::rest::skus::router(s, o))
    }
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let category = |code: &'static str| {
        let app = app.clone();
        async move {
            let r = post(
                &app,
                tenant,
                "/bss-products/v1/categories",
                json!({"code":code,"name":code}),
            )
            .await;
            assert_eq!(r.status(), StatusCode::CREATED);
            serde_json::from_value::<Uuid>(body_json(r).await["id"].clone()).unwrap()
        }
    };
    let unused = category("unused").await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/skus",
        json!({"code":"LOOSE","name":"Loose","type":"recurring"}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let (db, scope) = repo_connection(&dsn, tenant).await;
    assert_eq!(
        repo::count_skus_in_category(&db.conn().unwrap(), &scope, tenant, unused)
            .await
            .unwrap(),
        0
    );
    let r = post(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{unused}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(body_json(r).await["status"], "retired");
    let used = category("used").await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/skus",
        json!({"code":"HELD","name":"Held","type":"recurring","category_id":used}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = post(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{used}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_IN_USE");
}

/// P-D-208 (D6): retiring a category that is already retired names its own cause,
/// `CATEGORY_RETIRED`, not `CATEGORY_IN_USE`.
#[tokio::test]
async fn retiring_a_retired_category_is_category_retired() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app(tenant, router).await;
    let c = body_json(
        post(
            &app,
            tenant,
            "/bss-products/v1/categories",
            json!({"code":"gone","name":"Gone"}),
        )
        .await,
    )
    .await;
    let url = format!(
        "/bss-products/v1/categories/{}/retire",
        c["id"].as_str().unwrap()
    );
    assert_eq!(
        post(&app, tenant, &url, json!({})).await.status(),
        StatusCode::OK
    );
    let r = post(&app, tenant, &url, json!({})).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_RETIRED");
}

/// P-D-208 (#9, amends P-D-186; P-D-248): a category is in use only while a SKU in `draft`,
/// `published` or `deprecated` names it. A retire under review keeps one of those, so it still
/// holds the category. SKUs that are all `retired` no longer hold it.
#[tokio::test]
async fn only_a_sku_that_is_not_retired_keeps_a_category_in_use() {
    use crate::test_support::{id_matches, repo_connection, seed_rest_sku};
    use sea_orm::{ConnectionTrait, Database};
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (db, scope) = repo_connection(&dsn, tenant).await;
    for (n, lifecycle, expected) in [
        (0, "draft", StatusCode::CONFLICT),
        (1, "published", StatusCode::CONFLICT),
        (2, "deprecated", StatusCode::CONFLICT),
        (4, "retired", StatusCode::OK),
    ] {
        let c = body_json(
            post(
                &app,
                tenant,
                "/bss-products/v1/categories",
                json!({"code":format!("c{n}"),"name":format!("C{n}")}),
            )
            .await,
        )
        .await;
        let id: Uuid = serde_json::from_value(c["id"].clone()).unwrap();
        let retired =
            seed_rest_sku(&db.conn().unwrap(), &scope, tenant, id, &format!("R{n}")).await;
        let held = seed_rest_sku(&db.conn().unwrap(), &scope, tenant, id, &format!("H{n}")).await;
        let raw = Database::connect(&dsn).await.unwrap();
        raw.execute_unprepared(&format!(
            "UPDATE products_sku SET lifecycle = 'retired' WHERE {}",
            id_matches("id", retired.id)
        ))
        .await
        .unwrap();
        raw.execute_unprepared(&format!(
            "UPDATE products_sku SET lifecycle = '{lifecycle}' WHERE {}",
            id_matches("id", held.id)
        ))
        .await
        .unwrap();
        raw.close().await.unwrap();
        let r = post(
            &app,
            tenant,
            &format!("/bss-products/v1/categories/{id}/retire"),
            json!({}),
        )
        .await;
        assert_eq!(
            r.status(),
            expected,
            "a {lifecycle} SKU beside a retired one"
        );
        let b = body_json(r).await;
        if expected == StatusCode::OK {
            assert_eq!(b["status"], "retired");
        } else {
            assert_eq!(problem_code(&b), "CATEGORY_IN_USE", "{lifecycle}");
        }
    }
    let c = body_json(
        post(
            &app,
            tenant,
            "/bss-products/v1/categories",
            json!({"code":"c-fence","name":"C fence"}),
        )
        .await,
    )
    .await;
    let id: Uuid = serde_json::from_value(c["id"].clone()).unwrap();
    let held = seed_rest_sku(&db.conn().unwrap(), &scope, tenant, id, "Rf").await;
    let raw = Database::connect(&dsn).await.unwrap();
    raw.execute_unprepared(&format!(
        "UPDATE products_sku SET lifecycle = 'published', retire_pending = 1, fenced_at = '2026-01-01T00:00:00Z' WHERE {}",
        id_matches("id", held.id)
    ))
    .await
    .unwrap();
    raw.close().await.unwrap();
    let r = post(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{id}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_IN_USE");

    let c = body_json(
        post(
            &app,
            tenant,
            "/bss-products/v1/categories",
            json!({"code":"c-due","name":"C due"}),
        )
        .await,
    )
    .await;
    let id: Uuid = serde_json::from_value(c["id"].clone()).unwrap();
    let held = seed_rest_sku(&db.conn().unwrap(), &scope, tenant, id, "Rd").await;
    let raw = Database::connect(&dsn).await.unwrap();
    raw.execute_unprepared(&format!(
        "UPDATE products_sku SET lifecycle = 'retired', lifecycle_next = 'published', lifecycle_next_from = '2020-01-01' WHERE {}",
        id_matches("id", held.id)
    ))
    .await
    .unwrap();
    raw.close().await.unwrap();
    let r = post(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{id}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(
        r.status(),
        StatusCode::CONFLICT,
        "a due published next still holds the category"
    );
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_IN_USE");
}

// ------------------------------------------------------------------ P-D-215: category reads

/// A category of `tenant` with this code and sort order, by the door.
async fn new_category(app: &axum::Router, tenant: Uuid, code: &str, sort_order: i32) -> Uuid {
    let r = post(
        app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code":code,"name":format!("Name {code}"),"sort_order":sort_order}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    serde_json::from_value(body_json(r).await["id"].clone()).unwrap()
}

/// SKUs of `category` in each lifecycle named, seeded as drafts and moved there raw.
async fn skus_in(dsn: &str, tenant: Uuid, category: Uuid, prefix: &str, lifecycles: &[&str]) {
    use crate::test_support::{id_matches, repo_connection, seed_rest_sku};
    use sea_orm::{ConnectionTrait, Database};
    let (db, scope) = repo_connection(dsn, tenant).await;
    let raw = Database::connect(dsn).await.unwrap();
    for (n, lifecycle) in lifecycles.iter().enumerate() {
        let s = seed_rest_sku(
            &db.conn().unwrap(),
            &scope,
            tenant,
            category,
            &format!("{prefix}{n}"),
        )
        .await;
        raw.execute_unprepared(&format!(
            "UPDATE products_sku SET lifecycle = '{lifecycle}' WHERE {}",
            id_matches("id", s.id)
        ))
        .await
        .unwrap();
    }
    raw.close().await.unwrap();
}

/// `GET /categories/{id}` reads one category with its `ETag` and its `sku_count`: the SKUs that
/// are not retired (the ones that keep it in use); another category's SKUs, a SKU without a
/// category and another tenant's are not counted, and another tenant's category is 404.
#[tokio::test]
async fn a_category_reads_alone_with_its_etag_and_sku_count() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let hosting = new_category(&app, tenant, "hosting", 1).await;
    let storage = new_category(&app, tenant, "storage", 2).await;
    skus_in(
        &dsn,
        tenant,
        hosting,
        "H",
        &[
            "draft",
            "published",
            "deprecated",
            "published",
            "retired",
            "retired",
        ],
    )
    .await;
    skus_in(&dsn, tenant, storage, "S", &["published"]).await;
    let r = get(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{hosting}"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["etag"], "\"1\"");
    let c = body_json(r).await;
    assert_eq!(
        (&c["code"], &c["status"], &c["sort_order"], &c["sku_count"]),
        (&json!("hosting"), &json!("active"), &json!(1), &json!(4)),
        "{c}"
    );
    let r = get(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{storage}"),
    )
    .await;
    assert_eq!(body_json(r).await["sku_count"], 1);
    let empty = new_category(&app, tenant, "empty", 3).await;
    let r = get(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{empty}"),
    )
    .await;
    assert_eq!(body_json(r).await["sku_count"], 0);
    for (who, id) in [(Uuid::new_v4(), hosting), (tenant, Uuid::new_v4())] {
        let r = get(&app, who, &format!("/bss-products/v1/categories/{id}")).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
    // A retired category reads too, with no SKU keeping it (P-D-208).
    skus_in(&dsn, tenant, empty, "E", &["retired"]).await;
    let r = post(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{empty}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let c = body_json(
        get(
            &app,
            tenant,
            &format!("/bss-products/v1/categories/{empty}"),
        )
        .await,
    )
    .await;
    assert_eq!(
        (&c["status"], &c["sku_count"]),
        (&json!("retired"), &json!(0))
    );
}

/// The codes of a list page, and the page.
async fn codes(app: &axum::Router, tenant: Uuid, query: &str) -> (Vec<String>, serde_json::Value) {
    let r = get(app, tenant, &format!("/bss-products/v1/categories{query}")).await;
    assert_eq!(r.status(), StatusCode::OK, "{query}");
    let page = body_json(r).await;
    let codes = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap().to_owned())
        .collect();
    (codes, page)
}

/// `GET /categories` pages on the toolkit's `OData`: by default in `sort_order`, then `code`, 200 to
/// a page; `sort_order` orders, `status` and `is_default` filter; every item carries its
/// `sku_count`; a cursor walks the list in the order it was cut; the list refuses what it does not
/// take.
#[tokio::test]
#[allow(clippy::too_many_lines, reason = "the whole list contract")]
async fn the_category_list_pages_in_sort_order_then_code_with_counts() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    for (code, sort_order) in [
        ("delta", 2),
        ("alpha", 2),
        ("charlie", 1),
        ("bravo", 3),
        ("echo", 1),
    ] {
        new_category(&app, tenant, code, sort_order).await;
    }
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code":"foxtrot","name":"Foxtrot","is_default":true,"sort_order":0}),
    )
    .await;
    let foxtrot: Uuid = serde_json::from_value(body_json(r).await["id"].clone()).unwrap();
    skus_in(
        &dsn,
        tenant,
        foxtrot,
        "F",
        &["published", "retired", "draft"],
    )
    .await;
    let order = ["foxtrot", "charlie", "echo", "alpha", "delta", "bravo"];
    let (all, page) = codes(&app, tenant, "").await;
    assert_eq!(all, order);
    assert_eq!(page["page_info"]["limit"], 200, "{page}");
    assert!(page["page_info"]["next_cursor"].is_null());
    let counts: Vec<&serde_json::Value> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["sku_count"])
        .collect();
    assert_eq!(counts, [2, 0, 0, 0, 0, 0], "{page}");
    // Walked two at a time, the cursor keeps the default order.
    let mut walked = Vec::new();
    let mut query = "?limit=2".to_owned();
    loop {
        let (codes, page) = codes(&app, tenant, &query).await;
        walked.extend(codes);
        let Some(next) = page["page_info"]["next_cursor"].as_str() else {
            break;
        };
        query = format!("?limit=2&cursor={next}");
    }
    assert_eq!(walked, order);
    assert_eq!(
        codes(&app, tenant, "?%24orderby=sort_order%20desc,code%20desc")
            .await
            .0,
        ["bravo", "delta", "alpha", "echo", "charlie", "foxtrot"]
    );
    // "Foxtrot" sorts before every "Name …".
    assert_eq!(
        codes(&app, tenant, "?%24orderby=name").await.0,
        ["foxtrot", "alpha", "bravo", "charlie", "delta", "echo"]
    );
    assert_eq!(
        codes(&app, tenant, "?%24filter=is_default%20eq%20true")
            .await
            .0,
        ["foxtrot"]
    );
    let r = post(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{}/retire", {
            let (_, page) = codes(&app, tenant, "?%24filter=code%20eq%20%27echo%27").await;
            page["items"][0]["id"].as_str().unwrap().to_owned()
        }),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        codes(&app, tenant, "?%24filter=status%20eq%20%27active%27")
            .await
            .0,
        ["foxtrot", "charlie", "alpha", "delta", "bravo"]
    );
    assert_eq!(
        codes(&app, tenant, "?%24filter=sort_order%20ge%202")
            .await
            .0,
        ["alpha", "delta", "bravo"]
    );
    let (_, page) = codes(&app, tenant, "?%24top=5000").await;
    assert_eq!(page["page_info"]["limit"], 200, "`$top` is clamped at 200");
    // A cursor cut under one filter is refused under another, or under none.
    let (_, page) = codes(&app, tenant, "?%24filter=sort_order%20ge%201&limit=1").await;
    let cursor = page["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    for query in [
        format!("?cursor={cursor}"),
        format!("?%24filter=sort_order%20ge%202&cursor={cursor}"),
    ] {
        let r = get(&app, tenant, &format!("/bss-products/v1/categories{query}")).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(problem_code(&body_json(r).await), "FILTER_MISMATCH");
    }
    for query in [
        "?%24select=code",
        "?%24count=true",
        "?bogus=1",
        "?%24orderby=status",
        "?%24orderby=is_default",
        "?%24filter=status%20eq%20%27gone%27",
        "?%24filter=nope%20eq%201",
        "?%24filter=code%20eq%20null",
        "?limit=0",
        "?cursor=garbage",
    ] {
        let r = get(&app, tenant, &format!("/bss-products/v1/categories{query}")).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{query}");
    }
    let (other, _) = codes(&app, Uuid::new_v4(), "").await;
    assert!(other.is_empty(), "another tenant lists none");
}

/// A door over a database whose statements are recorded, with `n` categories, each named by one
/// published SKU.
async fn recorded_categories(
    n: usize,
) -> (
    axum::Router,
    Uuid,
    Uuid,
    toolkit_db::test_support::QueryRecorder,
) {
    use sea_orm_migration::MigratorTrait;
    let dsn = crate::test_support::TestDsn::new("products-categories-");
    let (db, recorder) = toolkit_db::test_support::connect_with_recorder(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        toolkit_db::outbox::outbox_migrations_with_prefix(
            crate::infra::events::OUTBOX_TABLE_PREFIX,
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let tenant = Uuid::new_v4();
    let (app, _) = crate::test_support::rest_app_on_db(
        tenant,
        router,
        crate::test_support::resolved_usage_types(),
        "test",
        toolkit_db::DBProvider::new(db),
    )
    .await;
    let mut first = Uuid::nil();
    for i in 0..n {
        let id = new_category(&app, tenant, &format!("c{i:03}"), 0).await;
        if i == 0 {
            first = id;
        }
        skus_in(&dsn, tenant, id, &format!("S{i:03}-"), &["published"]).await;
    }
    recorder.clear();
    // The router holds the database's temporary directory.
    (app.layer(axum::Extension(dsn)), tenant, first, recorder)
}

/// The statements on the gear's tables one read makes, with their bind counts.
async fn statements(
    app: &axum::Router,
    tenant: Uuid,
    recorder: &toolkit_db::test_support::QueryRecorder,
    uri: &str,
) -> Vec<(String, usize)> {
    recorder.clear();
    let r = get(app, tenant, uri).await;
    assert_eq!(r.status(), StatusCode::OK, "{uri}");
    let page = body_json(r).await;
    assert!(page.to_string().contains("\"sku_count\":1"), "{page}");
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("products_"))
        })
        .map(|q| (q.sql, q.param_count))
        .collect()
}

/// The list and the single read count the SKUs in ONE grouped statement, the same for 10 and for
/// 100 categories: one read of the categories and one count.
#[tokio::test]
async fn category_reads_count_skus_in_fixed_statements_for_10_and_100_categories() {
    let (ten, ten_tenant, ten_first, ten_rec) = recorded_categories(10).await;
    let (hundred, hundred_tenant, hundred_first, hundred_rec) = recorded_categories(100).await;
    let a = statements(&ten, ten_tenant, &ten_rec, "/bss-products/v1/categories").await;
    let b = statements(
        &hundred,
        hundred_tenant,
        &hundred_rec,
        "/bss-products/v1/categories",
    )
    .await;
    for (i, (sql, binds)) in b.iter().enumerate() {
        eprintln!("list statement {i} ({binds} binds): {sql}");
    }
    assert_eq!(a.len(), 2, "one page read and one grouped count: {a:#?}");
    assert_eq!(
        a, b,
        "the same statements whatever the number of categories"
    );
    let a = statements(
        &ten,
        ten_tenant,
        &ten_rec,
        &format!("/bss-products/v1/categories/{ten_first}"),
    )
    .await;
    let b = statements(
        &hundred,
        hundred_tenant,
        &hundred_rec,
        &format!("/bss-products/v1/categories/{hundred_first}"),
    )
    .await;
    assert_eq!(a.len(), 2, "one read and one grouped count: {a:#?}");
    assert_eq!(a, b);
}

/// P-D-217: a stored status outside the category's closed set (a row written around the gear, with
/// its CHECK bypassed) is `CorruptRow`: the read answers 500 — never a panic, never the token
/// under an `enum` that does not hold it — and the gear goes on serving.
#[tokio::test]
async fn a_stored_status_outside_its_closed_set_is_a_500_never_a_panic() {
    use crate::test_support::id_matches;
    use sea_orm::{ConnectionTrait, Database};
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let poisoned = new_category(&app, tenant, "poisoned", 1).await;
    let healthy = new_category(&app, tenant, "healthy", 2).await;
    let raw = Database::connect(&dsn).await.unwrap();
    // The premise: the CHECK holds the column to its set, so only a writer around it can.
    let refused = raw
        .execute_unprepared(&format!(
            "UPDATE products_category SET status = 'archived' WHERE {}",
            id_matches("id", poisoned)
        ))
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("CHECK"), "{refused}");
    raw.execute_unprepared("PRAGMA ignore_check_constraints = ON")
        .await
        .unwrap();
    let written = raw
        .execute_unprepared(&format!(
            "UPDATE products_category SET status = 'archived' WHERE {}",
            id_matches("id", poisoned)
        ))
        .await
        .unwrap();
    assert_eq!(written.rows_affected(), 1);
    raw.close().await.unwrap();
    let r = get(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{poisoned}"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let b = body_json(r).await;
    assert!(!b.to_string().contains("archived"), "{b}");
    let r = get(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{healthy}"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK, "the gear still serves");
}

// ------------------------------------------------------------------ P-D-218: moving the default

const CATEGORIES: &str = "/bss-products/v1/categories";

/// A category by the door: its id and `ETag`.
async fn default_candidate(
    app: &axum::Router,
    tenant: Uuid,
    code: &str,
    is_default: bool,
) -> (Uuid, String) {
    let r = post(
        app,
        tenant,
        CATEGORIES,
        json!({"code":code,"name":format!("Name {code}"),"is_default":is_default}),
    )
    .await;
    let status = r.status();
    let etag = r
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().to_owned());
    let body = body_json(r).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (
        serde_json::from_value(body["id"].clone()).unwrap(),
        etag.unwrap(),
    )
}

/// The ids of the tenant's default categories, by the list's own filter.
async fn defaults(app: &axum::Router, tenant: Uuid) -> Vec<String> {
    let body = body_json(
        get(
            app,
            tenant,
            &format!("{CATEGORIES}?%24filter=is_default%20eq%20true"),
        )
        .await,
    )
    .await;
    body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a page: {body}"))
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect()
}

/// **Making a second category the default moves the default** (P-D-218), on PATCH and on POST:
/// the previous holder is cleared in the same transaction, both rows are written and audited as
/// category writes are, and the tenant has one default after. It was a 500: the partial unique
/// index `uq_products_category_default` refused the second default and nothing mapped it.
#[tokio::test]
async fn a_second_default_moves_the_default_on_patch_and_on_post() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (first, first_tag) = default_candidate(&app, tenant, "first", true).await;
    let (second, second_tag) = default_candidate(&app, tenant, "second", false).await;

    let r = patch(
        &app,
        tenant,
        &format!("{CATEGORIES}/{second}"),
        json!({"is_default":true}),
        Some(&second_tag),
    )
    .await;
    let status = r.status();
    let body = body_json(r).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["is_default"], true, "{body}");
    let old = body_json(get(&app, tenant, &format!("{CATEGORIES}/{first}")).await).await;
    assert_eq!(
        (old["is_default"].clone(), old["version"].clone()),
        (json!(false), json!(2)),
        "{old}"
    );
    assert_eq!(defaults(&app, tenant).await, vec![second.to_string()]);
    // The move wrote the old holder: its old ETag is stale now.
    let r = patch(
        &app,
        tenant,
        &format!("{CATEGORIES}/{first}"),
        json!({"name":"First"}),
        Some(&first_tag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "STALE_REVISION");

    let (third, _) = default_candidate(&app, tenant, "third", true).await;
    assert_eq!(defaults(&app, tenant).await, vec![third.to_string()]);
    let old = body_json(get(&app, tenant, &format!("{CATEGORIES}/{second}")).await).await;
    assert_eq!(old["is_default"], false, "{old}");

    // Three creates; the PATCH move wrote two updates (the old holder and the new one), the POST
    // move one (the old holder) beside its create.
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE action = 'category.update'"
        )
        .await,
        3
    );
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_audit_log WHERE action = 'category.create'"
        )
        .await,
        3
    );
    // Making the default the default again moves nothing.
    let tag = get(&app, tenant, &format!("{CATEGORIES}/{third}"))
        .await
        .headers()["etag"]
        .to_str()
        .unwrap()
        .to_owned();
    let r = patch(
        &app,
        tenant,
        &format!("{CATEGORIES}/{third}"),
        json!({"is_default":true}),
        Some(&tag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(defaults(&app, tenant).await, vec![third.to_string()]);
}

/// **A default another writer takes between the clear and the set is 409
/// `CATEGORY_DEFAULT_TAKEN`, never a 500** (P-D-218), on PATCH and on POST, and the refused act
/// writes nothing. The racer is a trigger that makes another category the default inside the
/// door's own write, which is the window a concurrent move lands in.
#[tokio::test]
async fn a_default_taken_by_a_racing_writer_is_409_never_500() {
    use sea_orm::{ConnectionTrait, Database};
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (first, _) = default_candidate(&app, tenant, "first", true).await;
    let (second, second_tag) = default_candidate(&app, tenant, "second", false).await;
    let racer = "INSERT INTO products_category (id, tenant_id, code, name, is_default, sort_order, \
                 status, version, created_at, updated_at) VALUES (randomblob(16), NEW.tenant_id, \
                 'racer-' || hex(randomblob(4)), 'Racer', 1, 0, 'active', 1, NEW.created_at, \
                 NEW.updated_at);";
    let raw = Database::connect(&dsn).await.unwrap();
    raw.execute_unprepared(&format!(
        "CREATE TRIGGER race_the_patch BEFORE UPDATE OF is_default ON products_category \
         WHEN NEW.is_default = 1 BEGIN {racer} END"
    ))
    .await
    .unwrap();
    raw.execute_unprepared(&format!(
        "CREATE TRIGGER race_the_post BEFORE INSERT ON products_category \
         WHEN NEW.is_default = 1 AND NEW.code = 'third' BEGIN {racer} END"
    ))
    .await
    .unwrap();
    raw.close().await.unwrap();
    let audits = raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_audit_log").await;

    let r = patch(
        &app,
        tenant,
        &format!("{CATEGORIES}/{second}"),
        json!({"is_default":true}),
        Some(&second_tag),
    )
    .await;
    let status = r.status();
    let body = body_json(r).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(problem_code(&body), "CATEGORY_DEFAULT_TAKEN", "{body}");

    let r = post(
        &app,
        tenant,
        CATEGORIES,
        json!({"code":"third","name":"Third","is_default":true}),
    )
    .await;
    let status = r.status();
    let body = body_json(r).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(problem_code(&body), "CATEGORY_DEFAULT_TAKEN", "{body}");

    assert_eq!(
        defaults(&app, tenant).await,
        vec![first.to_string()],
        "nothing moved"
    );
    assert_eq!(
        raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_audit_log").await,
        audits,
        "a refused move writes no audit row"
    );
}

/// Two moves at once leave one default and no 500: each is 200 (the later one moved it again)
/// or 409 `CATEGORY_DEFAULT_TAKEN`.
#[tokio::test]
async fn two_concurrent_moves_leave_one_default_and_no_500() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app(tenant, router).await;
    default_candidate(&app, tenant, "first", true).await;
    let (b, b_tag) = default_candidate(&app, tenant, "b", false).await;
    let (c, c_tag) = default_candidate(&app, tenant, "c", false).await;
    let (b_uri, c_uri) = (format!("{CATEGORIES}/{b}"), format!("{CATEGORIES}/{c}"));
    let (rb, rc) = tokio::join!(
        patch(
            &app,
            tenant,
            &b_uri,
            json!({"is_default":true}),
            Some(&b_tag)
        ),
        patch(
            &app,
            tenant,
            &c_uri,
            json!({"is_default":true}),
            Some(&c_tag)
        ),
    );
    for r in [rb, rc] {
        let status = r.status();
        let body = body_json(r).await;
        assert!(
            status == StatusCode::OK
                || (status == StatusCode::CONFLICT
                    && problem_code(&body) == "CATEGORY_DEFAULT_TAKEN"),
            "{status}: {body}"
        );
    }
    let now = defaults(&app, tenant).await;
    assert_eq!(now.len(), 1, "{now:?}");
    assert!(
        now[0] == b.to_string() || now[0] == c.to_string(),
        "{now:?}"
    );
}

/// The category's `ETag`, as its read answers it.
async fn tag_of(app: &axum::Router, tenant: Uuid, id: Uuid) -> String {
    get(app, tenant, &format!("{CATEGORIES}/{id}"))
        .await
        .headers()["etag"]
        .to_str()
        .unwrap()
        .to_owned()
}

/// The category's audit rows, `action@version` in the order they were written.
async fn category_rows(dsn: &str, id: Uuid) -> Vec<String> {
    use crate::test_support::id_matches;
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let raw = Database::connect(dsn).await.unwrap();
    let rows = raw
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            format!(
                "SELECT action || '@' || subject_revision AS v FROM products_audit_log \
                 WHERE subject_kind = 'category' AND {} ORDER BY audit_id",
                id_matches("subject_id", id)
            ),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect();
    raw.close().await.unwrap();
    rows
}

/// **A retired category is never made the default** (P-D-220, amends P-D-218). PATCH
/// `{is_default: true}` on a retired category is 409 `CATEGORY_RETIRED` and moves nothing: the
/// tenant's default keeps its flag and its version, and no row is written. A stale tag is judged
/// first (`STALE_REVISION`, as every PATCH). The retired category's other edits still pass: a
/// rename, and `is_default: false`. A POST always creates an active category, so it has no such
/// case.
#[tokio::test]
async fn a_retired_category_is_never_made_the_default() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (keeper, _) = default_candidate(&app, tenant, "keeper", true).await;
    let (gone, _) = default_candidate(&app, tenant, "gone", false).await;
    let r = post(
        &app,
        tenant,
        &format!("{CATEGORIES}/{gone}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let tag = tag_of(&app, tenant, gone).await;
    let rows_before = (
        category_rows(&dsn, keeper).await,
        category_rows(&dsn, gone).await,
    );

    let r = patch(
        &app,
        tenant,
        &format!("{CATEGORIES}/{gone}"),
        json!({"is_default":true}),
        Some(&tag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_RETIRED");
    assert_eq!(defaults(&app, tenant).await, vec![keeper.to_string()]);
    let kept = body_json(get(&app, tenant, &format!("{CATEGORIES}/{keeper}")).await).await;
    assert_eq!(
        (kept["is_default"].clone(), kept["version"].clone()),
        (json!(true), json!(1)),
        "{kept}"
    );
    assert_eq!(
        (
            category_rows(&dsn, keeper).await,
            category_rows(&dsn, gone).await
        ),
        rows_before,
        "the refused move writes nothing"
    );
    let r = patch(
        &app,
        tenant,
        &format!("{CATEGORIES}/{gone}"),
        json!({"is_default":true}),
        Some("\"1\""),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "STALE_REVISION");

    for body in [json!({"name":"Gone for good"}), json!({"is_default":false})] {
        let r = patch(
            &app,
            tenant,
            &format!("{CATEGORIES}/{gone}"),
            body.clone(),
            Some(&tag_of(&app, tenant, gone).await),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK, "{body}");
    }
    assert_eq!(defaults(&app, tenant).await, vec![keeper.to_string()]);
}

/// **Retiring the tenant's default clears it** (P-D-220, amends P-D-218), in the retirement's
/// transaction: the clear is a category write of its own (`version` + 1 and a `category.update`
/// row), then the retirement (`version` + 1 and its `category.retire` row). The answer reads
/// `retired` and not default, and the tenant has no default after (P-D-196: nothing falls back to
/// one). A refused retirement of the default (in use) clears nothing; retiring a category that is
/// not the default writes the one retirement.
#[tokio::test]
async fn retiring_the_default_leaves_the_tenant_without_one() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (used, _) = default_candidate(&app, tenant, "used", true).await;
    skus_in(&dsn, tenant, used, "U-", &["published"]).await;
    let r = post(
        &app,
        tenant,
        &format!("{CATEGORIES}/{used}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(problem_code(&body_json(r).await), "CATEGORY_IN_USE");
    assert_eq!(defaults(&app, tenant).await, vec![used.to_string()]);
    assert_eq!(category_rows(&dsn, used).await, ["category.create@1"]);

    let (side, _) = default_candidate(&app, tenant, "side", false).await;
    let r = post(
        &app,
        tenant,
        &format!("{CATEGORIES}/{side}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let body = body_json(r).await;
    assert_eq!(
        (body["status"].clone(), body["version"].clone()),
        (json!("retired"), json!(2)),
        "{body}"
    );
    assert_eq!(
        category_rows(&dsn, side).await,
        ["category.create@1", "category.retire@2"]
    );

    let (main, main_tag) = default_candidate(&app, tenant, "main", false).await;
    let r = patch(
        &app,
        tenant,
        &format!("{CATEGORIES}/{main}"),
        json!({"is_default":true}),
        Some(&main_tag),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(defaults(&app, tenant).await, vec![main.to_string()]);
    let r = post(
        &app,
        tenant,
        &format!("{CATEGORIES}/{main}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let body = body_json(r).await;
    assert_eq!(
        (
            body["status"].clone(),
            body["is_default"].clone(),
            body["version"].clone()
        ),
        (json!("retired"), json!(false), json!(4)),
        "{body}"
    );
    assert_eq!(
        body_json(get(&app, tenant, &format!("{CATEGORIES}/{main}")).await).await["is_default"],
        false
    );
    assert!(defaults(&app, tenant).await.is_empty(), "no default after");
    assert_eq!(
        category_rows(&dsn, main).await,
        [
            "category.create@1",
            "category.update@2",
            "category.update@3",
            "category.retire@4"
        ]
    );
}
