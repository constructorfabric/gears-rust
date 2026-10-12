#![allow(clippy::expect_used, clippy::unwrap_used)]
//! P-D-207: the usage-type picker, served by products and read as the caller.
use super::router;
use crate::infra::usage_types::{LocalDevStaticUsageTypes, UnconfiguredUsageTypes};
use crate::test_support::{
    EmptyUsageTypes, PluginLikeCollector, UnreachableUsageTypes, body_json,
    denying_collector_catalog, get, plugin_like_catalog, rest_app_with_catalog,
};
use async_trait::async_trait;
use axum::http::StatusCode;
use bss_products_sdk::usage_types::{
    UsageTypeAnswer, UsageTypeBinding, UsageTypeCatalog, UsageTypePage,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use usage_collector_sdk::UsageKind;
use uuid::Uuid;

const PICKER: &str = "/bss-products/v1/usage-types";

/// One `list` call: `q`, `kind`, `limit`, `cursor`.
type Asked = (Option<String>, Option<String>, u32, Option<String>);
/// A catalog that records what it was asked and answers one page.
#[derive(Default)]
struct Recording {
    asked: Mutex<Vec<Asked>>,
}

#[async_trait]
impl UsageTypeCatalog for Recording {
    async fn resolve(&self, _: &SecurityContext, _: &str) -> UsageTypeAnswer {
        UsageTypeAnswer::Unavailable
    }
    async fn list(
        &self,
        _: &SecurityContext,
        q: Option<&str>,
        kind: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<UsageTypePage, CanonicalError> {
        self.asked.lock().unwrap().push((
            q.map(ToOwned::to_owned),
            kind.map(ToOwned::to_owned),
            limit,
            cursor.map(ToOwned::to_owned),
        ));
        Ok(UsageTypePage {
            items: vec![UsageTypeBinding {
                gts_id: "gts.cf.core.uc.usage_record.v1~cf.bss.usage_type.storage.v1".into(),
                kind: "counter".into(),
                metadata_fields: vec!["region".into()],
            }],
            next_cursor: Some("next".into()),
            prev_cursor: None,
            limit: limit.min(100),
        })
    }
}

#[tokio::test]
async fn the_picker_lists_the_catalog_page_with_its_source() {
    let tenant = Uuid::new_v4();
    let catalog = Arc::new(Recording::default());
    let (app, _) = rest_app_with_catalog(tenant, router, catalog.clone(), "registry").await;
    let r = get(
        &app,
        tenant,
        &format!("{PICKER}?q=storage&kind=counter&limit=150&cursor=abc"),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        body_json(r).await,
        json!({
            "source": "registry",
            "items": [{
                "gts_id": "gts.cf.core.uc.usage_record.v1~cf.bss.usage_type.storage.v1",
                "kind": "counter",
                "metadata_fields": ["region"],
            }],
            "page_info": {"next_cursor": "next", "prev_cursor": null, "limit": 100},
        })
    );
    // No parameters: the default page of 50; a limit past 200 is clamped, never refused.
    assert_eq!(get(&app, tenant, PICKER).await.status(), StatusCode::OK);
    assert_eq!(
        get(&app, tenant, &format!("{PICKER}?limit=5000"))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        *catalog.asked.lock().unwrap(),
        vec![
            (
                Some("storage".to_owned()),
                Some("counter".to_owned()),
                150,
                Some("abc".to_owned())
            ),
            (None, None, 50, None),
            (None, None, 200, None),
        ]
    );
    for bad in ["limit=0", "limit=many", "limit=-1"] {
        let r = get(&app, tenant, &format!("{PICKER}?{bad}")).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(
        catalog.asked.lock().unwrap().len(),
        3,
        "a refused query asks nobody"
    );
}

#[tokio::test]
async fn the_picker_tells_no_catalog_from_an_empty_or_unreachable_one() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app_with_catalog(
        tenant,
        router,
        Arc::new(UnconfiguredUsageTypes),
        "unconfigured",
    )
    .await;
    assert_eq!(
        get(&app, tenant, PICKER).await.status(),
        StatusCode::NOT_IMPLEMENTED
    );
    let (app, _) = rest_app_with_catalog(tenant, router, Arc::new(EmptyUsageTypes), "test").await;
    let r = get(&app, tenant, PICKER).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(body_json(r).await["items"], json!([]));
    let (app, _) =
        rest_app_with_catalog(tenant, router, Arc::new(UnreachableUsageTypes), "test").await;
    assert_eq!(
        get(&app, tenant, PICKER).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (app, _) = rest_app_with_catalog(
        tenant,
        router,
        Arc::new(LocalDevStaticUsageTypes),
        "local_dev_static",
    )
    .await;
    let r = get(&app, tenant, &format!("{PICKER}?q=storage")).await;
    assert_eq!(r.status(), StatusCode::OK);
    let b = body_json(r).await;
    assert_eq!(b["source"], "local_dev_static");
    assert_eq!(b["items"].as_array().unwrap().len(), 1, "{b}");
}

/// Read as the caller: a collector that refuses the caller answers 403, not 503.
#[tokio::test]
async fn a_collector_denial_of_the_picker_is_403() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app_with_catalog(
        tenant,
        router,
        denying_collector_catalog(),
        "usage_collector",
    )
    .await;
    let r = get(&app, tenant, PICKER).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// `q` against the collector as its real plugin serves it (P-D-207).
// ---------------------------------------------------------------------------

/// The base every usage type derives from.
const BASE: &str = "gts.cf.core.uc.usage_record.v1~";
/// The adapter's search cap.
const SEARCH_CAP: usize = crate::infra::usage_types::USAGE_TYPE_SEARCH_CAP;

fn id(leaf: &str) -> String {
    format!("{BASE}cf.bss.usage_type.{leaf}.v1")
}

async fn ask(app: &axum::Router, tenant: Uuid, uri: &str) -> (StatusCode, serde_json::Value) {
    let r = get(app, tenant, uri).await;
    let status = r.status();
    (status, body_json(r).await)
}

fn ids(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("a page has items: {body}"))
        .iter()
        .map(|i| i["gts_id"].as_str().unwrap().to_owned())
        .collect()
}

async fn collector_app(tenant: Uuid, collector: &Arc<PluginLikeCollector>) -> axum::Router {
    rest_app_with_catalog(
        tenant,
        router,
        plugin_like_catalog(Arc::clone(collector)),
        "usage_collector",
    )
    .await
    .0
}

/// **The picker's `q` never asks the collector for an operator its plugin lacks.** The
/// collector's timescaledb plugin translates comparison operators only; products sent `contains(gts_id,…)`
/// and the picker answered 503 `internal error: unsupported operator: Contains`. The collector is
/// asked with `kind eq` at most; products narrows by `q` itself, case-insensitively.
#[tokio::test]
async fn the_picker_searches_the_real_collector_with_equality_only() {
    let tenant = Uuid::new_v4();
    let collector = Arc::new(PluginLikeCollector::new([
        (id("storage").as_str(), UsageKind::Counter),
        (id("storage_iops").as_str(), UsageKind::Gauge),
        (id("vcpuhours").as_str(), UsageKind::Counter),
        (id("requests").as_str(), UsageKind::Counter),
    ]));
    let app = collector_app(tenant, &collector).await;

    let (status, body) = ask(&app, tenant, &format!("{PICKER}?q=storage")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"], "usage_collector", "{body}");
    assert_eq!(
        ids(&body),
        vec![id("storage"), id("storage_iops")],
        "{body}"
    );
    assert_eq!(body["items"][0]["kind"], "counter", "{body}");
    assert_eq!(
        body["items"][0]["metadata_fields"],
        json!(["region"]),
        "{body}"
    );
    assert_eq!(body["page_info"]["next_cursor"], json!(null), "{body}");

    let (status, body) = ask(&app, tenant, &format!("{PICKER}?q=STORAGE")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        ids(&body),
        vec![id("storage"), id("storage_iops")],
        "q folds case: {body}"
    );

    let (status, body) = ask(&app, tenant, &format!("{PICKER}?q=storage&kind=gauge")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec![id("storage_iops")], "{body}");

    let (status, body) = ask(&app, tenant, &format!("{PICKER}?q=nothing")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["items"],
        json!([]),
        "no match is an empty page: {body}"
    );

    let kind_eq = format!(
        "{:?}",
        toolkit_odata::parse_filter_string("kind eq 'gauge'")
            .unwrap()
            .into_expr()
    );
    for asked in collector.asked() {
        let filter = asked.filter().map(|e| format!("{e:?}"));
        assert!(
            filter.is_none() || filter.as_deref() == Some(kind_eq.as_str()),
            "the collector is asked with `kind eq` at most, never a function: {filter:?}"
        );
    }
}

/// The matches are paged by products with a cursor of its own, bound to `q` and `kind`: a cursor
/// replayed with other values is 400, never a page of another search and never a 503. The walk
/// crosses the collector's own pages when its ceiling is lower than the catalog.
#[tokio::test]
async fn the_picker_pages_its_matches_with_a_cursor_bound_to_q_and_kind() {
    let tenant = Uuid::new_v4();
    let leaves = [
        "meter_a", "meter_b", "meter_c", "meter_d", "meter_e", "other_a", "other_b",
    ];
    let owned: Vec<String> = leaves.iter().map(|l| id(l)).collect();
    let collector = Arc::new(
        PluginLikeCollector::new(owned.iter().map(|i| (i.as_str(), UsageKind::Counter)))
            .with_ceiling(2),
    );
    let app = collector_app(tenant, &collector).await;

    let mut seen = Vec::new();
    let mut uri = format!("{PICKER}?q=meter&limit=2");
    let mut first_cursor = None;
    for page in 0.. {
        assert!(page < 5, "the matches end: {seen:?}");
        let (status, body) = ask(&app, tenant, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["page_info"]["limit"], 2, "{body}");
        seen.extend(ids(&body));
        match body["page_info"]["next_cursor"].as_str() {
            Some(cursor) => {
                first_cursor.get_or_insert_with(|| cursor.to_owned());
                uri = format!("{PICKER}?q=meter&limit=2&cursor={cursor}");
            }
            None => break,
        }
    }
    let expected: Vec<String> = leaves[..5].iter().map(|l| id(l)).collect();
    assert_eq!(seen, expected, "every match once, in gts_id order");
    assert!(
        collector.asked().len() > 3,
        "the walk crossed the collector's pages: {}",
        collector.asked().len()
    );

    let cursor = first_cursor.expect("five matches at two a page mint a cursor");
    for replay in [
        format!("{PICKER}?q=other&limit=2&cursor={cursor}"),
        format!("{PICKER}?q=METER&limit=2&cursor={cursor}"),
        format!("{PICKER}?q=meter&kind=counter&limit=2&cursor={cursor}"),
        format!("{PICKER}?limit=2&cursor={cursor}"),
    ] {
        let (status, body) = ask(&app, tenant, &replay).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{replay}: {body}");
    }

    // The collector's own cursor, from a page without `q`, is not a search's.
    let (status, body) = ask(&app, tenant, &format!("{PICKER}?limit=2")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let passthrough = body["page_info"]["next_cursor"]
        .as_str()
        .expect("seven types at two a page")
        .to_owned();
    let (status, body) = ask(
        &app,
        tenant,
        &format!("{PICKER}?limit=2&cursor={passthrough}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "without q the collector pages itself: {body}"
    );
    assert_eq!(ids(&body), vec![id("meter_c"), id("meter_d")], "{body}");
    let (status, body) = ask(
        &app,
        tenant,
        &format!("{PICKER}?q=meter&limit=2&cursor={passthrough}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// The search walks at most the cap: past it the picker answers 503
/// `USAGE_TYPE_CATALOG_TOO_LARGE` rather than a page it did not search; at the cap it answers.
/// `kind` narrows the walk at the collector, so it counts what the collector returns for it.
/// Without `q` the collector still pages a catalog of any size.
#[tokio::test]
async fn past_the_cap_a_search_is_503_and_a_passthrough_still_pages() {
    let tenant = Uuid::new_v4();
    let many = |n: usize| -> Vec<String> { (0..n).map(|i| id(&format!("t{i:04}"))).collect() };

    let over = many(SEARCH_CAP + 1);
    let mut types: Vec<(&str, UsageKind)> = over
        .iter()
        .map(|i| (i.as_str(), UsageKind::Counter))
        .collect();
    let gauge = id("gauge_one");
    types.push((gauge.as_str(), UsageKind::Gauge));
    let collector = Arc::new(PluginLikeCollector::new(types));
    let app = collector_app(tenant, &collector).await;
    let (status, body) = ask(&app, tenant, &format!("{PICKER}?q=t0001")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body.to_string().contains("USAGE_TYPE_CATALOG_TOO_LARGE"),
        "{body}"
    );
    let (status, body) = ask(&app, tenant, &format!("{PICKER}?q=gauge&kind=gauge")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec![gauge.clone()], "{body}");
    let (status, body) = ask(&app, tenant, &format!("{PICKER}?limit=200")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 200, "{body}");
    assert!(body["page_info"]["next_cursor"].is_string(), "{body}");

    let at = many(SEARCH_CAP);
    let collector = Arc::new(PluginLikeCollector::new(
        at.iter().map(|i| (i.as_str(), UsageKind::Counter)),
    ));
    let app = collector_app(tenant, &collector).await;
    let (status, body) = ask(&app, tenant, &format!("{PICKER}?q=t0999")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(ids(&body), vec![id("t0999")], "{body}");
}

/// P-D-207's refusals hold for a search as for a page: a denial is 403, a catalog store down
/// in the middle of the walk is 503 and never a partial page, no catalog is 501.
#[tokio::test]
async fn a_search_keeps_the_pickers_refusals() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app_with_catalog(
        tenant,
        router,
        denying_collector_catalog(),
        "usage_collector",
    )
    .await;
    assert_eq!(
        get(&app, tenant, &format!("{PICKER}?q=storage"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    let leaves = ["a_one", "a_two", "a_three", "a_four", "a_five"];
    let owned: Vec<String> = leaves.iter().map(|l| id(l)).collect();
    let collector = Arc::new(
        PluginLikeCollector::new(owned.iter().map(|i| (i.as_str(), UsageKind::Counter)))
            .with_ceiling(2)
            .failing_from(1),
    );
    let app = collector_app(tenant, &collector).await;
    let (status, body) = ask(&app, tenant, &format!("{PICKER}?q=a_")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        collector.asked().len(),
        2,
        "the first collector page answered and the second did not: {body}"
    );

    let (app, _) = rest_app_with_catalog(
        tenant,
        router,
        Arc::new(UnconfiguredUsageTypes),
        "unconfigured",
    )
    .await;
    assert_eq!(
        get(&app, tenant, &format!("{PICKER}?q=storage"))
            .await
            .status(),
        StatusCode::NOT_IMPLEMENTED
    );
}

/// RS-39: `kind` is the collector's closed set, `counter` or `gauge`. Any other value is a 400
/// before a catalog is asked, so it never reaches the collector's filter.
#[tokio::test]
async fn a_kind_outside_the_closed_set_is_400_and_asks_nobody() {
    let tenant = Uuid::new_v4();
    let catalog = Arc::new(Recording::default());
    let (app, _) = rest_app_with_catalog(tenant, router, catalog.clone(), "registry").await;
    for bad in ["bogus", "Counter", "counter'%20or%20kind%20eq%20'gauge"] {
        let r = get(&app, tenant, &format!("{PICKER}?kind={bad}")).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    assert!(
        catalog.asked.lock().unwrap().is_empty(),
        "a refused kind asks nobody"
    );
    for kind in ["counter", "gauge"] {
        assert_eq!(
            get(&app, tenant, &format!("{PICKER}?kind={kind}"))
                .await
                .status(),
            StatusCode::OK,
            "{kind}"
        );
    }
}

/// P-D-247 (ask 56): a page of the picker carries `Cache-Control: private, max-age=60`. It is read
/// as the caller, so only the caller's own cache may keep it, for a minute; a refusal carries no
/// cache header.
#[tokio::test]
async fn a_picker_page_may_be_kept_privately_for_a_minute() {
    let tenant = Uuid::new_v4();
    let catalog = Arc::new(Recording::default());
    let (app, _) = rest_app_with_catalog(tenant, router, catalog, "registry").await;
    let cache = |r: &axum::response::Response| {
        r.headers()
            .get(axum::http::header::CACHE_CONTROL)
            .map(|v| v.to_str().unwrap().to_owned())
    };
    for uri in [
        PICKER.to_owned(),
        format!("{PICKER}?q=storage&kind=counter&limit=5"),
    ] {
        let r = get(&app, tenant, &uri).await;
        assert_eq!(r.status(), StatusCode::OK, "{uri}");
        assert_eq!(cache(&r).as_deref(), Some("private, max-age=60"), "{uri}");
    }
    let refused = get(&app, tenant, &format!("{PICKER}?kind=bogus")).await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert_eq!(cache(&refused), None, "a refusal is not kept");
}
