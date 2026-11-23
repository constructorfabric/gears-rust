//! The catalog reads (P-D-270, P-D-271, P-D-272): the versions applicable to a SKU version,
//! enabled ones by default and all with their reasons, a cursor bound to the tenant, the filter and
//! the availability, text in the asked locale or English, the exact version as stored, and no
//! registry call; a PDP outage is 503 and a stored row that does not decode a 500.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::sync::Arc;

use super::definitions::DEFINITIONS;
use crate::authz::labels;
use crate::infra::storage::repo::quota_catalog_repo::insert_availability;
use crate::quota_test_support::{
    FakeQuotaRegistry, LogCapture, QuotaApp, QuotaFakes, VCPU_METRIC, VM_TYPE, quota_app,
    quota_app_for, refusal, registered, seed_sku_version, vcpu_record, vcpu_record_numbered,
};
use crate::test_support::{body_json, get, problem_code, repo_connection};
use async_trait::async_trait;
use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
use bss_products_sdk::quota::catalog::{QuotaAvailabilityState, QuotaVersion};
use sea_orm::{ConnectionTrait, Database};
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

/// The quota endpoints for one tenant, with the catalog's helpers.
struct CatalogFixture {
    q: QuotaApp,
    tenant: Uuid,
}

impl CatalogFixture {
    async fn new() -> Self {
        let tenant = Uuid::new_v4();
        Self {
            q: quota_app(tenant, QuotaFakes::default()).await,
            tenant,
        }
    }

    fn app(&self) -> &axum::Router {
        &self.q.app
    }

    fn registry(&self) -> &FakeQuotaRegistry {
        &self.q.registry
    }

    /// A SKU of the tenant with versions 1 … `n`.
    async fn sku_with_versions(&self, n: i64) -> Uuid {
        let code = crate::quota_test_support::fresh_code();
        seed_sku_version(&self.q.dsn, self.tenant, &code, n).await
    }

    /// `record` authored and submitted by the tenant's user and approved by another subject.
    async fn approved(&self, record: Value, sku: Uuid, sku_version: i64) -> (Uuid, QuotaVersion) {
        self.q.approved(self.tenant, record, sku, sku_version).await
    }

    /// The definition of the last approval, read from its audit row.
    async fn last_definition(&self) -> Uuid {
        let reason = crate::test_support::raw_string_opt(
            &self.q.dsn,
            "SELECT reason AS v FROM products_audit_log WHERE action = \
             'quota_candidate.approve' ORDER BY written_at DESC LIMIT 1",
        )
        .await
        .unwrap();
        // "definition <id> version <n>"
        Uuid::parse_str(reason.split(' ').nth(1).unwrap()).unwrap()
    }

    /// Availability revision 2 of `definition` `version`, enabled: test-only, since no endpoint
    /// enables a version.
    async fn enable(&self, definition: Uuid, version: QuotaVersion) {
        let (db, scope) = repo_connection(&self.q.dsn, self.tenant).await;
        insert_availability(
            &db.conn().unwrap(),
            &scope.tenant_only(),
            self.tenant,
            &crate::domain::quota::Availability {
                definition_id: definition,
                version,
                revision: QuotaVersion::new(2).unwrap(),
                state: QuotaAvailabilityState::Enabled,
                reason_code: None,
                qualification: None,
                observed_at: crate::infra::storage::stored_now(),
                decided_by: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    }
}

fn list_uri(sku: Uuid, version: u64, extra: &str) -> String {
    format!("{DEFINITIONS}?$filter=sku_id eq {sku} and sku_version eq '{version}'{extra}")
        .replace(' ', "%20")
        .replace('\'', "%27")
}

fn version_uri(definition: Uuid, version: u64) -> String {
    format!("{DEFINITIONS}/{definition}/versions/{version}")
}

/// Another subject of `tenant` approves `candidate`.
async fn approve(
    q: &QuotaApp,
    tenant: Uuid,
    candidate: &(Uuid, String),
) -> axum::response::Response {
    crate::test_support::request_as(
        &q.app,
        &crate::test_support::authed_ctx(tenant),
        axum::http::Method::POST,
        &format!("{}/{}/approve", super::CANDIDATES, candidate.0),
        Some(json!({})),
        Some(&candidate.1),
    )
    .await
}

fn items(page: &Value) -> &Vec<Value> {
    page["items"].as_array().unwrap()
}

#[tokio::test]
async fn the_list_answers_the_versions_applicable_to_the_sku_version() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(4).await;
    let (d, _) = f.approved(vcpu_record(), sku, 2).await;
    for (version, expected) in [(1, 0), (2, 1), (4, 1)] {
        let page = body_json(
            get(
                f.app(),
                f.tenant,
                &list_uri(sku, version, "&availability=all"),
            )
            .await,
        )
        .await;
        assert_eq!(items(&page).len(), expected, "sku version {version}");
    }
    let page =
        body_json(get(f.app(), f.tenant, &list_uri(sku, 4, "&availability=all")).await).await;
    assert_eq!(page["items"][0]["definition_id"], json!(d));
    assert_eq!(
        page["items"][0]["sku_version"], "2",
        "the version it was approved against"
    );
    assert!(page["page_info"].get("limit").is_some(), "{page}");
}

#[tokio::test]
async fn the_default_list_is_enabled_only_and_all_carries_reasons() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let (d1, v1) = f.approved(vcpu_record_numbered(1), sku, 1).await;
    let (_d2, _v2) = f.approved(vcpu_record_numbered(2), sku, 1).await;
    let default = body_json(get(f.app(), f.tenant, &list_uri(sku, 1, "")).await).await;
    assert!(
        items(&default).is_empty(),
        "nothing is enabled before qualification"
    );
    f.enable(d1, v1).await;
    let default = body_json(get(f.app(), f.tenant, &list_uri(sku, 1, "")).await).await;
    assert_eq!(items(&default).len(), 1);
    assert_eq!(default["items"][0]["availability"]["state"], "enabled");
    let all = body_json(get(f.app(), f.tenant, &list_uri(sku, 1, "&availability=all")).await).await;
    let reasons: Vec<_> = items(&all)
        .iter()
        .map(|i| i["availability"]["reason_code"].clone())
        .collect();
    assert!(
        reasons.contains(&json!("qualification_pending")) && reasons.contains(&Value::Null),
        "{reasons:?}"
    );
    let (status, code, _) =
        refusal(get(f.app(), f.tenant, &list_uri(sku, 1, "&availability=some")).await).await;
    assert_eq!((status, code.as_str()), (400, "VALIDATION"));
}

#[tokio::test]
async fn a_cursor_with_changed_filters_is_refused() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    for i in 0..3 {
        f.approved(vcpu_record_numbered(i), sku, 1).await;
    }
    let first = body_json(
        get(
            f.app(),
            f.tenant,
            &list_uri(sku, 1, "&availability=all&limit=1"),
        )
        .await,
    )
    .await;
    let cursor = first["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let changed = get(
        f.app(),
        f.tenant,
        &format!("{}&cursor={cursor}", list_uri(sku, 1, "&limit=1")),
    )
    .await;
    assert_eq!(
        changed.status(),
        400,
        "availability changed under the cursor"
    );
    let narrowed = get(
        f.app(),
        f.tenant,
        &format!(
            "{}&cursor={cursor}",
            list_uri(
                sku,
                1,
                &format!(" and resource_type_ref eq '{VM_TYPE}'&availability=all&limit=1")
            )
        ),
    )
    .await;
    assert_eq!(narrowed.status(), 400, "a term added under the cursor");
    let same = get(
        f.app(),
        f.tenant,
        &format!(
            "{}&cursor={cursor}",
            list_uri(sku, 1, "&availability=all&limit=1")
        ),
    )
    .await;
    assert_eq!(same.status(), 200);
    let next = body_json(same).await;
    assert_ne!(
        next["items"][0]["definition_id"],
        first["items"][0]["definition_id"]
    );
}

#[tokio::test]
async fn a_cursor_from_another_tenant_is_refused() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    for i in 0..2 {
        q.approved(tenant, vcpu_record_numbered(i), sku, 1).await;
    }
    let uri = list_uri(sku, 1, "&availability=all&limit=1");
    let first = body_json(get(&q.app, tenant, &uri).await).await;
    let cursor = first["page_info"]["next_cursor"].as_str().unwrap();
    let own = get(&q.app, tenant, &format!("{uri}&cursor={cursor}")).await;
    assert_eq!(own.status(), 200);
    let foreign = get(&q.app, other, &format!("{uri}&cursor={cursor}")).await;
    assert_eq!(foreign.status(), 400);
}

#[tokio::test]
async fn the_list_makes_no_registry_call() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    f.approved(vcpu_record(), sku, 1).await;
    let calls = f.registry().calls();
    let listed = get(
        f.app(),
        f.tenant,
        &list_uri(sku, 1, "&availability=all&locale=ru"),
    )
    .await;
    assert_eq!(listed.status(), 200);
    let read = get(
        f.app(),
        f.tenant,
        &version_uri(f.last_definition().await, 1),
    )
    .await;
    assert_eq!(read.status(), 200);
    assert_eq!(f.registry().calls(), calls);
}

#[tokio::test]
async fn the_definition_answers_the_requested_locale_with_an_english_fallback() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let (d, _) = f.approved(vcpu_record(), sku, 1).await;
    let metric = registered(VCPU_METRIC);
    let ru = body_json(
        get(
            f.app(),
            f.tenant,
            &format!("{}?locale=ru", version_uri(d, 1)),
        )
        .await,
    )
    .await;
    assert_eq!(ru["locale"], "ru");
    assert_eq!(ru["display_name"], metric["display_name"]["ru"]);
    assert_eq!(ru["description"], metric["description"]["ru"]);
    assert_eq!(
        ru["profile_conditions"],
        "Vydelennye vCPU uchityvayutsya, poka VM ostanovlena."
    );
    let de = body_json(
        get(
            f.app(),
            f.tenant,
            &format!("{}?locale=de", version_uri(d, 1)),
        )
        .await,
    )
    .await;
    assert_eq!(de["locale"], "en");
    assert_eq!(de["display_name"], "Virtual CPUs");
    assert_eq!(
        de["profile_conditions"],
        "Allocated vCPUs count while a VM is stopped."
    );
    let listed = body_json(
        get(
            f.app(),
            f.tenant,
            &list_uri(sku, 1, "&availability=all&locale=ru"),
        )
        .await,
    )
    .await;
    assert_eq!(
        listed["items"][0]["display_name"],
        metric["display_name"]["ru"]
    );
}

/// The `OData` extractor's budget answers before the list reads a term (P-D-272): a `$filter`
/// over 8 KiB is `FILTER_TOO_LONG` and one over 2000 nodes `FILTER_TOO_COMPLEX`, though the list
/// would refuse either's `or`, because the list reads its terms from the extractor's one parse.
#[tokio::test]
async fn the_odata_budget_answers_before_the_terms_are_read() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let head = format!("sku_id eq {sku} and sku_version eq '1' and (x eq 1");
    for (filter, reason) in [
        (
            format!("{head}{})", " or x eq 1".repeat(900)),
            "FILTER_TOO_LONG",
        ),
        (
            format!("{head}{})", " or x eq 1".repeat(700)),
            "FILTER_TOO_COMPLEX",
        ),
    ] {
        let uri = format!("{DEFINITIONS}?$filter={filter}&availability=all")
            .replace(' ', "%20")
            .replace('\'', "%27");
        let response = get(f.app(), f.tenant, &uri).await;
        let status = response.status();
        let body = body_json(response).await.to_string();
        assert_eq!(status, 400, "{reason}: {body}");
        assert!(body.contains(reason), "{reason}: {body}");
        assert!(!body.contains("QUOTA_FILTER_INVALID"), "{reason}: {body}");
    }
}

/// A `$filter` that does not parse is the `OData` binding's 400 `INVALID_FILTER` on
/// `cf.core.odata.query.v1~` (P-D-272), never `QUOTA_FILTER_INVALID`: the list reads its terms
/// from the extractor's one parse.
#[tokio::test]
async fn an_unparsable_filter_is_the_bindings_invalid_filter() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    for filter in [
        "sku_id eq".to_owned(),
        format!("sku_id eq {sku} and sku_version eq '1' and"),
    ] {
        let uri = format!("{DEFINITIONS}?$filter={filter}&availability=all")
            .replace(' ', "%20")
            .replace('\'', "%27");
        let response = get(f.app(), f.tenant, &uri).await;
        let status = response.status();
        let body = body_json(response).await;
        assert_eq!(status, 400, "{filter}: {body}");
        assert_eq!(problem_code(&body), "INVALID_FILTER", "{filter}: {body}");
        assert!(
            body["context"]["resource_type"]
                .as_str()
                .is_some_and(|t| t.ends_with("cf.core.odata.query.v1~")),
            "{filter}: {body}"
        );
        assert!(
            !body.to_string().contains("QUOTA_FILTER_INVALID"),
            "{filter}: {body}"
        );
    }
}

#[tokio::test]
async fn the_filter_takes_only_eq_on_its_five_fields() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let (d, _) = f.approved(vcpu_record(), sku, 1).await;
    let fixture = &f;
    let refused = |filter: String| {
        let uri = format!("{DEFINITIONS}?$filter={filter}&availability=all")
            .replace(' ', "%20")
            .replace('\'', "%27");
        let f = fixture;
        async move {
            let response = get(f.app(), f.tenant, &uri).await;
            assert_eq!(response.status(), 400, "{uri}");
            let body = body_json(response).await;
            assert_eq!(problem_code(&body), "QUOTA_FILTER_INVALID", "{uri}: {body}");
            assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
            body.to_string()
        }
    };
    refused(format!("sku_id eq {sku} or sku_version eq '1'")).await;
    let named = refused(format!(
        "sku_id eq {sku} and sku_version eq '1' and metric_ref eq '{VCPU_METRIC}'"
    ))
    .await;
    assert!(
        named.contains("metric_ref"),
        "the refusal names the term: {named}"
    );
    refused(format!("sku_id eq {sku} and sku_version gt '1'")).await;
    let missing = refused(format!("sku_id eq {sku}")).await;
    assert!(missing.contains("sku_version"), "{missing}");
    refused(format!("sku_id eq {sku} and sku_version eq '0'")).await;
    refused(format!(
        "sku_id eq {sku} and sku_version eq '1' and sku_id eq {sku}"
    ))
    .await;
    let vm = body_json(
        get(
            f.app(),
            f.tenant,
            &list_uri(
                sku,
                1,
                &format!(" and resource_type_ref eq '{VM_TYPE}'&availability=all"),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(vm["items"][0]["definition_id"], json!(d));
    let other = body_json(
        get(
            f.app(),
            f.tenant,
            &list_uri(
                sku,
                1,
                " and resource_type_ref eq 'gts.cf.core.compute.volume.v1~'&availability=all",
            ),
        )
        .await,
    )
    .await;
    assert!(items(&other).is_empty(), "another resource type");
    let profile = vm["items"][0]["profile_ref"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let by_profile = body_json(
        get(
            f.app(),
            f.tenant,
            &list_uri(
                sku,
                1,
                &format!(
                    " and profile_id eq {profile} and subject_type_ref eq \
                     'gts.cf.core.compute.tenant.v1~'&availability=all"
                ),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(items(&by_profile).len(), 1, "{by_profile}");
}

#[tokio::test]
async fn an_exact_version_reads_as_stored_after_a_newer_version() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let tenant = f.tenant;
    let user = crate::test_support::tenant_user(tenant);
    let target = f.q.fresh_target(tenant).await;
    let first = f.q.manual_candidate_on(&user, target, vcpu_record()).await;
    let first =
        f.q.submit(tenant, &first, &vcpu_record(), sku, 1, None)
            .await;
    let v1 = body_json(approve(&f.q, tenant, &first).await).await["definition"].clone();
    let d = Uuid::parse_str(v1["definition_id"].as_str().unwrap()).unwrap();
    let mut changed = vcpu_record();
    changed["id"] = json!(Uuid::new_v4().to_string());
    changed["limits"]["max"] = json!("2048");
    let second =
        f.q.manual_candidate_on(&user, target, changed.clone())
            .await;
    let second = f.q.submit(tenant, &second, &changed, sku, 1, Some(d)).await;
    let v2 = body_json(approve(&f.q, tenant, &second).await).await["definition"].clone();
    assert_eq!(v2["version"], "2");
    let read = body_json(get(f.app(), tenant, &version_uri(d, 1)).await).await;
    assert_eq!(read, v1, "version 1 reads as its approval answered it");
    assert_eq!(read["allowed_units"][0]["max"], "1024");
    let unknown = get(f.app(), tenant, &version_uri(d, 3)).await;
    assert_eq!(unknown.status(), 404);
    let (status, code, _) =
        refusal(get(f.app(), tenant, &format!("{DEFINITIONS}/{d}/versions/01")).await).await;
    assert_eq!((status, code.as_str()), (400, "VALIDATION"));
}

#[tokio::test]
async fn another_tenants_definition_is_not_found() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let (d, _) = q.approved(tenant, vcpu_record(), sku, 1).await;
    assert_eq!(get(&q.app, tenant, &version_uri(d, 1)).await.status(), 200);
    let foreign = get(&q.app, other, &version_uri(d, 1)).await;
    assert_eq!(foreign.status(), 404);
    let page = body_json(get(&q.app, other, &list_uri(sku, 1, "&availability=all")).await).await;
    assert!(items(&page).is_empty(), "another tenant lists none");
}

/// A PDP that fails to evaluate: an unreachable PDP.
struct FailingResolver;

#[async_trait]
impl authz_resolver_sdk::AuthZResolverApi for FailingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::internal("pdp unreachable".to_owned()).create())
    }
}

#[tokio::test]
async fn a_pdp_outage_is_503_not_an_empty_page() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let (d, _) = f.approved(vcpu_record(), sku, 1).await;
    let unreachable = super::router(f.q.state.clone(), &toolkit::api::OpenApiRegistryImpl::new())
        .layer(axum::Extension(authz_resolver_sdk::PolicyEnforcer::new(
            Arc::new(FailingResolver),
        )));
    for uri in [list_uri(sku, 1, "&availability=all"), version_uri(d, 1)] {
        assert_eq!(
            get(&unreachable, f.tenant, &uri).await.status(),
            503,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn a_staged_or_submitted_candidate_is_not_listed() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    f.q.manual_candidate(f.tenant, vcpu_record_numbered(1))
        .await;
    let staged =
        f.q.manual_candidate(f.tenant, vcpu_record_numbered(2))
            .await;
    f.q.submit(f.tenant, &staged, &vcpu_record_numbered(2), sku, 1, None)
        .await;
    let all = body_json(get(f.app(), f.tenant, &list_uri(sku, 1, "&availability=all")).await).await;
    assert!(items(&all).is_empty(), "{all}");
}

/// Every member name of `value`, at any depth.
fn names(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                found.push(key.clone());
                names(inner, found);
            }
        }
        Value::Array(list) => list.iter().for_each(|inner| names(inner, found)),
        _ => {}
    }
}

#[tokio::test]
async fn the_definition_carries_bounds_and_no_amount() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let (d, _) = f.approved(vcpu_record(), sku, 1).await;
    let read = body_json(get(f.app(), f.tenant, &version_uri(d, 1)).await).await;
    let unit = &read["allowed_units"][0];
    assert_eq!(
        (&unit["min"], &unit["step"], &unit["max"]),
        (&json!("0"), &json!("1"), &json!("1024"))
    );
    let mut found = Vec::new();
    names(&read, &mut found);
    for forbidden in ["amount", "quantity", "cap", "paid"] {
        assert!(
            !found.iter().any(|n| n == forbidden),
            "{forbidden}: {found:?}"
        );
    }
}

/// A stored definition whose content does not decode is a 500 naming no row, never "not found"
/// and never skipped from a page; the row's table, tenant and id stay in the log.
#[tokio::test]
async fn a_stored_definition_that_does_not_decode_is_500() {
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let (d, _) = f.approved(vcpu_record(), sku, 1).await;
    let raw = Database::connect(&*f.q.dsn).await.unwrap();
    raw.execute_unprepared("DROP TRIGGER products_quota_definition_version_no_update")
        .await
        .unwrap();
    // The database holds this one definition version.
    raw.execute_unprepared(
        "UPDATE products_quota_definition_version SET content_json = '{\"metric\": 1}'",
    )
    .await
    .unwrap();
    raw.close().await.unwrap();
    for uri in [version_uri(d, 1), list_uri(sku, 1, "&availability=all")] {
        let (logs, guard) = LogCapture::install();
        let response = get(f.app(), f.tenant, &uri).await;
        drop(guard);
        assert_eq!(response.status(), 500, "{uri}");
        let body = body_json(response).await;
        assert!(
            body.to_string()
                .contains("a stored quota row does not read"),
            "{uri}: {body}"
        );
        assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
        // The row's table, tenant and id are in the log, not on the wire.
        assert!(!body.to_string().contains(&d.to_string()), "{uri}: {body}");
        let lines = logs.lines();
        assert!(
            lines.iter().any(|line| {
                line.starts_with("a stored quota row does not read")
                    && line.contains("products_quota_definition_version")
                    && line.contains(&f.tenant.to_string())
                    && line.contains(&d.to_string())
            }),
            "{uri}: {lines:?}"
        );
    }
}

/// A grant on some definitions reads only those (P-D-273): the definition reads keep the
/// grant's row constraints, through REST and through the SDK client alike, while a candidate
/// read keeps only its tenant.
#[tokio::test]
async fn a_grant_on_some_definitions_reads_only_those() {
    use crate::infra::quota::catalog_client::LocalQuotaCatalogClient;
    use bss_products_sdk::ProductsQuotaCatalogClientV1 as _;
    use bss_products_sdk::quota::catalog::{QuotaAvailabilityFilter, QuotaDefinitionListRequest};
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let (d1, v1) = f.approved(vcpu_record_numbered(1), sku, 1).await;
    let (d2, v2) = f.approved(vcpu_record_numbered(2), sku, 1).await;
    // The grant is now `owner_tenant_id = T AND id IN (d1)`.
    f.q.pdp.narrow_to(&[d1]);
    let page =
        body_json(get(f.app(), f.tenant, &list_uri(sku, 1, "&availability=all")).await).await;
    let listed: Vec<&Value> = items(&page).iter().map(|i| &i["definition_id"]).collect();
    assert_eq!(listed, [&json!(d1)], "{page}");
    assert_eq!(
        get(f.app(), f.tenant, &version_uri(d1, 1)).await.status(),
        200
    );
    assert_eq!(
        get(f.app(), f.tenant, &version_uri(d2, 1)).await.status(),
        404
    );
    let sdk = LocalQuotaCatalogClient::new(
        f.q.state.clone(),
        Arc::new(authz_resolver_sdk::PolicyEnforcer::new(f.q.pdp.clone())),
    );
    let caller = crate::test_support::tenant_user(f.tenant);
    let page = sdk
        .list_quota_definitions(
            &caller,
            QuotaDefinitionListRequest {
                sku_id: sku,
                sku_version: QuotaVersion::new(1).unwrap(),
                subject_type_ref: None,
                resource_type_ref: None,
                profile_id: None,
                locale: None,
                availability: QuotaAvailabilityFilter::All,
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let listed: Vec<Uuid> = page.items.iter().map(|d| d.definition_id).collect();
    assert_eq!(listed, [d1]);
    assert!(
        sdk.get_quota_definition(&caller, d1, v1, None)
            .await
            .is_ok()
    );
    let refused = sdk
        .get_quota_definition(&caller, d2, v2, None)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, CanonicalError::NotFound { .. }),
        "{refused:?}"
    );
    // A candidate is not a definition: its list keeps the tenant only.
    let candidates = body_json(get(f.app(), f.tenant, super::CANDIDATES).await).await;
    assert_eq!(items(&candidates).len(), 2, "{candidates}");
}

/// Both reads ask `read` on `quota_definition` for the caller's tenant only.
#[tokio::test]
async fn every_read_asks_read_for_the_callers_tenant_only() {
    use authz_resolver_sdk::models::TenantMode;
    let f = CatalogFixture::new().await;
    let sku = f.sku_with_versions(1).await;
    let (d, _) = f.approved(vcpu_record(), sku, 1).await;
    let before = f.q.pdp.asked().len();
    assert_eq!(
        get(f.app(), f.tenant, &list_uri(sku, 1, "")).await.status(),
        200
    );
    assert_eq!(
        get(f.app(), f.tenant, &version_uri(d, 1)).await.status(),
        200
    );
    let read = (
        labels::QUOTA_DEFINITION.to_owned(),
        "read".to_owned(),
        Some(TenantMode::RootOnly),
    );
    assert_eq!(f.q.pdp.asked()[before..], [read.clone(), read]);
}
