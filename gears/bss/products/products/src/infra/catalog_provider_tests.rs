#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::{
    domain::{category::NewCategory, sku::NewSku},
    infra::storage::repo,
    test_support::*,
};
use bss_products_sdk::models::{Lifecycle, SkuType};
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture covers the published page and the fenced and dated heads"
)]
async fn both_transports_serve_the_same_published_catalog_and_pages() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let now = OffsetDateTime::now_utc();
    let cat = repo::insert_category(
        &conn,
        &scope,
        tenant,
        NewCategory {
            code: "hosting".into(),
            name: "Hosting".into(),
            is_default: true,
            sort_order: 0,
        },
        now,
    )
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (code, name, lifecycle) in [
        ("A", "storage draft", Lifecycle::Draft),
        ("B", "storage", Lifecycle::Published),
        ("C", "storage legacy", Lifecycle::Deprecated),
        ("D", "storage retired", Lifecycle::Retired),
        ("F", "O'Brien_%", Lifecycle::Published),
    ] {
        let s = repo::insert_sku(
            &conn,
            &scope,
            tenant,
            NewSku {
                code: code.into(),
                name: name.into(),
                r#type: SkuType::Usage,
                category_id: Some(cat.id),
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: Some("cloud".into()),
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: Some("storage.bytes".into()),
                unit: Some("GiB".into()),
            },
            tenant,
            now,
        )
        .await
        .unwrap();
        repo::set_lifecycle(
            &conn,
            &scope,
            tenant,
            s.id,
            &[Lifecycle::Draft],
            lifecycle,
            now,
        )
        .await
        .unwrap();
        ids.push(s.id);
    }
    let fenced = repo::insert_sku(
        &conn,
        &scope,
        tenant,
        NewSku {
            code: "G".into(),
            name: "fenced unit".into(),
            r#type: SkuType::Usage,
            category_id: Some(cat.id),
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: Some("cloud".into()),
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: Some("storage.bytes".into()),
            unit: Some("GiB".into()),
        },
        tenant,
        now,
    )
    .await
    .unwrap();
    repo::set_lifecycle(
        &conn,
        &scope,
        tenant,
        fenced.id,
        &[Lifecycle::Draft],
        Lifecycle::Published,
        now,
    )
    .await
    .unwrap();
    repo::fence_sku(
        &conn,
        &scope,
        tenant,
        fenced.id,
        repo::Fence::Retire,
        Uuid::new_v4(),
        now,
    )
    .await
    .unwrap();
    let dated = repo::insert_sku(
        &conn,
        &scope,
        tenant,
        NewSku {
            code: "H".into(),
            name: "dated unit".into(),
            r#type: SkuType::Usage,
            category_id: Some(cat.id),
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: Some("cloud".into()),
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: Some("storage.bytes".into()),
            unit: Some("GiB".into()),
        },
        tenant,
        now,
    )
    .await
    .unwrap();
    repo::set_lifecycle(
        &conn,
        &scope,
        tenant,
        dated.id,
        &[Lifecycle::Draft],
        Lifecycle::Published,
        now,
    )
    .await
    .unwrap();
    repo::set_lifecycle_next(
        &conn,
        &scope,
        tenant,
        dated.id,
        &[Lifecycle::Published],
        Lifecycle::Deprecated,
        now.date().previous_day().unwrap(),
        now,
    )
    .await
    .unwrap();
    let ctx = authed_ctx(tenant);
    let provider = BrowseCatalogProvider::new(db.db(), Arc::new(flat_in_enforcer(tenant)));
    let page = provider
        .search_skus(&ctx, Some("stor"), 1, None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0],
        CatalogSku {
            sku_id: ids[1],
            sku_code: "B".into(),
            name: "storage".into(),
            metering_unit: Some("GiB".into()),
            status: "published".into(),
            plan_tier: None,
            sku_type: "usage".into(),
            sellable: true,
            usage_type_ref: Some("storage.bytes".into()),
            deprecated: false,
        }
    );
    assert_eq!(page.next_cursor.as_deref(), Some("B"));
    let all = provider.get_skus(&ctx, &ids).await.unwrap();
    assert_eq!(all.len(), 3);
    assert!(all[1].deprecated);
    assert_eq!(all[1].status, "deprecated");
    let extra = provider
        .get_skus(&ctx, &[fenced.id, dated.id])
        .await
        .unwrap();
    assert_eq!(extra.len(), 2);
    assert_eq!(extra[0].sku_id, fenced.id);
    assert_eq!(extra[0].status, "published");
    assert!(!extra[0].deprecated);
    assert_eq!(extra[1].sku_id, dated.id);
    assert_eq!(extra[1].status, "deprecated");
    assert!(extra[1].deprecated);
    assert_eq!(
        provider.list_tax_categories(&ctx).await.unwrap(),
        vec![CatalogTaxCategory {
            code: "cloud".into(),
            display_name: "cloud".into()
        }]
    );
    let foreign = authed_ctx(Uuid::new_v4());
    assert!(provider.get_skus(&foreign, &ids).await.unwrap().is_empty());

    let (app, _state) = rest_app_on_db(
        tenant,
        crate::api::rest::browse::router,
        Arc::new(crate::infra::usage_types::UnconfiguredUsageTypes),
        "unconfigured",
        db,
    )
    .await;
    let app = app.layer(axum::Extension(ctx.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = crate::infra::catalog_rest_client::ProductCatalogRestClient::new(
        toolkit::contract_support::runtime::config::ClientConfig::new(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )),
    )
    .unwrap();
    let requests = async {
        assert_eq!(client.get_skus(&ctx, &ids).await.unwrap(), all);
        assert_eq!(
            client
                .search_skus(&ctx, Some("stor"), 1, None)
                .await
                .unwrap(),
            page
        );
        let next = client
            .search_skus(&ctx, Some("stor"), 1, page.next_cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(
            next,
            provider
                .search_skus(&ctx, Some("stor"), 1, page.next_cursor.as_deref())
                .await
                .unwrap()
        );
        assert_eq!(next.items[0].sku_code, "C");
        assert!(next.next_cursor.is_none());
        assert_eq!(
            client
                .search_skus(&ctx, Some("O'Brien_%"), 50, None)
                .await
                .unwrap(),
            provider
                .search_skus(&ctx, Some("O'Brien_%"), 50, None)
                .await
                .unwrap()
        );
        assert_eq!(
            client.list_tax_categories(&ctx).await.unwrap(),
            provider.list_tax_categories(&ctx).await.unwrap()
        );
    };
    tokio::select! {
        () = requests => {},
        result = axum::serve(listener, app) => { result.unwrap(); panic!("server ended before requests"); }
    }
}

/// `search_skus` sends the caller's text as `startswith(name, …)`: a `%`, `_` or `\` in it is a
/// literal on both dialects, because the toolkit's `LIKE` carries `ESCAPE '\'` (it had none, and
/// `SQLite` has no default escape, so such a search matched nothing).
#[tokio::test]
async fn a_wildcard_in_the_search_text_is_a_literal() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let now = OffsetDateTime::now_utc();
    for (code, name) in [
        ("A", "O'Brien_%"),
        ("B", "O'BrienX%"),
        ("C", "O'Brien_x"),
        ("D", r"back\slash"),
        ("E", "backXslash"),
    ] {
        let s = repo::insert_sku(
            &conn,
            &scope,
            tenant,
            NewSku {
                code: code.into(),
                name: name.into(),
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
            tenant,
            now,
        )
        .await
        .unwrap();
        repo::set_lifecycle(
            &conn,
            &scope,
            tenant,
            s.id,
            &[Lifecycle::Draft],
            Lifecycle::Published,
            now,
        )
        .await
        .unwrap();
    }
    let ctx = authed_ctx(tenant);
    let provider = BrowseCatalogProvider::new(db.db(), Arc::new(flat_in_enforcer(tenant)));
    for (text, expected) in [
        ("O'Brien_%", vec!["A"]),
        ("O'Brien_", vec!["A", "C"]),
        ("O'Brien", vec!["A", "B", "C"]),
        (r"back\", vec!["D"]),
    ] {
        let page = provider
            .search_skus(&ctx, Some(text), 50, None)
            .await
            .unwrap();
        let codes: Vec<&str> = page.items.iter().map(|s| s.sku_code.as_str()).collect();
        assert_eq!(codes, expected, "search {text}");
    }
}

/// RS-06: a refusal the browse door answers keeps its class through the REST client: a caller the
/// door refused stays 403, a query it refused 400 and a missing row 404, never a retryable 503.
/// A 5xx other than 503 stays the catalog that did not answer.
#[test]
fn a_browse_refusal_keeps_its_class_through_the_rest_client() {
    use crate::infra::catalog_rest_client::catalog_error_from_http;
    use toolkit_canonical_errors::Problem;
    for refusal in [
        SkuResource::permission_denied()
            .with_reason("a PDP reason")
            .create(),
        invalid("$filter", "a filter that does not parse"),
        SkuResource::not_found("sku x").with_resource("x").create(),
    ] {
        let status = refusal.status_code();
        let body = serde_json::to_vec(&Problem::from(refusal)).unwrap();
        let error = catalog_error_from_http(status, None, &body);
        assert_eq!(error.status_code(), status);
    }
    assert_eq!(
        catalog_error_from_http(502, None, b"bad gateway").status_code(),
        503
    );
}

/// RS-07 / RS-09: a storage failure behind the catalog is the repository's logged 500, with the
/// driver's text kept off the wire; it was a retryable 503 whose detail was the driver's message.
#[tokio::test]
async fn a_storage_failure_behind_the_catalog_is_a_500_without_driver_text() {
    use toolkit_canonical_errors::Problem;
    let (db, _scope, tenant, dsn) = test_db().await;
    drop_table(&dsn, "products_sku").await;
    let ctx = authed_ctx(tenant);
    let provider = BrowseCatalogProvider::new(db.db(), Arc::new(flat_in_enforcer(tenant)));
    for error in [
        provider
            .search_skus(&ctx, Some("x"), 10, None)
            .await
            .unwrap_err(),
        provider
            .get_skus(&ctx, &[Uuid::new_v4()])
            .await
            .unwrap_err(),
        provider.list_tax_categories(&ctx).await.unwrap_err(),
    ] {
        assert_eq!(error.status_code(), 500);
        let wire = serde_json::to_string(&Problem::from(error)).unwrap();
        assert!(!wire.contains("products_sku"), "{wire}");
    }
}

/// `n` published usage SKUs of the tenant, their tax categories cycling over three; their ids.
async fn published_skus(
    db: &toolkit_db::DBProvider<toolkit_db::DbError>,
    scope: &toolkit_db::secure::AccessScope,
    tenant: Uuid,
    n: usize,
) -> Vec<Uuid> {
    let conn = db.conn().unwrap();
    let now = OffsetDateTime::now_utc();
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let s = repo::insert_sku(
            &conn,
            scope,
            tenant,
            NewSku {
                code: format!("P{i:04}"),
                name: format!("published {i:04}"),
                r#type: SkuType::Recurring,
                category_id: None,
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: Some(format!("tax-{}", i % 3)),
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: None,
                unit: None,
            },
            tenant,
            now,
        )
        .await
        .unwrap();
        repo::set_lifecycle(
            &conn,
            scope,
            tenant,
            s.id,
            &[Lifecycle::Draft],
            Lifecycle::Published,
            now,
        )
        .await
        .unwrap();
        ids.push(s.id);
    }
    ids
}

/// RS-40: `get_skus` reads the SKUs a write names in one statement, whatever their number.
#[tokio::test]
async fn get_skus_reads_in_the_same_statements_for_10_and_100_ids() {
    let mut runs = Vec::new();
    for n in [10, 100] {
        let (db, scope, tenant, _dsn, recorder) = recorded_test_db().await;
        let ids = published_skus(&db, &scope, tenant, n).await;
        let provider = BrowseCatalogProvider::new(db.db(), Arc::new(flat_in_enforcer(tenant)));
        recorder.clear();
        let skus = provider.get_skus(&authed_ctx(tenant), &ids).await.unwrap();
        assert_eq!(skus.len(), n);
        let codes: Vec<&str> = skus.iter().map(|s| s.sku_code.as_str()).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted, "the SKUs come back in code order");
        runs.push(products_statements(&recorder));
    }
    assert_eq!(runs[0], runs[1]);
}

/// RS-14: the tax-category dictionary is one distinct read over the published SKUs, whatever
/// their number; it paged every published row 200 at a time.
#[tokio::test]
async fn tax_categories_read_in_the_same_statements_for_10_and_250_skus() {
    let mut runs = Vec::new();
    for n in [10, 250] {
        let (db, scope, tenant, _dsn, recorder) = recorded_test_db().await;
        published_skus(&db, &scope, tenant, n).await;
        let provider = BrowseCatalogProvider::new(db.db(), Arc::new(flat_in_enforcer(tenant)));
        recorder.clear();
        let dictionary = provider
            .list_tax_categories(&authed_ctx(tenant))
            .await
            .unwrap();
        assert_eq!(
            dictionary
                .iter()
                .map(|c| c.code.as_str())
                .collect::<Vec<_>>(),
            ["tax-0", "tax-1", "tax-2"]
        );
        runs.push(products_statements(&recorder));
    }
    assert_eq!(runs[0], runs[1]);
}
