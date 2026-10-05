//! P-D-263: a retired SKU and a retired category can be archived, hidden from their lists, and
//! unarchived. Reads by id ignore the mark.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::api::rest::{ApiState, categories, skus};
use crate::domain::sku::NewSku;
use crate::infra::storage::repo;
use crate::test_support::{
    body_json, get, post, problem_code, raw_i64, repo_connection, request, rest_app, tenant_user,
};
use axum::{Router, http::Method, http::StatusCode};
use bss_products_sdk::models::{Lifecycle, SkuType};
use serde_json::{Value, json};
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit::api::OpenApiRegistry;
use uuid::Uuid;

fn doors(s: Arc<ApiState>, o: &dyn OpenApiRegistry) -> Router {
    categories::router(Arc::clone(&s), o).merge(skus::router(s, o))
}

/// Percent-encode a query value.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// A SKU written straight into `lifecycle` through the repository.
async fn sku(dsn: &str, tenant: Uuid, code: &str, lifecycle: Lifecycle) -> Uuid {
    let (db, scope) = repo_connection(dsn, tenant).await;
    let conn = db.conn().unwrap();
    let row = repo::insert_sku(
        &conn,
        &scope,
        tenant,
        NewSku {
            code: code.into(),
            name: code.into(),
            r#type: SkuType::Recurring,
            category_id: None,
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: None,
            unit: None,
        },
        tenant_user(tenant).subject_id(),
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    if lifecycle != Lifecycle::Draft {
        repo::set_lifecycle(
            &conn,
            &scope,
            tenant,
            row.id,
            &[Lifecycle::Draft],
            lifecycle,
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    }
    row.id
}

async fn codes(app: &Router, tenant: Uuid, filter: Option<&str>) -> Vec<String> {
    let uri = filter.map_or_else(
        || "/bss-products/v1/skus".to_owned(),
        |f| format!("/bss-products/v1/skus?$filter={}", enc(f)),
    );
    let r = get(app, tenant, &uri).await;
    assert_eq!(r.status(), StatusCode::OK, "{uri}");
    let page = body_json(r).await;
    let mut codes: Vec<String> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["code"].as_str().unwrap().to_owned())
        .collect();
    codes.sort_unstable();
    codes
}

async fn counts(app: &Router, tenant: Uuid, filter: Option<&str>) -> Value {
    let uri = filter.map_or_else(
        || "/bss-products/v1/skus/counts".to_owned(),
        |f| format!("/bss-products/v1/skus/counts?$filter={}", enc(f)),
    );
    let r = get(app, tenant, &uri).await;
    assert_eq!(r.status(), StatusCode::OK, "{uri}");
    body_json(r).await
}

async fn post_if_match(
    app: &Router,
    tenant: Uuid,
    uri: &str,
    etag: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let r = request(app, tenant, Method::POST, uri, None, etag).await;
    let status = r.status();
    let tag = r
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, tag, body)
}

async fn etag_of(app: &Router, tenant: Uuid, uri: &str) -> String {
    let r = get(app, tenant, uri).await;
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "the card read before the archive door"
    );
    r.headers()["etag"].to_str().unwrap().to_owned()
}

/// A retired SKU archives: it leaves `/skus` and every count but `archived`, `archived eq true`
/// lists it, its card reads it with the mark, and it unarchives. Each act writes its audit row.
#[tokio::test]
async fn a_retired_sku_archives_leaves_the_list_and_unarchives() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    sku(&dsn, tenant, "D1", Lifecycle::Draft).await;
    sku(&dsn, tenant, "P1", Lifecycle::Published).await;
    let retired = sku(&dsn, tenant, "R1", Lifecycle::Retired).await;
    assert_eq!(codes(&app, tenant, None).await, ["D1", "P1", "R1"]);
    let before = counts(&app, tenant, None).await;
    assert_eq!(before["all"], 3, "{before}");
    assert_eq!(before["retired"], 1, "{before}");
    assert_eq!(before["archived"], 0, "{before}");

    let card = format!("/bss-products/v1/skus/{retired}");
    let archive = format!("{card}/archive");
    let (status, _, body) = post_if_match(&app, tenant, &archive, None).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "If-Match is required: {body}"
    );
    let (status, _, body) = post_if_match(&app, tenant, &archive, Some("\"99\"")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(problem_code(&body), "STALE_REVISION");

    let tag = etag_of(&app, tenant, &card).await;
    let (status, new_tag, body) = post_if_match(&app, tenant, &archive, Some(&tag)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(
        new_tag.as_deref(),
        Some(tag.as_str()),
        "the mark is a write"
    );
    assert!(body["archived_at"].is_string(), "{body}");
    assert_eq!(
        body["archived_by"],
        json!(tenant_user(tenant).subject_id()),
        "{body}"
    );
    assert_eq!(body["lifecycle"], "retired", "the mark is not a lifecycle");

    assert_eq!(codes(&app, tenant, None).await, ["D1", "P1"]);
    assert_eq!(
        codes(&app, tenant, Some("archived eq false")).await,
        ["D1", "P1"]
    );
    assert_eq!(codes(&app, tenant, Some("archived eq true")).await, ["R1"]);
    assert_eq!(
        codes(&app, tenant, Some("archived eq true and code eq 'R1'")).await,
        ["R1"]
    );
    let after = counts(&app, tenant, None).await;
    assert_eq!(after["all"], 2, "{after}");
    assert_eq!(after["retired"], 0, "{after}");
    assert_eq!(after["archived"], 1, "{after}");
    // The counts count both sides whatever the archived term says, as they do lifecycles.
    assert_eq!(counts(&app, tenant, Some("archived eq true")).await, after);

    let r = get(&app, tenant, &card).await;
    assert_eq!(r.status(), StatusCode::OK, "a read by id ignores the mark");
    let read = body_json(r).await;
    assert!(read["sku"]["archived_at"].is_string(), "{read}");

    let tag = etag_of(&app, tenant, &card).await;
    let unarchive = format!("{card}/unarchive");
    let (status, _, body) = post_if_match(&app, tenant, &unarchive, Some(&tag)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["archived_at"].is_null(), "{body}");
    assert!(body["archived_by"].is_null(), "{body}");
    assert_eq!(codes(&app, tenant, None).await, ["D1", "P1", "R1"]);
    assert_eq!(counts(&app, tenant, None).await, before);
    for action in ["sku.archive", "sku.unarchive"] {
        assert_eq!(
            raw_i64(
                &dsn,
                &format!("SELECT COUNT(*) AS v FROM products_audit_log WHERE action = '{action}'")
            )
            .await,
            1,
            "{action}"
        );
    }
}

/// The number of audit rows with `action`.
async fn audited(dsn: &str, action: &str) -> i64 {
    raw_i64(
        dsn,
        &format!("SELECT COUNT(*) AS v FROM products_audit_log WHERE action = '{action}'"),
    )
    .await
}

/// A row already in the asked state is answered as it is and nothing is written: unarchiving a
/// SKU or a category that is not archived, and archiving one twice, keep its tag and add no audit
/// row (P-D-263).
#[tokio::test]
async fn a_move_to_the_state_a_row_is_in_writes_nothing() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let retired = sku(&dsn, tenant, "R1", Lifecycle::Retired).await;
    let r = post(
        &app,
        tenant,
        "/bss-products/v1/categories",
        json!({"code": "old", "name": "Old"}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let category = body_json(r).await["id"].as_str().unwrap().to_owned();
    let r = post(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{category}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    for (url, kind) in [
        (format!("/bss-products/v1/skus/{retired}"), "sku"),
        (
            format!("/bss-products/v1/categories/{category}"),
            "category",
        ),
    ] {
        let tag = etag_of(&app, tenant, &url).await;
        let (status, same, body) =
            post_if_match(&app, tenant, &format!("{url}/unarchive"), Some(&tag)).await;
        assert_eq!(status, StatusCode::OK, "{kind}: {body}");
        assert_eq!(same.as_deref(), Some(tag.as_str()), "{kind}: nothing moved");
        assert!(body["archived_at"].is_null(), "{kind}: {body}");
        assert_eq!(etag_of(&app, tenant, &url).await, tag, "{kind}");

        let (status, archived, first) =
            post_if_match(&app, tenant, &format!("{url}/archive"), Some(&tag)).await;
        assert_eq!(status, StatusCode::OK, "{kind}: {first}");
        let archived = archived.expect("the archive answers its tag");
        assert_ne!(archived, tag, "{kind}: the first archive is a write");
        let (status, again, second) =
            post_if_match(&app, tenant, &format!("{url}/archive"), Some(&archived)).await;
        assert_eq!(status, StatusCode::OK, "{kind}: {second}");
        assert_eq!(
            again.as_deref(),
            Some(archived.as_str()),
            "{kind}: nothing moved"
        );
        assert_eq!(second["archived_at"], first["archived_at"], "{kind}");
        assert_eq!(etag_of(&app, tenant, &url).await, archived, "{kind}");

        assert_eq!(audited(&dsn, &format!("{kind}.archive")).await, 1, "{kind}");
        assert_eq!(
            audited(&dsn, &format!("{kind}.unarchive")).await,
            0,
            "{kind}"
        );
    }
}

/// An archived SKU that a pending unit still locks is counted in `archived` only: `in_review`
/// counts the locked SKUs that are not archived (P-D-263). The Postgres twin is in
/// `tests/postgres_archive.rs`.
#[tokio::test]
async fn an_archived_locked_sku_is_not_in_review() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let retired = sku(&dsn, tenant, "R1", Lifecycle::Retired).await;
    let published = sku(&dsn, tenant, "P1", Lifecycle::Published).await;
    sku(&dsn, tenant, "D1", Lifecycle::Draft).await;
    let (db, scope) = repo_connection(&dsn, tenant).await;
    let conn = db.conn().unwrap();
    for id in [retired, published] {
        let revision = repo::find_sku(&conn, &scope, tenant, id)
            .await
            .unwrap()
            .unwrap()
            .revision;
        assert!(
            repo::try_lock_sku(&conn, &scope, tenant, id, Uuid::new_v4(), revision)
                .await
                .unwrap()
        );
    }
    let locked = repo::find_sku(&conn, &scope, tenant, retired)
        .await
        .unwrap()
        .unwrap();
    assert!(locked.pending_unit_id.is_some());
    let repo::HeadWrite::Written(_) = repo::set_sku_archived(
        &conn,
        &scope,
        tenant,
        retired,
        locked.revision,
        Some(tenant_user(tenant).subject_id()),
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap() else {
        panic!("the archive matched")
    };
    let counts = counts(&app, tenant, None).await;
    assert_eq!(counts["in_review"], 1, "only the published lock: {counts}");
    assert_eq!(counts["archived"], 1, "{counts}");
    assert_eq!(counts["all"], 2, "{counts}");
}

/// Only a retired SKU archives: a published one is 409 `SKU_NOT_RETIRED`, a draft too, and a
/// SKU the tenant does not hold is 404.
#[tokio::test]
async fn only_a_retired_sku_archives() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    for (code, lifecycle) in [("P1", Lifecycle::Published), ("D1", Lifecycle::Draft)] {
        let id = sku(&dsn, tenant, code, lifecycle).await;
        let card = format!("/bss-products/v1/skus/{id}");
        let tag = etag_of(&app, tenant, &card).await;
        let (status, _, body) =
            post_if_match(&app, tenant, &format!("{card}/archive"), Some(&tag)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{code}: {body}");
        assert_eq!(problem_code(&body), "SKU_NOT_RETIRED", "{code}");
    }
    let (status, _, _) = post_if_match(
        &app,
        tenant,
        &format!("/bss-products/v1/skus/{}/archive", Uuid::new_v4()),
        Some("\"1\""),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// An `archived` term the list cannot take apart is 400: under `or` or `not`, or a value that is
/// not a boolean.
#[tokio::test]
async fn an_archived_term_under_or_or_not_is_refused() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app(tenant, doors).await;
    for filter in [
        "archived eq true or code eq 'A'",
        "not (archived eq true)",
        "archived eq 'yes'",
    ] {
        for path in ["skus", "skus/counts", "categories"] {
            let uri = format!("/bss-products/v1/{path}?$filter={}", enc(filter));
            let r = get(&app, tenant, &uri).await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    }
}

/// A retired category archives and leaves `/categories`; `archived eq true` lists it; its read by
/// id still answers; an active one is 409 `CATEGORY_NOT_RETIRED`; a stale If-Match is 409
/// `STALE_REVISION`; it unarchives.
#[tokio::test]
async fn a_retired_category_archives_and_an_active_one_is_refused() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, doors).await;
    let mut ids = Vec::new();
    for code in ["live", "old"] {
        let r = post(
            &app,
            tenant,
            "/bss-products/v1/categories",
            json!({"code": code, "name": code}),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        ids.push(body_json(r).await["id"].as_str().unwrap().to_owned());
    }
    let (live, old) = (&ids[0], &ids[1]);
    let r = post(
        &app,
        tenant,
        &format!("/bss-products/v1/categories/{old}/retire"),
        json!({}),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);

    let live_url = format!("/bss-products/v1/categories/{live}");
    let tag = etag_of(&app, tenant, &live_url).await;
    let (status, _, body) =
        post_if_match(&app, tenant, &format!("{live_url}/archive"), Some(&tag)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(problem_code(&body), "CATEGORY_NOT_RETIRED");

    let old_url = format!("/bss-products/v1/categories/{old}");
    let (status, _, body) =
        post_if_match(&app, tenant, &format!("{old_url}/archive"), Some("\"99\"")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(problem_code(&body), "STALE_REVISION");
    let tag = etag_of(&app, tenant, &old_url).await;
    let (status, new_tag, body) =
        post_if_match(&app, tenant, &format!("{old_url}/archive"), Some(&tag)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(new_tag.as_deref(), Some(tag.as_str()));
    assert!(body["archived_at"].is_string(), "{body}");
    assert_eq!(body["status"], "retired");

    let listed = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap().to_owned())
            .collect()
    };
    let page = body_json(get(&app, tenant, "/bss-products/v1/categories").await).await;
    assert_eq!(listed(&page), ["live"], "{page}");
    let page = body_json(
        get(
            &app,
            tenant,
            &format!(
                "/bss-products/v1/categories?$filter={}",
                enc("archived eq true")
            ),
        )
        .await,
    )
    .await;
    assert_eq!(listed(&page), ["old"], "{page}");
    let r = get(&app, tenant, &old_url).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert!(body_json(r).await["archived_at"].is_string());

    let tag = etag_of(&app, tenant, &old_url).await;
    let (status, _, body) =
        post_if_match(&app, tenant, &format!("{old_url}/unarchive"), Some(&tag)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["archived_at"].is_null(), "{body}");
    let page = body_json(get(&app, tenant, "/bss-products/v1/categories").await).await;
    assert_eq!(listed(&page).len(), 2, "{page}");
    for action in ["category.archive", "category.unarchive"] {
        assert_eq!(
            raw_i64(
                &dsn,
                &format!("SELECT COUNT(*) AS v FROM products_audit_log WHERE action = '{action}'")
            )
            .await,
            1,
            "{action}"
        );
    }
}

/// The archive and unarchive doors answer the new version in `ETag`, which the next write sends
/// as If-Match, and the served spec says so on each 200.
#[tokio::test]
async fn the_archive_doors_declare_the_etag_of_their_200() {
    let (db, _, _, _dsn) = crate::test_support::test_db().await;
    let (_, state) = crate::test_support::rest_app_on_db(
        Uuid::new_v4(),
        doors,
        crate::test_support::resolved_usage_types(),
        "test",
        db,
    )
    .await;
    let registry = toolkit::api::OpenApiRegistryImpl::new();
    let _doors = doors(state, &registry);
    let spec = serde_json::to_value(
        registry
            .build_openapi(&toolkit::api::OpenApiInfo::default())
            .unwrap(),
    )
    .unwrap();
    for path in [
        "/bss-products/v1/skus/{id}/archive",
        "/bss-products/v1/skus/{id}/unarchive",
        "/bss-products/v1/categories/{id}/archive",
        "/bss-products/v1/categories/{id}/unarchive",
    ] {
        let headers = &spec["paths"][path]["post"]["responses"]["200"]["headers"];
        assert!(headers.get("ETag").is_some(), "{path}: {headers}");
    }
}
