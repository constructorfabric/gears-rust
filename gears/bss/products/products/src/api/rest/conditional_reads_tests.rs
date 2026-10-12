//! Conditional GET on the SKU list, its counts, the derived-type list and the category list
//! (P-D-261).
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::api::rest::{ApiState, categories, derived_usage_types, skus};
use crate::domain::category::NewCategory;
use crate::domain::sku::NewSku;
use crate::infra::storage::repo;
use crate::test_support::{resolved_usage_types, rest_app_on_db, tenant_user, test_db};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use bss_products_sdk::models::SkuType;
use bss_products_sdk::sku_usage::{SkuUsage, SkuUsageSets, SkuUsageV1, UsageScope};
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit::api::OpenApiRegistry;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn doors(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    categories::router(Arc::clone(&state), openapi)
        .merge(skus::router(Arc::clone(&state), openapi))
        .merge(derived_usage_types::router(state, openapi))
}

struct Answer {
    status: StatusCode,
    etag: String,
    cache_control: String,
    body: Vec<u8>,
}

async fn get(app: &Router, ctx: &SecurityContext, path: &str, tag: Option<&str>) -> Answer {
    let mut request = Request::builder()
        .method("GET")
        .uri(path)
        .extension(ctx.clone());
    if let Some(tag) = tag {
        request = request.header(header::IF_NONE_MATCH, tag);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let cache_control = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Answer {
        status,
        etag,
        cache_control,
        body,
    }
}

async fn revalidates(app: &Router, ctx: &SecurityContext, path: &str, cache: &str) -> String {
    let first = get(app, ctx, path, None).await;
    assert_eq!(
        first.status,
        StatusCode::OK,
        "{path}: {}",
        String::from_utf8_lossy(&first.body)
    );
    assert_eq!(first.cache_control, cache, "{path}");
    assert!(first.etag.starts_with("W/\""), "{path}: {}", first.etag);
    let again = get(app, ctx, path, Some(&first.etag)).await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED, "{path}");
    assert!(again.body.is_empty(), "{path}");
    assert_eq!(again.etag, first.etag, "{path}");
    assert_eq!(again.cache_control, cache, "{path}");
    let listed = format!("\"other\", {}", first.etag);
    let in_list = get(app, ctx, path, Some(&listed)).await;
    assert_eq!(in_list.status, StatusCode::NOT_MODIFIED, "{path}: a list");
    let star = get(app, ctx, path, Some("*")).await;
    assert_eq!(star.status, StatusCode::NOT_MODIFIED, "{path}: *");
    let other = get(app, ctx, path, Some("W/\"other\"")).await;
    assert_eq!(other.status, StatusCode::OK, "{path}: an unrelated tag");
    assert_eq!(other.etag, first.etag, "{path}");
    first.etag
}

/// A derived usage type, created through its door (P-D-231).
async fn create_derived(app: &Router, ctx: &SecurityContext, code: &str) {
    let ram = "gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1";
    let body = serde_json::json!({
        "code": code,
        "name": format!("{code} name"),
        "declaration": {
            "output_unit": "cloudlet\u{b7}hour",
            "granularity": "hour",
            "inputs": [
                {"name": "ram_mb", "usage_type_ref": ram, "granule_fold": "peak", "unit": "MB"}
            ],
            "formula": {"op": "ceil", "arg": {
                "op": "div_const", "arg": {"op": "input", "name": "ram_mb"}, "divisor": "128"
            }},
            "output_scale": 0,
            "output_round": "half_even"
        }
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/bss-products/v1/derived-usage-types")
                .header(header::CONTENT_TYPE, "application/json")
                .extension(ctx.clone())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

#[tokio::test]
async fn the_list_reads_answer_304_until_the_page_changes() {
    let (db, scope, tenant, dsn) = test_db().await;
    let (app, state) = rest_app_on_db(tenant, doors, resolved_usage_types(), "test", db).await;
    let ctx = tenant_user(tenant);
    let _keep = dsn;
    let skus_tag = revalidates(&app, &ctx, "/bss-products/v1/skus", "private, no-cache").await;
    let counts_tag = revalidates(
        &app,
        &ctx,
        "/bss-products/v1/skus/counts",
        "private, no-cache",
    )
    .await;
    let derived_tag = revalidates(
        &app,
        &ctx,
        "/bss-products/v1/derived-usage-types",
        "private, no-cache",
    )
    .await;
    let categories_tag = revalidates(
        &app,
        &ctx,
        "/bss-products/v1/categories",
        "private, no-cache",
    )
    .await;

    let conn = state.db.conn().unwrap();
    let category = repo::insert_category(
        &conn,
        &scope,
        tenant,
        NewCategory {
            code: "general".into(),
            name: "General".into(),
            is_default: false,
            sort_order: 0,
        },
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    repo::insert_sku(
        &conn,
        &scope,
        tenant,
        NewSku {
            code: "sku-1".into(),
            name: "SKU 1".into(),
            r#type: SkuType::Recurring,
            category_id: Some(category.id),
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: None,
            unit: None,
        },
        ctx.subject_id(),
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();

    for (path, old) in [
        ("/bss-products/v1/skus", skus_tag),
        ("/bss-products/v1/skus/counts", counts_tag),
        ("/bss-products/v1/categories", categories_tag),
    ] {
        let fresh = get(&app, &ctx, path, Some(&old)).await;
        assert_eq!(fresh.status, StatusCode::OK, "{path}");
        assert_ne!(fresh.etag, old, "{path}");
        assert_eq!(fresh.cache_control, "private, no-cache", "{path}");
    }
    let derived = get(
        &app,
        &ctx,
        "/bss-products/v1/derived-usage-types",
        Some(&derived_tag),
    )
    .await;
    assert_eq!(
        derived.status,
        StatusCode::NOT_MODIFIED,
        "a SKU and a category leave the derived-type page as it was"
    );
    create_derived(&app, &ctx, "cloudlets").await;
    let fresh = get(
        &app,
        &ctx,
        "/bss-products/v1/derived-usage-types",
        Some(&derived_tag),
    )
    .await;
    assert_eq!(fresh.status, StatusCode::OK);
    assert_ne!(fresh.etag, derived_tag);
    assert!(fresh.etag.starts_with("W/\""), "{}", fresh.etag);
    assert_eq!(
        fresh.cache_control, "private, no-cache",
        "the page names its creators, so it revalidates (P-D-261, amended)"
    );
}

/// A caller pricing refuses sees `usage: null`. A caller it answers sees the usage. The tags differ.
struct UsageGrant {
    allowed: Uuid,
}

#[async_trait]
impl SkuUsageV1 for UsageGrant {
    async fn usage(
        &self,
        ctx: &SecurityContext,
        _tenant: Uuid,
        sku_ids: &[Uuid],
    ) -> Result<Vec<SkuUsage>, CanonicalError> {
        if ctx.subject_id() != self.allowed {
            return Err(CanonicalError::internal("pricing refused the usage").create());
        }
        Ok(sku_ids
            .iter()
            .map(|sku_id| SkuUsage {
                sku_id: *sku_id,
                entries: 1,
                ..SkuUsage::default()
            })
            .collect())
    }

    async fn usage_sets(
        &self,
        _ctx: &SecurityContext,
        _tenant: Uuid,
    ) -> Result<SkuUsageSets, CanonicalError> {
        Ok(SkuUsageSets::default())
    }

    async fn sku_ids_in(
        &self,
        _ctx: &SecurityContext,
        _tenant: Uuid,
        _scope: UsageScope,
    ) -> Result<Vec<Uuid>, CanonicalError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn a_refused_usage_port_changes_the_sku_list_tag() {
    let (db, scope, tenant, dsn) = test_db().await;
    let (app, state) = rest_app_on_db(tenant, doors, resolved_usage_types(), "test", db).await;
    let allowed = tenant_user(tenant);
    let refused = SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type(allowed.subject_type().unwrap())
        .build()
        .unwrap();
    let _keep = dsn;
    let conn = state.db.conn().unwrap();
    repo::insert_sku(
        &conn,
        &scope,
        tenant,
        NewSku {
            code: "sku-1".into(),
            name: "SKU 1".into(),
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
        allowed.subject_id(),
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    state.hub.register::<dyn SkuUsageV1>(Arc::new(UsageGrant {
        allowed: allowed.subject_id(),
    }));
    let with_usage = get(&app, &allowed, "/bss-products/v1/skus", None).await;
    let without = get(&app, &refused, "/bss-products/v1/skus", None).await;
    assert_eq!(with_usage.status, StatusCode::OK);
    assert_eq!(without.status, StatusCode::OK);
    let with_text = String::from_utf8_lossy(&with_usage.body);
    let without_text = String::from_utf8_lossy(&without.body);
    assert_ne!(with_usage.etag, without.etag);
    assert!(with_text.contains("\"entries\":1"), "{with_text}");
    assert!(without_text.contains("\"usage\":null"), "{without_text}");
}

/// P-D-261: the served spec declares, on each of the four list doors, `If-None-Match` as an
/// optional header, the weak `ETag` and the `Cache-Control` of the 200, and the 304 with both. All
/// four revalidate: the derived-type list names its creators, so it no longer keeps a minute.
#[tokio::test]
async fn the_four_lists_declare_the_conditional_get() {
    let (db, _, _, _dsn) = test_db().await;
    let (_, state) =
        rest_app_on_db(Uuid::new_v4(), doors, resolved_usage_types(), "test", db).await;
    let registry = toolkit::api::OpenApiRegistryImpl::new();
    let _doors = doors(state, &registry);
    let spec = serde_json::to_value(
        registry
            .build_openapi(&toolkit::api::OpenApiInfo::default())
            .unwrap(),
    )
    .unwrap();
    for (path, cache) in [
        ("/bss-products/v1/skus", "private, no-cache"),
        ("/bss-products/v1/skus/counts", "private, no-cache"),
        ("/bss-products/v1/categories", "private, no-cache"),
        ("/bss-products/v1/derived-usage-types", "private, no-cache"),
    ] {
        let op = &spec["paths"][path]["get"];
        let parameter = op["parameters"]
            .as_array()
            .unwrap_or_else(|| panic!("{path}: no parameters"))
            .iter()
            .find(|param| param["name"] == "If-None-Match")
            .unwrap_or_else(|| panic!("{path}: If-None-Match"));
        assert_eq!(parameter["in"], "header", "{path}");
        assert_eq!(parameter["required"], false, "{path}");
        for status in ["200", "304"] {
            let headers = &op["responses"][status]["headers"];
            assert!(headers.get("ETag").is_some(), "{path} {status}: {headers}");
            assert_eq!(
                headers["Cache-Control"]["description"], cache,
                "{path} {status}: {headers}"
            );
        }
    }
}
